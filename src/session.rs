//! The connection to one server and everything that goes over it: dialer (direct or SOCKS5), the
//! transport halves, login (NTLM, or Kerberos from the ticket cache with the KDC relay), the tree
//! map, the `cd` check, the listing, the share list, the cache,
//! reconnect, and the mapping of the crate's errors to ours. All use of `smb2` is in this file.
//! docs/architecture.md describes the design.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use smb2::KerberosCredentials;
use smb2::auth::kerberos::ccache::{CCache, parse_ccache};
use smb2::auth::kerberos::messages::parse_krb_error;
use smb2::client::Connection;
use smb2::msg::close::CloseRequest;
use smb2::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use smb2::msg::query_directory::{
    FileInformationClass, QueryDirectoryFlags, QueryDirectoryRequest,
};
use smb2::msg::read::{ReadRequest, ReadResponse};
use smb2::msg::tree_connect::{TreeConnectRequest, TreeConnectRequestFlags, TreeConnectResponse};
use smb2::msg::tree_disconnect::TreeDisconnectRequest;
use smb2::msg::write::WriteRequest;
use smb2::pack::{ReadCursor, Unpack};
use smb2::transport::{ReceiveProgress, TransportReceive, TransportSend};
use smb2::types::flags::FileAccessMask;
use smb2::types::status::NtStatus;
use smb2::types::{Command, CreditCharge, FileId, OplockLevel, TreeId};
use smb2::{CompoundOp, FileWriter, Frame, RenameOptions, Tree};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::walk;

// ---------------------------------------------------------------------------------------------
// Errors

/// What went wrong, in the terms a caller acts on. `name()` is the `kind` of the JSON error object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NotFound,
    AccessDenied,
    NotADirectory,
    IsADirectory,
    AlreadyExists,
    /// `rmdir` of a directory that has entries.
    NotEmpty,
    DiskFull,
    InvalidName,
    Auth,
    Connect,
    /// `silent`: the server stopped answering (as opposed to the connection being closed).
    ConnectionLost {
        silent: bool,
    },
    Interrupted,
    LocalIo,
    Usage,
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::NotFound => "not_found",
            Kind::AccessDenied => "access_denied",
            Kind::NotADirectory => "not_a_directory",
            Kind::IsADirectory => "is_a_directory",
            Kind::AlreadyExists => "already_exists",
            Kind::NotEmpty => "not_empty",
            Kind::DiskFull => "disk_full",
            Kind::InvalidName => "invalid_name",
            Kind::Auth => "auth",
            Kind::Connect => "connect",
            Kind::ConnectionLost { .. } => "connection_lost",
            Kind::Interrupted => "interrupted",
            Kind::LocalIo => "local_io",
            Kind::Usage => "usage",
            Kind::Other => "other",
        }
    }
}

/// Every kind name, in the order of `Kind`; the manual lists them from here.
pub const KIND_NAMES: [&str; 15] = [
    "not_found",
    "access_denied",
    "not_a_directory",
    "is_a_directory",
    "already_exists",
    "not_empty",
    "disk_full",
    "invalid_name",
    "auth",
    "connect",
    "connection_lost",
    "interrupted",
    "local_io",
    "usage",
    "other",
];

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: Kind,
    /// A whole line for a person: `<what>: <reason>`.
    pub message: String,
    /// The item, for an error inside a recursive transfer.
    pub path: Option<String>,
    /// Temporary files that could not be removed.
    pub left: Vec<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
            path: None,
            left: Vec::new(),
        }
    }
    pub fn other(message: impl Into<String>) -> Error {
        Error::new(Kind::Other, message)
    }
    pub fn at(mut self, path: impl Into<String>) -> Error {
        self.path = Some(path.into());
        self
    }
    pub fn usage(message: impl Into<String>) -> Error {
        Error::new(Kind::Usage, message)
    }
    pub fn exit_code(&self, at_startup: bool) -> u8 {
        match self.kind {
            Kind::Usage => 2,
            Kind::Interrupted => 130,
            _ if at_startup => 3,
            _ => 1,
        }
    }
    pub fn is_lost(&self) -> bool {
        matches!(self.kind, Kind::ConnectionLost { .. })
    }
}

/// The crate's error, for `what` (a path, a share, a step), in our terms.
pub fn classify(e: smb2::Error, what: &str) -> Error {
    use smb2::ErrorKind as K;
    let simple = |kind, reason: &str| Error::new(kind, format!("{what}: {reason}"));
    match e.status() {
        Some(NtStatus::DIRECTORY_NOT_EMPTY) => {
            return simple(Kind::NotEmpty, "directory not empty");
        }
        Some(NtStatus::CANNOT_DELETE) => {
            return simple(Kind::Other, "cannot be deleted (read-only?)");
        }
        _ => {}
    }
    match e.kind() {
        K::SharingViolation => simple(Kind::Other, "in use by another client"),
        K::AccessDenied => simple(Kind::AccessDenied, "access denied"),
        K::NotFound => simple(Kind::NotFound, "not found"),
        K::NotADirectory => simple(Kind::NotADirectory, "not a directory"),
        K::IsADirectory => simple(Kind::IsADirectory, "is a directory"),
        K::AlreadyExists => simple(Kind::AlreadyExists, "already exists"),
        K::DiskFull => simple(Kind::DiskFull, "disk full"),
        K::InvalidName => simple(Kind::InvalidName, "invalid name"),
        K::GuestSessionRefused => {
            let user = match &e {
                smb2::Error::GuestSessionRefused { username, .. } => username.clone(),
                _ => String::new(),
            };
            Error::new(
                Kind::Auth,
                format!(
                    "the server answered the login of '{user}' with a guest session (wrong user \
                     name or password, on a server that maps unknown users to guest). To log in \
                     as guest on purpose: -u Guest --no-pass"
                ),
            )
        }
        K::AuthRequired | K::SigningRequired => simple(Kind::Auth, &format!("login refused ({e})")),
        K::ConnectionLost => {
            let message = match &e {
                smb2::Error::ServerUnresponsive { silent_for } => format!(
                    "connection lost: no answer from the server for {} s",
                    silent_for.as_secs()
                ),
                _ => format!("connection lost: {e}"),
            };
            let silent = matches!(e, smb2::Error::ServerUnresponsive { .. });
            Error::new(Kind::ConnectionLost { silent }, message)
        }
        K::TimedOut => Error::new(
            Kind::ConnectionLost { silent: true },
            "connection lost: no complete reply within the response deadline; on a slow link \
             raise --timeout",
        ),
        K::SessionExpired => Error::new(
            Kind::ConnectionLost { silent: false },
            "connection lost: the server ended the session (with -k: the ticket's lifetime is over)",
        ),
        K::Io => Error::new(
            Kind::ConnectionLost { silent: false },
            format!("connection lost: {e}"),
        ),
        _ => simple(Kind::Other, &e.to_string()),
    }
}

/// A frame whose status is not success, as the crate's own error, so it is classified the same way.
fn status_error(f: &Frame) -> smb2::Error {
    smb2::Error::Protocol {
        status: f.header.status,
        command: f.header.command,
    }
}

/// The frame if the crate and the server both said yes, the classified error otherwise.
fn ok_frame(r: smb2::Result<Frame>, what: &str) -> Result<Frame> {
    match r {
        Ok(f) if f.header.status == NtStatus::SUCCESS => Ok(f),
        Ok(f) => Err(classify(status_error(&f), what)),
        Err(e) => Err(classify(e, what)),
    }
}

// ---------------------------------------------------------------------------------------------
// Target, proxy, dialer, transport

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub auth: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub domain: String,
    pub proxy: Option<Proxy>,
    /// The crate's response deadline (`--timeout`).
    pub timeout: Duration,
    /// `-k`. `user` and `domain` then hold what `-u` and `-d` gave (empty: take the cache's) and,
    /// after the first login, the cache's principal.
    pub kerberos: bool,
}

/// `%40` → `@`; None for a bad escape or bytes that are not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s
                .get(i + 1..i + 3)
                .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The one message for every refused `--proxy`: it never repeats the URL, which may hold a
/// password, and no rule for cutting it out holds for every malformed URL.
pub const PROXY_USAGE: &str = "--proxy: expected socks5://[user:pass@]host:port (SOCKS5 only; \
percent-encode special characters in user and password, put an IPv6 host in brackets)";

/// `socks5://[user:pass@]host:port`, parsed by hand; `socks5h://` means the same (the proxy
/// always resolves the server's name); one trailing `/` is dropped. User and password are
/// percent-decoded (a password with `@`, `:` or `/` is written `%40`, `%3A`, `%2F`) and each is 1
/// to 255 bytes (RFC 1929); an IPv6 host is in brackets.
pub fn parse_proxy(url: &str) -> Result<Proxy> {
    let bad = || Error::usage(PROXY_USAGE);
    let rest = url
        .strip_prefix("socks5://")
        .or_else(|| url.strip_prefix("socks5h://"))
        .ok_or_else(bad)?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => {
            let (u, p) = a.split_once(':').ok_or_else(bad)?;
            let (u, p) = (
                percent_decode(u).ok_or_else(bad)?,
                percent_decode(p).ok_or_else(bad)?,
            );
            if !(1..=255).contains(&u.len()) || !(1..=255).contains(&p.len()) {
                return Err(bad());
            }
            (Some((u, p)), h)
        }
        None => (None, rest),
    };
    let (host, port) = match hostport.strip_prefix('[') {
        Some(v6) => v6
            .split_once("]:")
            .filter(|(h, _)| h.contains(':'))
            .ok_or_else(bad)?,
        None => hostport.rsplit_once(':').ok_or_else(bad)?,
    };
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let port: u16 = port.parse().map_err(|_| bad())?;
    let bad_host =
        host.is_empty() || host.contains('/') || (!hostport.starts_with('[') && host.contains(':'));
    if bad_host || port == 0 {
        return Err(bad());
    }
    Ok(Proxy {
        host: host.to_string(),
        port,
        auth,
    })
}

struct Tx(Mutex<OwnedWriteHalf>);

/// The receive half. It reports its progress inside a frame to the crate: without that, a large
/// reply arriving slower than the response deadline would count as silence (docs/architecture.md).
struct Rx {
    half: Mutex<OwnedReadHalf>,
    progress: Arc<ReceiveProgress>,
}

#[async_trait]
impl TransportSend for Tx {
    async fn send(&self, data: &[u8]) -> smb2::Result<()> {
        if data.len() >= 1 << 24 {
            return Err(smb2::Error::invalid_data(
                "frame too large for the transport",
            ));
        }
        let mut frame = Vec::with_capacity(data.len() + 4);
        frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
        frame.extend_from_slice(data);
        self.0.lock().await.write_all(&frame).await.map_err(io_err)
    }
}

fn io_err(e: std::io::Error) -> smb2::Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        smb2::Error::Disconnected
    } else {
        smb2::Error::Io(e)
    }
}

/// Ends the frame in progress on every way out of `receive`.
struct FrameGuard<'a>(&'a ReceiveProgress);
impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        self.0.end_frame();
    }
}

#[async_trait]
impl TransportReceive for Rx {
    async fn receive(&self) -> smb2::Result<Vec<u8>> {
        let mut r = self.half.lock().await;
        let mut h = [0u8; 4];
        r.read_exact(&mut h).await.map_err(io_err)?;
        if h[0] != 0 {
            return Err(smb2::Error::invalid_data("bad transport frame header"));
        }
        let len = u32::from_be_bytes(h) as usize;
        self.progress.begin_frame(len);
        let _guard = FrameGuard(&self.progress);
        let mut buf = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            let n = r.read(&mut buf[filled..]).await.map_err(io_err)?;
            if n == 0 {
                return Err(smb2::Error::Disconnected);
            }
            filled += n;
            self.progress.record(n);
        }
        Ok(buf)
    }

    fn receive_progress(&self) -> Option<Arc<ReceiveProgress>> {
        Some(Arc::clone(&self.progress))
    }
}

fn transport(tcp: TcpStream) -> (Tx, Rx) {
    let (r, w) = tcp.into_split();
    (
        Tx(Mutex::new(w)),
        Rx {
            half: Mutex::new(r),
            progress: Arc::new(ReceiveProgress::new()),
        },
    )
}

/// How long a dial may take, direct or through the proxy: the TCP connect and, with a proxy,
/// the handshake and the proxy's own connect to the server. `--timeout` does not change it.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the crate lets a server stay silent before it fails a request (3 keepalive probes).
const SILENCE: Duration = Duration::from_secs(15);

/// `host:port`, an IPv6 address in brackets.
pub fn hostport(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `10 s`, or `200 ms` below a second (the unit tests' limits).
fn span(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// A failed SOCKS5 step in our words; always `Kind::Connect`, never a credential.
fn proxy_error(p: &Proxy, target: &str, e: &tokio_socks::Error) -> Error {
    use tokio_socks::Error as S;
    let proxy = hostport(&p.host, p.port);
    let cannot = |why: &str| format!("cannot reach {target}: {why}");
    let what = match e {
        S::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            "closed the connection during the SOCKS5 handshake (is it a SOCKS5 proxy?)".to_string()
        }
        S::NoAcceptableAuthMethods if p.auth.is_none() => {
            format!("wants a user name and password: --proxy socks5://user:pass@{proxy}")
        }
        S::NoAcceptableAuthMethods => {
            "accepts neither a session without login nor a user name and password".to_string()
        }
        S::PasswordAuthFailure(_) => "user name or password refused".to_string(),
        S::InvalidResponseVersion
        | S::UnknownAuthMethod
        | S::InvalidReservedByte
        | S::UnknownAddressType => "not a SOCKS5 answer (is it a SOCKS5 proxy?)".to_string(),
        S::HostUnreachable => cannot("host unreachable"),
        S::NetworkUnreachable => cannot("network unreachable"),
        S::ConnectionRefused => cannot("connection refused"),
        S::GeneralSocksServerFailure => cannot(
            "the proxy reports a general failure (some proxies answer an unknown host name this \
             way)",
        ),
        S::TtlExpired => cannot("timed out on the way"),
        S::ConnectionNotAllowedByRuleset => cannot("not allowed by the proxy's rules"),
        S::CommandNotSupported => cannot("the proxy does not support CONNECT"),
        S::AddressTypeNotSupported => cannot("the proxy does not support this kind of address"),
        e => e.to_string(),
    };
    Error::new(Kind::Connect, format!("proxy {proxy}: {what}"))
}

/// The TCP stream to the server, direct or through the SOCKS5 proxy, within `limit`,
/// `TCP_NODELAY` set on the socket we own (the proxy's, when there is one). With a proxy the
/// server is never dialled directly, whatever happens: there is no fallback. A timeout says
/// whether the TCP connection to the proxy stood.
async fn open_tcp(t: &Target, limit: Duration) -> Result<TcpStream> {
    let target = hostport(&t.host, t.port);
    let within = span(limit);
    let Some(p) = &t.proxy else {
        let connect = async {
            let s = TcpStream::connect((t.host.as_str(), t.port)).await?;
            s.set_nodelay(true)?;
            Ok::<_, std::io::Error>(s)
        };
        let fail = |what: String| Error::new(Kind::Connect, format!("connect to {target}: {what}"));
        return match tokio::time::timeout(limit, connect).await {
            Ok(r) => r.map_err(|e| fail(e.to_string())),
            Err(_) => Err(fail(format!("no answer within {within}"))),
        };
    };
    let proxy = hostport(&p.host, p.port);
    let mut stood = false;
    let work = async {
        let fail = |e: std::io::Error| Error::new(Kind::Connect, format!("proxy {proxy}: {e}"));
        let s = TcpStream::connect((p.host.as_str(), p.port))
            .await
            .map_err(fail)?;
        s.set_nodelay(true).map_err(fail)?;
        stood = true;
        let dest = (t.host.as_str(), t.port);
        let socks = match &p.auth {
            None => tokio_socks::tcp::Socks5Stream::connect_with_socket(s, dest).await,
            Some((u, pw)) => {
                tokio_socks::tcp::Socks5Stream::connect_with_password_and_socket(s, dest, u, pw)
                    .await
            }
        };
        socks
            .map(|s| s.into_inner())
            .map_err(|e| proxy_error(p, &target, &e))
    };
    let r = tokio::time::timeout(limit, work).await;
    match r {
        Ok(r) => r,
        Err(_) if stood => Err(Error::new(
            Kind::Connect,
            format!(
                "proxy {proxy}: connected to the proxy, but the SOCKS5 handshake did not finish \
                 within {within} (the proxy is silent, is not a SOCKS5 proxy, or cannot reach \
                 {target})"
            ),
        )),
        Err(_) => Err(Error::new(
            Kind::Connect,
            format!("proxy {proxy}: no answer within {within}"),
        )),
    }
}

/// A connected stream to the server inside a `Connection`, within `DIAL_TIMEOUT`.
async fn dial(t: &Target) -> Result<Connection> {
    let (tx, rx) = transport(open_tcp(t, DIAL_TIMEOUT).await?);
    Ok(Connection::from_transport(
        Box::new(tx),
        Box::new(rx),
        t.host.clone(),
    ))
}

/// Dial, negotiate, log in (NTLM, or Kerberos with `t.kerberos`). Used at start-up and for every
/// reconnect. With Kerberos, also who the cache says we are.
async fn login(t: &Target) -> Result<(Connection, Option<(String, String)>)> {
    // Before the dial: a cache that cannot be used costs no connection.
    let ticket = if t.kerberos {
        Some(read_ticket(t)?)
    } else {
        None
    };
    let mut conn = dial(t).await?;
    conn.set_response_timeout(Some(t.timeout));
    // A frame being written when the link goes silent blocks until the send timeout, also after
    // the receive side has declared the server silent: the same bound as silence, max(--timeout,
    // 15 s), so an upload fails when any other command would (the crate's own default is 20 s).
    conn.set_send_timeout(Some(t.timeout.max(SILENCE)));
    let target = format!("{}:{}", t.host, t.port);
    conn.negotiate()
        .await
        .map_err(|e| classify(e, &format!("negotiate with {target}")))?;
    match &ticket {
        None => {
            let who = display_user(t);
            smb2::Session::setup(&mut conn, &t.user, &t.password, &t.domain)
                .await
                .map_err(|e| classify(e, &format!("login as {who}")))?;
        }
        Some(k) => kerberos_setup(&mut conn, t, k).await?,
    }
    Ok((conn, ticket.map(|k| (k.user, k.realm))))
}

/// `logged in as <user> on <host>:<port>[ through proxy <host>:<port>] (<dialect>)`: the proxy's
/// address, never its user or password.
fn banner(t: &Target, dialect: &str) -> String {
    let via = t
        .proxy
        .as_ref()
        .map(|p| format!(" through proxy {}", hostport(&p.host, p.port)))
        .unwrap_or_default();
    let how = if t.kerberos { " (Kerberos)" } else { "" };
    format!(
        "logged in as {}{how} on {}{via} ({dialect})",
        display_user(t),
        hostport(&t.host, t.port)
    )
}

fn display_user(t: &Target) -> String {
    match (t.user.is_empty(), t.domain.is_empty()) {
        (true, _) => "anonymous".to_string(),
        (false, true) => format!("'{}'", t.user),
        (false, false) => format!("'{}\\{}'", t.domain, t.user),
    }
}

// ---------------------------------------------------------------------------------------------
// Kerberos: the ticket cache, the KDC's address, the relay, the errors (docs/commands.md,
// "Kerberos"). Every message of a Kerberos failure starts `Kerberos: `.

fn kerberos_error(kind: Kind, message: impl std::fmt::Display) -> Error {
    Error::new(kind, format!("Kerberos: {message}"))
}

/// The cache file, and whether it is the default one: `var` is KRB5CCNAME, taken as bytes (a path
/// need not be UTF-8). `FILE:<path>` or a path; None or empty: `/tmp/krb5cc_<uid>`. Another cache
/// type is an error that names it.
fn ccache_path(var: Option<&OsStr>, uid: u32) -> Result<(PathBuf, bool)> {
    let v = var.map_or(&[][..], OsStr::as_bytes);
    if v.is_empty() {
        return Ok((PathBuf::from(format!("/tmp/krb5cc_{uid}")), true));
    }
    if let Some(p) = v.strip_prefix(b"FILE:") {
        return Ok((PathBuf::from(OsStr::from_bytes(p)), false));
    }
    match v.iter().position(|&b| b == b':') {
        Some(i) if !v.starts_with(b"/") && !v.starts_with(b".") => Err(kerberos_error(
            Kind::Auth,
            format!(
                "KRB5CCNAME names a ticket cache of type '{}'; rsmb reads a cache file only. Put \
                 the tickets into a file and name it: kinit -c /tmp/krb5cc_rsmb <user>; \
                 KRB5CCNAME=/tmp/krb5cc_rsmb",
                String::from_utf8_lossy(&v[..i])
            ),
        )),
        _ => Ok((PathBuf::from(OsStr::from_bytes(v)), false)),
    }
}

/// Opens `path` to read it: without waiting for a writer (a FIFO would block `open` for good, and
/// a signal cannot end that), and only a regular file is let through.
fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

/// What follows a message about the default file: kinit may well have written elsewhere.
fn default_hint(path: &str, default: bool) -> String {
    if default {
        format!(
            " (KRB5CCNAME is not set, so the default file {path} was used; if kinit writes \
             somewhere else, for example to a KCM or KEYRING cache, set KRB5CCNAME to a cache \
             file: kinit -c <file>)"
        )
    } else {
        String::new()
    }
}

/// A cache of another user would log the session in as that user's principal (the default file
/// lies in world-writable /tmp).
fn cache_owner(file_uid: u32, uid: u32, path: &str) -> Result<()> {
    if file_uid == uid {
        return Ok(());
    }
    Err(kerberos_error(
        Kind::Auth,
        format!(
            "ticket cache {path} belongs to uid {file_uid}, not to the user rsmb runs as (uid \
             {uid}); rsmb uses a cache of its own user only"
        ),
    ))
}

/// What one Kerberos login takes from the cache.
struct Ticket {
    /// The parsed cache without its expired entries.
    cache: CCache,
    /// The default principal, as the cache spells it.
    user: String,
    realm: String,
    /// The cache holds a ticket for `cifs/<host>`: no KDC is asked.
    service: bool,
}

/// Reads the cache for a login at `t.host`: KRB5CCNAME, the file (which must be ours),
/// `parse_ccache`, then `pick_ticket` with the clock. No network.
fn read_ticket(t: &Target) -> Result<Ticket> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let (path, default) = ccache_path(std::env::var_os("KRB5CCNAME").as_deref(), uid)?;
    let shown = path.display().to_string();
    let io = |e: std::io::Error| {
        kerberos_error(
            Kind::Auth,
            format!("ticket cache {shown}: {e}{}", default_hint(&shown, default)),
        )
    };
    // The owner is checked on the opened file, so the check and the read see the same file.
    let mut file = open_regular(&path).map_err(io)?;
    cache_owner(file.metadata().map_err(io)?.uid(), uid, &shown)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).map_err(io)?;
    // ponytail: a principal that claims an absurd number of parts makes the crate's parser abort
    // the process (exit 134, docs/known-limitations.md); a complete guard is a second parser.
    let cache = parse_ccache(&data).map_err(|e| {
        let text = e.to_string();
        let text = text.strip_prefix("Invalid data: ").unwrap_or(&text);
        kerberos_error(
            Kind::Auth,
            format!("{shown} is not a ticket cache rsmb can read ({text})"),
        )
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    pick_ticket(cache, t, now, &shown, default)
}

/// The pure part of `read_ticket`: the principal and its match with `t.user` and `t.domain`,
/// expiry (this host's clock, no allowance), which ticket. `default`: the path is the default
/// file (KRB5CCNAME was not set).
fn pick_ticket(
    mut cache: CCache,
    t: &Target,
    now: u64,
    path: &str,
    default: bool,
) -> Result<Ticket> {
    let auth = |m: String| kerberos_error(Kind::Auth, m);
    let p = &cache.default_principal;
    let realm = p.realm.clone();
    let [user] = p.components.as_slice() else {
        return Err(auth(format!(
            "the ticket cache {path} belongs to '{}@{realm}'; a principal with more than one name \
             part is not supported",
            p.components.join("/")
        )));
    };
    let user = user.clone();
    let user_differs = !t.user.is_empty() && !t.user.eq_ignore_ascii_case(&user);
    let realm_differs = !t.domain.is_empty() && !t.domain.eq_ignore_ascii_case(&realm);
    if user_differs || realm_differs {
        let given = match (t.user.is_empty(), t.domain.is_empty()) {
            (true, _) => format!("the realm '{}'", t.domain),
            (false, true) => format!("'{}'", t.user),
            (false, false) => format!("'{}' in '{}'", t.user, t.domain),
        };
        return Err(auth(format!(
            "the ticket cache {path} belongs to '{user}@{realm}', not to {given} (given with -u and \
             -d, or the principal this session logged in as). With -k leave -u and -d out, or give \
             the cache's own"
        )));
    }
    // The tickets this login could use, compared as the crate compares them.
    let usable = |c: &smb2::auth::kerberos::ccache::CcacheCredential| {
        let s = &c.server;
        s.realm.eq_ignore_ascii_case(&realm)
            && s.components.len() == 2
            && ((s.components[0].eq_ignore_ascii_case("cifs")
                && s.components[1].eq_ignore_ascii_case(&t.host))
                || (s.components[0] == "krbtgt" && s.components[1].eq_ignore_ascii_case(&realm)))
    };
    let expired = cache
        .credentials
        .iter()
        .filter(|c| usable(c) && u64::from(c.endtime) <= now)
        .max_by_key(|c| c.endtime)
        .map(|c| {
            let server = format!("{}@{}", c.server.components.join("/"), c.server.realm);
            (server, c.endtime)
        });
    // The crate takes the first match whether it has ended or not.
    cache.credentials.retain(|c| u64::from(c.endtime) > now);
    let service = cache.find_service_ticket("cifs", &t.host, &realm).is_some();
    if !service && cache.find_tgt(&realm).is_none() {
        let hint = default_hint(path, default);
        return Err(auth(match expired {
            Some((server, end)) => {
                let ft = (u64::from(end) + 11_644_473_600) * 10_000_000;
                let at = crate::commands::filetime(ft, true).unwrap_or_default();
                format!("the ticket for {server} in {path} expired at {at}; run kinit{hint}")
            }
            None => format!(
                "{path} holds no ticket for cifs/{}@{realm} and no ticket-granting ticket for \
                 {realm}; run kinit {user}@{realm}{hint}",
                t.host
            ),
        }));
    }
    Ok(Ticket {
        cache,
        user,
        realm,
        service,
    })
}

/// The realm's first `kdc` in the text of a krb5.conf: host and port (88 without one). Braces
/// are counted, so a `kdc` inside a nested block or another realm's block is not taken.
fn krb5_kdc(conf: &str, realm: &str) -> Option<(String, u16)> {
    let (mut in_realms, mut depth, mut wanted) = (false, 0usize, false);
    for line in conf.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if depth == 0 && line.starts_with('[') {
            in_realms = line == "[realms]";
            continue;
        }
        if !in_realms {
            continue;
        }
        if line.ends_with('{') {
            if depth == 0 {
                let name = line.trim_end_matches('{').trim_end().strip_suffix('=');
                wanted = name.map(str::trim) == Some(realm);
            }
            depth += 1;
        } else if line.starts_with('}') {
            depth = depth.saturating_sub(1);
        } else if depth == 1
            && wanted
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "kdc"
            && let Some(kdc) = kdc_value(value.trim())
        {
            return Some(kdc);
        }
    }
    None
}

/// One `kdc` value: `host`, `host:port`, `[v6]`, `[v6]:port`, a bare IPv6 address; a Heimdal
/// `tcp/` or `udp/` prefix is dropped; None for an HTTP (KKDCP) KDC or a bad port.
fn kdc_value(v: &str) -> Option<(String, u16)> {
    let v = v
        .strip_prefix("tcp/")
        .or_else(|| v.strip_prefix("udp/"))
        .unwrap_or(v);
    if v.is_empty() || v.starts_with("http") || v.starts_with("kkdcp") {
        return None;
    }
    if let Some(rest) = v.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        return match after {
            "" => Some((host.to_string(), 88)),
            _ => Some((host.to_string(), after.strip_prefix(':')?.parse().ok()?)),
        };
    }
    match v.split_once(':') {
        Some(_) if v.matches(':').count() > 1 => Some((v.to_string(), 88)),
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((v.to_string(), 88)),
    }
}

/// The files to look in: `var` is KRB5_CONFIG. None or empty: `/etc/krb5.conf`; otherwise its
/// colon-separated parts, in order.
fn krb5_files(var: Option<&OsStr>) -> Vec<PathBuf> {
    match var.map(OsStr::as_bytes) {
        None | Some([]) => vec![PathBuf::from("/etc/krb5.conf")],
        Some(v) => v
            .split(|&b| b == b':')
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(OsStr::from_bytes(p)))
            .collect(),
    }
}

/// Where to ask for a service ticket: the first of `files` that names a kdc for `realm` (a file
/// that cannot be read, or is not a regular file, counts as absent), with that file; otherwise
/// the realm's own name in lower case, port 88, and None.
fn kdc_for(files: &[PathBuf], realm: &str) -> (String, u16, Option<PathBuf>) {
    use std::io::Read;
    for f in files {
        let mut b = Vec::new();
        if open_regular(f)
            .and_then(|mut file| file.read_to_end(&mut b))
            .is_ok()
            && let Some((host, port)) = krb5_kdc(&String::from_utf8_lossy(&b), realm)
        {
            return (host, port, Some(f.clone()));
        }
    }
    (realm.to_lowercase(), 88, None)
}

/// The reason for a KRB-ERROR `code` of the KDC.
fn kdc_error(code: i32, text: Option<&str>, host: &str, user: &str, realm: &str) -> String {
    match code {
        7 => format!(
            "the realm {realm} has no service cifs/{host}: give -H the name the server has in the \
             realm"
        ),
        13 => format!(
            "refused (KDC_ERR_BADOPTION): rsmb asks for a forwardable, renewable ticket, and this \
             KDC gives one only for a ticket-granting ticket that is both. Get one with kinit -f \
             (add -r 7d if it is not renewable), or put the service ticket into the cache: kvno \
             cifs/{host}"
        ),
        32 => "the KDC says the ticket-granting ticket has expired; run kinit".to_string(),
        36 => format!(
            "the KDC does not accept the ticket-granting ticket for '{user}@{realm}' \
             (KRB_AP_ERR_BADMATCH)"
        ),
        37 => "the clocks of this host and the KDC differ too much".to_string(),
        _ => match text.filter(|t| !t.is_empty()) {
            Some(t) => format!("KDC error {code}: {t}"),
            None => format!("KDC error {code}"),
        },
    }
}

/// Where the relay sends the crate's requests, and what its messages need.
struct Kdc {
    /// The login's target with the KDC's host and port: `open_tcp` dials it as it dials the
    /// server, so through the proxy when there is one, and never around it.
    target: Target,
    /// The file that named the KDC; None: no file did, and the realm's name was tried.
    from: Option<PathBuf>,
    /// The files looked at.
    files: Vec<PathBuf>,
    /// The server's host (the SPN is `cifs/<host>`), user and realm.
    host: String,
    user: String,
    realm: String,
}

impl Kdc {
    /// What follows a message about a KDC that could not be asked: where its address came from
    /// and what to do; never a file only root can write.
    fn tail(&self) -> String {
        match &self.from {
            Some(f) => format!(
                " (the KDC is the one {} names for {})",
                f.display(),
                self.realm
            ),
            None => {
                let files: Vec<String> =
                    self.files.iter().map(|f| f.display().to_string()).collect();
                format!(
                    " (no kdc for {realm} in {files}, so the realm's name was tried. To name the \
                     KDC, write a file with the four lines \"[realms]\", \"{realm} = {{\", \"kdc = \
                     <the KDC's host>\", \"}}\" and set KRB5_CONFIG to it; or put the service \
                     ticket into the cache: kvno cifs/{host})",
                    realm = self.realm,
                    files = files.join(", "),
                    host = self.host
                )
            }
        }
    }
}

/// Writes one KDC request with its 4-byte length in one write, reads the answer. An answer that
/// cannot go back as a datagram is `InvalidData`.
async fn kdc_exchange(tcp: &mut TcpStream, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut m = Vec::with_capacity(request.len() + 4);
    m.extend_from_slice(&(request.len() as u32).to_be_bytes());
    m.extend_from_slice(request);
    tcp.write_all(&m).await?;
    let mut h = [0u8; 4];
    tcp.read_exact(&mut h).await?;
    let len = u32::from_be_bytes(h) as usize;
    if len > 65_507 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the KDC's answer of {len} bytes does not fit a datagram"),
        ));
    }
    let mut answer = vec![0u8; len];
    tcp.read_exact(&mut answer).await?;
    Ok(answer)
}

/// Carries the crate's KDC requests: each datagram that arrives on `sock` (a loopback socket of
/// ours, which the crate was given as the KDC's address; only the first sender's) goes over TCP,
/// with a 4-byte length, through `open_tcp` to the KDC, and the answer goes back as a datagram.
/// One exchange has `limit`: the dial within it with `open_tcp`'s own words, then what is left
/// of it for the request and the answer (one timer at a time, so the same silence always gives
/// the same message). Serves until dropped; returns only an error, which is the login's error.
async fn kdc_relay(sock: UdpSocket, kdc: &Kdc, limit: Duration) -> Error {
    let fail = |kind: Kind, reason: String| {
        kerberos_error(
            kind,
            format!(
                "asking the KDC {} for a ticket for cifs/{}: {reason}",
                hostport(&kdc.target.host, kdc.target.port),
                kdc.host
            ),
        )
    };
    let mut buf = vec![0u8; 65_536];
    // The crate's socket is the first to write; a datagram from any other address is ignored
    // (never sent on to the KDC with the proxy's credentials, never answered).
    let mut owner = None;
    loop {
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => return kerberos_error(Kind::LocalIo, format!("the loopback socket: {e}")),
        };
        if *owner.get_or_insert(from) != from {
            continue;
        }
        let t0 = Instant::now();
        let mut tcp = match open_tcp(&kdc.target, limit).await {
            Ok(s) => s,
            Err(e) => return fail(Kind::Connect, format!("{}{}", e.message, kdc.tail())),
        };
        let left = limit.saturating_sub(t0.elapsed());
        let answer = match tokio::time::timeout(left, kdc_exchange(&mut tcp, &buf[..n])).await {
            Ok(Ok(a)) => a,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                return fail(Kind::Auth, e.to_string());
            }
            Ok(Err(_)) => {
                let reason = "the KDC closed the connection without an answer";
                return fail(Kind::Connect, format!("{reason}{}", kdc.tail()));
            }
            Err(_) => {
                let reason = format!(
                    "connected to the KDC, but it did not answer within {}",
                    span(limit)
                );
                return fail(Kind::Connect, format!("{reason}{}", kdc.tail()));
            }
        };
        if answer.first() == Some(&0x7e)
            && let Ok(e) = parse_krb_error(&answer)
        {
            let reason = kdc_error(
                e.error_code,
                e.e_text.as_deref(),
                &kdc.host,
                &kdc.user,
                &kdc.realm,
            );
            return fail(Kind::Auth, reason);
        }
        if let Err(e) = sock.send_to(&answer, from).await {
            return kerberos_error(Kind::LocalIo, format!("the loopback socket: {e}"));
        }
    }
}

/// SESSION_SETUP with the cache's ticket; with a ticket-granting ticket only, the relay beside it.
async fn kerberos_setup(conn: &mut Connection, t: &Target, k: &Ticket) -> Result<()> {
    let what = format!(
        "login as '{}\\{}' at {}",
        k.realm,
        k.user,
        hostport(&t.host, t.port)
    );
    let mut creds = KerberosCredentials {
        username: k.user.clone(),
        password: String::new(),
        realm: k.realm.clone(),
        // Never used with a service ticket: the crate takes it from the cache.
        kdc_address: "127.0.0.1:9".to_string(),
    };
    let r = if k.service {
        smb2::Session::setup_kerberos_from_ccache(conn, &creds, &t.host, &k.cache).await
    } else {
        let files = krb5_files(std::env::var_os("KRB5_CONFIG").as_deref());
        let (host, port, from) = kdc_for(&files, &k.realm);
        let loopback = |e: std::io::Error| {
            kerberos_error(Kind::LocalIo, format!("cannot open a loopback socket: {e}"))
        };
        let sock = UdpSocket::bind("127.0.0.1:0").await.map_err(loopback)?;
        creds.kdc_address = sock.local_addr().map_err(loopback)?.to_string();
        let kdc = Kdc {
            target: Target {
                host,
                port,
                ..t.clone()
            },
            from,
            files,
            host: t.host.clone(),
            user: k.user.clone(),
            realm: k.realm.clone(),
        };
        tokio::select! {
            biased;
            e = kdc_relay(sock, &kdc, DIAL_TIMEOUT) => return Err(e),
            r = smb2::Session::setup_kerberos_from_ccache(conn, &creds, &t.host, &k.cache) => r,
        }
    };
    r.map(drop)
        .map_err(|e| kerberos_refusal(e, &what, &t.host, &k.realm))
}

/// The crate's error for a Kerberos SESSION_SETUP, in our words: only an error with a status is
/// the server's.
fn kerberos_refusal(e: smb2::Error, what: &str, host: &str, realm: &str) -> Error {
    let status = e.status();
    let text = e.to_string();
    let c = classify(e, &format!("Kerberos: {what}"));
    if c.is_lost() || (status.is_some() && c.kind == Kind::Auth) {
        return c;
    }
    let reason = match status {
        Some(s) => format!(
            "the server did not accept the ticket ({s}); is {host} in the realm {realm}, and is the \
             ticket valid on the server's clock?"
        ),
        None => format!("the ticket in the cache or the KDC's answer could not be used ({text})"),
    };
    kerberos_error(Kind::Auth, format!("{what}: {reason}"))
}

// ---------------------------------------------------------------------------------------------
// Paths

/// The working directory after `arg`: `/` and `\` separate, a leading one starts at the root, `.`
/// and empty components are dropped, `..` removes one (nothing at the root). No request.
pub fn resolve_path(cwd: &[String], arg: &str) -> Vec<String> {
    let mut out: Vec<String> = if arg.starts_with(['/', '\\']) {
        Vec::new()
    } else {
        cwd.to_vec()
    };
    for c in arg.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(c.to_string()),
        }
    }
    out
}

/// The name a request carries: components joined with `\`, no leading separator, the share root
/// empty. On a DFS tree the path starts with the server and the share, as the crate writes it.
pub fn wire_path(dfs: Option<(&str, &str)>, components: &[String]) -> String {
    let path = components.join("\\");
    match dfs {
        None => path,
        Some((server, share)) if path.is_empty() => format!("{server}\\{share}"),
        Some((server, share)) => format!("{server}\\{share}\\{path}"),
    }
}

fn tree_path(tree: &Tree, components: &[String]) -> String {
    let dfs = tree
        .is_dfs
        .then_some((tree.server.as_str(), tree.share_name.as_str()));
    wire_path(dfs, components)
}

/// A name or message for the terminal: control characters (C0, DEL, C1) and bidi controls
/// (U+202A-202E, U+2066-2069, U+200E, U+200F, U+061C) as `\u{9b}`, so a name from the server
/// cannot move the cursor, start an escape sequence or reorder what is shown. Only for text
/// output: `--json`, the names sent back to the server and local file names keep the exact name.
pub fn safe(s: &str) -> std::borrow::Cow<'_, str> {
    let bad = |c: char| {
        c.is_control()
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{61c}')
    };
    if !s.chars().any(bad) {
        return s.into();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if bad(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out.into()
}

/// `/a/b` for messages and JSON.
pub fn display_path(components: &[String]) -> String {
    format!("/{}", components.join("/"))
}

// ---------------------------------------------------------------------------------------------
// Listing

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// FILETIME: 100 ns since 1601-01-01 UTC; 0 when the server sent none.
    pub mtime: u64,
    pub attributes: u32,
    pub file_id: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attributes & 0x10 != 0
    }
}

const UNIT: u32 = 65_536;
const PROBE: u32 = UNIT;
const MAX_FRAME: u32 = (16 << 20) - (128 << 10);
const MAX_BUFFER: u32 = (8 << 20) - UNIT;
const MAX_QUERIES: u32 = 13;
const CLASS: FileInformationClass = FileInformationClass::FileIdBothDirectoryInformation;

/// (b, n): the buffer of one full query and how many go into one compound, so that the compound
/// (CREATE, n queries, the probe, CLOSE) never asks for more credits than there are (with at least
/// 4) and its reply fits one transport frame.
pub fn plan(max_transact: u32, credits: u16) -> (u32, usize) {
    let c = u32::from(credits.max(4)) - 3;
    let b = max_transact
        .min(MAX_BUFFER)
        .min(c.saturating_mul(UNIT))
        .div_euclid(UNIT)
        .max(1)
        * UNIT;
    let q = b / UNIT;
    let n = (MAX_FRAME / b).min(c / q).clamp(1, MAX_QUERIES);
    (b, n as usize)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Entries,
    End,
    Fail,
}

/// What one query answer means. `NO_SUCH_FILE` on the first query is an empty directory on a
/// server that sends no `.` and `..`.
fn query_outcome(status: NtStatus, first: bool) -> Outcome {
    if status == NtStatus::SUCCESS {
        Outcome::Entries
    } else if status == NtStatus::NO_MORE_FILES || (first && status == NtStatus::NO_SUCH_FILE) {
        Outcome::End
    } else {
        Outcome::Fail
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn malformed() -> Error {
    Error::new(Kind::Other, "malformed directory listing from the server")
}

const ENTRY_FIXED: usize = 104;

/// Entries of class 37 (FileIdBothDirectoryInformation, MS-FSCC 2.4.22). A bad UTF-16 unit in a
/// name becomes U+FFFD; the rest of the buffer is still listed.
fn parse_entries(buf: &[u8], out: &mut Vec<Entry>) -> Result<()> {
    let mut off = 0usize;
    while buf.len().saturating_sub(off) >= ENTRY_FIXED {
        let e = &buf[off..];
        let next = u32_at(e, 0) as usize;
        let name_len = u32_at(e, 60) as usize;
        let name_end = ENTRY_FIXED.checked_add(name_len).ok_or_else(malformed)?;
        // `next` is 0 (the last entry) or reaches past this entry's name: an entry that overlaps
        // the next would be read again and again out of a small buffer.
        if name_end > e.len() || (next != 0 && next < name_end) {
            return Err(malformed());
        }
        let units: Vec<u16> = e[ENTRY_FIXED..name_end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        out.push(Entry {
            name: String::from_utf16_lossy(&units),
            mtime: u64_at(e, 24),
            size: u64_at(e, 40),
            attributes: u32_at(e, 56),
            file_id: u64_at(e, 96),
        });
        if next == 0 {
            break;
        }
        off = off.checked_add(next).ok_or_else(malformed)?;
    }
    Ok(())
}

/// The output buffer of a QUERY_DIRECTORY answer, without copying it.
fn query_buffer(f: &Frame) -> Result<&[u8]> {
    let b = &f.body;
    if b.len() < 8 {
        return Err(malformed());
    }
    let off = (u16_at(b, 2) as usize)
        .checked_sub(64)
        .ok_or_else(malformed)?;
    let len = u32_at(b, 4) as usize;
    b.get(off..off + len).ok_or_else(malformed)
}

fn open_request(name: String, access: u32) -> CreateRequest {
    CreateRequest {
        requested_oplock_level: OplockLevel::None,
        impersonation_level: ImpersonationLevel::Impersonation,
        desired_access: FileAccessMask::new(access),
        file_attributes: 0,
        share_access: ShareAccess(
            ShareAccess::FILE_SHARE_READ
                | ShareAccess::FILE_SHARE_WRITE
                | ShareAccess::FILE_SHARE_DELETE,
        ),
        create_disposition: CreateDisposition::FileOpen,
        create_options: 1, // FILE_DIRECTORY_FILE
        name,
        create_contexts: vec![],
    }
}

const CD_ACCESS: u32 = FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE;
const LIST_ACCESS: u32 = FileAccessMask::FILE_READ_DATA // FILE_LIST_DIRECTORY on a directory
    | FileAccessMask::FILE_READ_ATTRIBUTES
    | FileAccessMask::SYNCHRONIZE;

fn query(file_id: FileId, len: u32, restart: bool) -> QueryDirectoryRequest {
    QueryDirectoryRequest {
        file_information_class: CLASS,
        flags: QueryDirectoryFlags(if restart {
            QueryDirectoryFlags::RESTART_SCANS
        } else {
            0
        }),
        file_index: 0,
        file_id,
        output_buffer_length: len,
        file_name: "*".into(),
    }
}

fn query_op(q: &QueryDirectoryRequest, tid: TreeId) -> CompoundOp<'_> {
    CompoundOp {
        command: Command::QueryDirectory,
        body: q,
        tree_id: Some(tid),
        credit_charge: CreditCharge(q.output_buffer_length.div_ceil(UNIT) as u16),
    }
}

/// Feeds query answers into `out`; true when one of them ended the listing.
fn take_queries(
    answers: impl Iterator<Item = smb2::Result<Frame>>,
    mut first: bool,
    out: &mut Vec<Entry>,
    what: &str,
) -> Result<bool> {
    for a in answers {
        let f = a.map_err(|e| classify(e, what))?;
        match query_outcome(f.header.status, first) {
            Outcome::Entries => {
                // SUCCESS always carries an entry; an empty answer would make the loop that asks
                // again run for ever.
                let before = out.len();
                parse_entries(query_buffer(&f)?, out)?;
                if out.len() == before {
                    return Err(malformed());
                }
            }
            Outcome::End => return Ok(true),
            Outcome::Fail => return Err(classify(status_error(&f), what)),
        }
        first = false;
    }
    Ok(false)
}

/// Lists one directory. Phase 1 is one compound, CREATE, n queries, a 64 KiB probe and CLOSE: one
/// flight for any directory up to n × b bytes of entries. Phase 2 lists again with the handle
/// kept open. `.` and `..` are kept (their file ids are what the walk's loop check starts from);
/// callers drop them.
async fn list_dir(
    conn: &Connection,
    tid: TreeId,
    path: String,
    b: u32,
    n: usize,
    what: &str,
) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let create = open_request(path, LIST_ACCESS);
    let first = query(FileId::SENTINEL, b, true);
    let more = query(FileId::SENTINEL, b, false);
    let probe = query(FileId::SENTINEL, PROBE, false);
    let close = CloseRequest {
        flags: 0,
        file_id: FileId::SENTINEL,
    };
    let mut ops = vec![
        CompoundOp::new(Command::Create, &create, Some(tid)),
        query_op(&first, tid),
    ];
    for _ in 1..n {
        ops.push(query_op(&more, tid));
    }
    let head = ops.len();
    ops.push(query_op(&probe, tid));
    ops.push(CompoundOp::new(Command::Close, &close, Some(tid)));

    let mut answers = conn
        .execute_compound(&ops)
        .await
        .map_err(|e| classify(e, what))?
        .into_iter();
    ok_frame(answers.next().ok_or_else(malformed)?, what)?;
    let queries = answers.by_ref().take(n + 1);
    if take_queries(queries, true, &mut out, what)? {
        return Ok(out);
    }

    // Phase 2: larger than n buffers. The handle above is closed; list again and keep it open.
    out.clear();
    ops.truncate(head);
    let mut answers = conn
        .execute_compound(&ops)
        .await
        .map_err(|e| classify(e, what))?
        .into_iter();
    let cf = ok_frame(answers.next().ok_or_else(malformed)?, what)?;
    let fid = CreateResponse::unpack(&mut ReadCursor::new(&cf.body))
        .map_err(|_| malformed())?
        .file_id;
    let mut result = take_queries(answers, true, &mut out, what);
    while let Ok(false) = result {
        let head = query(fid, b, false);
        let mut ops = vec![query_op(&head, tid)];
        for _ in 1..n {
            ops.push(query_op(&more, tid));
        }
        result = match conn.execute_compound(&ops).await {
            Ok(a) => take_queries(a.into_iter(), false, &mut out, what),
            Err(e) => Err(classify(e, what)),
        };
    }
    if let Err(e) = &result
        && e.is_lost()
    {
        return Err(e.clone());
    }
    let closed = conn
        .execute(
            Command::Close,
            &CloseRequest {
                flags: 0,
                file_id: fid,
            },
            Some(tid),
        )
        .await;
    result?;
    closed.map_err(|e| classify(e, what))?;
    Ok(out)
}

fn retain_real(entries: &mut Vec<Entry>) {
    entries.retain(|e| e.name != "." && e.name != "..");
}

// ---------------------------------------------------------------------------------------------
// Cache

/// Directory listings by (share, path), both folded to lower case, capped by units: one per
/// entry plus one per listing; the least recently stored listing goes first.
type Listing = Arc<Vec<Entry>>;
type Key = (String, String);

pub struct Cache {
    map: HashMap<Key, (Listing, u64)>,
    /// Tick → key, oldest first.
    order: BTreeMap<u64, Key>,
    total: usize,
    cap: usize,
    tick: u64,
}

/// What a listing costs in the cache: an empty one costs too.
fn units(l: &Listing) -> usize {
    l.len() + 1
}

pub const CACHE_CAP: usize = 500_000;

fn cache_key(share: &str, dir: &[String]) -> (String, String) {
    (share.to_lowercase(), dir.join("/").to_lowercase())
}

impl Cache {
    pub fn new(cap: usize) -> Cache {
        Cache {
            map: HashMap::new(),
            order: BTreeMap::new(),
            total: 0,
            cap,
            tick: 0,
        }
    }

    fn remove(&mut self, key: &Key) {
        if let Some((e, t)) = self.map.remove(key) {
            self.total -= units(&e);
            self.order.remove(&t);
        }
    }

    pub fn get(&self, share: &str, dir: &[String]) -> Option<Arc<Vec<Entry>>> {
        self.map.get(&cache_key(share, dir)).map(|(e, _)| e.clone())
    }

    pub fn put(&mut self, share: &str, dir: &[String], entries: Arc<Vec<Entry>>) {
        let key = cache_key(share, dir);
        self.remove(&key);
        let u = units(&entries);
        if u > self.cap {
            return;
        }
        while self.total + u > self.cap {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            if let Some((e, _)) = self.map.remove(&k) {
                self.total -= units(&e);
            }
        }
        self.tick += 1;
        self.total += u;
        self.order.insert(self.tick, key.clone());
        self.map.insert(key, (entries, self.tick));
    }

    /// A walk's listing. `fed`: the folded keys this walk has stored (true) or has given up
    /// (false). Two directories with one folded key (`Foo` and `foo`, which a server may hold as
    /// two directories) are stored neither, whether or not the walk lists both: they are found
    /// among the names of the parent's listing, and what is below a directory not stored is not
    /// stored either.
    pub fn feed(
        &mut self,
        fed: &mut HashMap<Key, bool>,
        share: &str,
        dir: &[String],
        entries: Arc<Vec<Entry>>,
    ) {
        let key = cache_key(share, dir);
        let stored = match fed.get(&key) {
            None => {
                self.put(share, dir, entries.clone());
                fed.insert(key, true);
                true
            }
            Some(true) => {
                self.remove(&key);
                fed.insert(key, false);
                false
            }
            Some(false) => false,
        };
        let mut kids = HashSet::new();
        for e in entries.iter().filter(|e| e.is_dir()) {
            let mut d = dir.to_vec();
            d.push(e.name.clone());
            let k = cache_key(share, &d);
            if !stored || !kids.insert(k.clone()) {
                self.remove(&k);
                fed.insert(k, false);
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.total = 0;
    }

    /// Drops the listing of `dir`; with `subtree` also every listing below it (every listing of
    /// the share when `dir` is the root).
    pub fn invalidate(&mut self, share: &str, dir: &[String], subtree: bool) {
        let (s, d) = cache_key(share, dir);
        let below = format!("{d}/");
        let gone: Vec<Key> = self
            .map
            .keys()
            .filter(|(ks, kd)| {
                *ks == s && (*kd == d || (subtree && (d.is_empty() || kd.starts_with(&below))))
            })
            .cloned()
            .collect();
        for k in gone {
            self.remove(&k);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Share list

#[derive(Clone, Debug)]
pub struct Share {
    pub name: String,
    pub share_type: u32,
    pub comment: String,
}

impl Share {
    pub fn type_name(&self) -> &'static str {
        match self.share_type & 0xffff {
            0 => "disk",
            1 => "printer",
            2 => "device",
            3 => "ipc",
            _ => "unknown",
        }
    }
    pub fn hidden(&self) -> bool {
        self.name.ends_with('$') || self.share_type & 0x8000_0000 != 0
    }
}

/// What a stub may hold: a PDU length is 16 bits, and the PDU is the 24 byte header and the stub.
const STUB_MAX: usize = 65_535 - 24;

fn too_large(n: usize) -> Error {
    Error::new(
        Kind::Other,
        format!("the share list is too large for rsmb ({n} bytes; the limit is 65,511)"),
    )
}

/// One response fragment into the stub, checked while collecting: the list may not grow past the
/// limit, and a fragment that adds nothing and is not the last would keep a hostile server's
/// answers coming for ever.
fn add_fragment(stub: &mut Vec<u8>, part: &[u8], last: bool) -> Result<()> {
    if stub.len() + part.len() > STUB_MAX {
        return Err(too_large(stub.len() + part.len()));
    }
    if part.is_empty() && !last {
        return Err(malformed());
    }
    stub.extend_from_slice(part);
    Ok(())
}

/// The reassembled stub inside one DCE/RPC response PDU, which the crate's parser takes.
fn share_pdu(stub: &[u8]) -> Result<Vec<u8>> {
    let len = 24 + stub.len();
    if stub.len() > STUB_MAX {
        return Err(too_large(stub.len()));
    }
    let mut pdu = vec![5, 0, 2, 3, 0x10, 0, 0, 0];
    pdu.extend((len as u16).to_le_bytes());
    pdu.extend([0, 0]);
    pdu.extend(2u32.to_le_bytes()); // call id
    pdu.extend((stub.len() as u32).to_le_bytes()); // alloc hint
    pdu.extend([0, 0, 0, 0]); // context id, cancel count, reserved
    pdu.extend_from_slice(stub);
    Ok(pdu)
}

/// One pipe message: READs while the server says there is more.
async fn read_pipe(conn: &Connection, tid: TreeId, fid: FileId) -> Result<Vec<u8>> {
    let mut msg = Vec::new();
    loop {
        let req = ReadRequest {
            padding: 0x50,
            flags: 0,
            length: 65_536,
            offset: 0,
            file_id: fid,
            minimum_count: 0,
            channel: 0,
            remaining_bytes: 0,
            read_channel_info: vec![],
        };
        let f = conn
            .execute(Command::Read, &req, Some(tid))
            .await
            .map_err(|e| classify(e, "share list"))?;
        let st = f.header.status;
        if !st.is_success_or_partial() {
            return Err(classify(status_error(&f), "share list"));
        }
        let r = ReadResponse::unpack(&mut ReadCursor::new(&f.body)).map_err(|_| malformed())?;
        // A PDU is at most 65,535 bytes; a read that brings nothing while the server says "more"
        // would never end.
        if r.data.is_empty() || msg.len() + r.data.len() > 65_535 {
            return Err(malformed());
        }
        msg.extend(r.data);
        if st != NtStatus::BUFFER_OVERFLOW {
            return Ok(msg);
        }
    }
}

async fn write_pipe(conn: &Connection, tid: TreeId, fid: FileId, data: Vec<u8>) -> Result<()> {
    let req = WriteRequest {
        data_offset: 0x70,
        offset: 0,
        file_id: fid,
        channel: 0,
        remaining_bytes: 0,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
        flags: 0,
        data,
    };
    ok_frame(
        conn.execute(Command::Write, &req, Some(tid)).await,
        "share list",
    )
    .map(|_| ())
}

/// srvsvc NetrShareEnumAll over an open pipe.
async fn enum_shares(conn: &Connection, tid: TreeId, fid: FileId) -> Result<Vec<Share>> {
    let bad = |e: smb2::Error| classify(e, "share list");
    write_pipe(conn, tid, fid, smb2::rpc::build_srvsvc_bind(1)).await?;
    smb2::rpc::parse_bind_ack(&read_pipe(conn, tid, fid).await?).map_err(bad)?;
    let server = format!(r"\\{}", conn.server_name());
    let req = smb2::rpc::srvsvc::build_net_share_enum_all(2, &server);
    write_pipe(conn, tid, fid, req).await?;
    let mut stub = Vec::new();
    loop {
        let pdu = read_pipe(conn, tid, fid).await?;
        let (s, last) = smb2::rpc::parse_response_fragment(&pdu).map_err(bad)?;
        add_fragment(&mut stub, s, last)?;
        if last {
            break;
        }
    }
    let list =
        smb2::rpc::srvsvc::parse_net_share_enum_all_response(&share_pdu(&stub)?).map_err(bad)?;
    Ok(list
        .into_iter()
        .map(|s| Share {
            name: s.name,
            share_type: s.share_type,
            comment: s.comment,
        })
        .collect())
}

/// Shares asked about in one compound. 64 worked on Samba; 256 (512 requests) lost the connection.
const ACCESS_CHAIN: usize = 16;

/// `rw`, `r`, `w` or `none` from a TREE_CONNECT's MaximalAccess: read is FILE_READ_DATA, write is
/// FILE_WRITE_DATA or FILE_APPEND_DATA; GENERIC_ALL and the generic bit count for each.
pub fn access_name(mask: u32) -> &'static str {
    let read = mask & (0x1 | 0x8000_0000 | 0x1000_0000) != 0;
    let write = mask & (0x2 | 0x4 | 0x4000_0000 | 0x1000_0000) != 0;
    match (read, write) {
        (true, true) => "rw",
        (true, false) => "r",
        (false, true) => "w",
        (false, false) => "none",
    }
}

/// One TREE_CONNECT answer: its access, `none` when refused with ACCESS_DENIED, None for any
/// other failure (the share is still listed).
fn access_of(answer: smb2::Result<Frame>) -> Option<&'static str> {
    let f = answer.ok()?;
    if f.header.status == NtStatus::ACCESS_DENIED {
        return Some("none");
    }
    if f.header.status != NtStatus::SUCCESS {
        return None;
    }
    let r = TreeConnectResponse::unpack(&mut ReadCursor::new(&f.body)).ok()?;
    Some(access_name(r.maximal_access))
}

/// TREE_CONNECT and a related TREE_DISCONNECT per share, all in one compound: 2 requests per share,
/// 1 flight. The server disconnects the tree its TREE_CONNECT just made (the tree id sent,
/// 0xFFFFFFFF, is never valid on its own), so nothing is left behind and the tree map is untouched.
async fn probe_access(conn: &Connection, names: &[String]) -> Result<Vec<Option<&'static str>>> {
    let reqs: Vec<TreeConnectRequest> = names
        .iter()
        .map(|n| TreeConnectRequest {
            flags: TreeConnectRequestFlags::default(),
            path: format!(r"\\{}\{n}", conn.server_name()),
        })
        .collect();
    let mut ops = Vec::with_capacity(2 * reqs.len());
    for r in &reqs {
        ops.push(CompoundOp::new(Command::TreeConnect, r, None));
        let last = Some(TreeId(u32::MAX));
        ops.push(CompoundOp::new(
            Command::TreeDisconnect,
            &TreeDisconnectRequest,
            last,
        ));
    }
    let answers = conn
        .execute_compound(&ops)
        .await
        .map_err(|e| classify(e, "share access"))?;
    let mut out = Vec::with_capacity(names.len());
    for a in answers.into_iter().step_by(2) {
        match a {
            // A lost connection answers every request of the compound with an error.
            Err(e) => {
                let e = classify(e, "share access");
                if e.is_lost() {
                    return Err(e);
                }
                out.push(None);
            }
            a => out.push(access_of(a)),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Names, the compound open

/// A name the crate sends as it is: not empty, no NUL, and nothing the crate would map into the
/// private-use area (`"*:<>?\|`, a control character, a trailing space or period; `smb2::name`).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('\0') && smb2::encode_name(name) == name
}

/// The path a crate call takes: every component `valid_name`, joined with `/` (the crate's `Tree`
/// methods split on `/`, take `\` as a name character, and add the DFS prefix themselves).
pub fn crate_path(components: &[String]) -> Result<String> {
    if let Some(bad) = components.iter().find(|c| !valid_name(c)) {
        return Err(Error::new(
            Kind::InvalidName,
            format!(
                "{}: '{bad}' has a character SMB does not allow",
                display_path(components)
            ),
        ));
    }
    Ok(components.join("/"))
}

/// CREATE + CLOSE in one compound: 2 requests, 1 flight. The CREATE's answer decides. Used by
/// `cd` (open a directory), the existence check (open anything) and `mkdir` (create, or
/// open-or-create, a directory).
async fn open_close(
    conn: &Connection,
    tree: &Tree,
    path: &[String],
    disposition: CreateDisposition,
    options: u32,
    attributes: u32,
    what: &str,
) -> Result<CreateResponse> {
    let mut create = open_request(tree_path(tree, path), CD_ACCESS);
    create.create_disposition = disposition;
    create.create_options = options;
    create.file_attributes = attributes;
    create_close(conn, tree, create, what).await
}

/// The compound itself: the CREATE as given, then a CLOSE of its handle.
async fn create_close(
    conn: &Connection,
    tree: &Tree,
    create: CreateRequest,
    what: &str,
) -> Result<CreateResponse> {
    let close = CloseRequest {
        flags: 0,
        file_id: FileId::SENTINEL,
    };
    let tid = Some(tree.tree_id);
    let ops = [
        CompoundOp::new(Command::Create, &create, tid),
        CompoundOp::new(Command::Close, &close, tid),
    ];
    let answers = conn
        .execute_compound(&ops)
        .await
        .map_err(|e| classify(e, what))?;
    let f = ok_frame(answers.into_iter().next().ok_or_else(malformed)?, what)?;
    CreateResponse::unpack(&mut ReadCursor::new(&f.body)).map_err(|_| malformed())
}

// ---------------------------------------------------------------------------------------------
// Remote: what a transfer, mkdir, rm and rmdir send

/// Largest file for the small path (one compound read or write).
pub const SMALL: u64 = 262_144;
/// Cap of the full query of a walk listing.
pub const WALK_BUFFER: u32 = 1_048_576;
/// The buffer a local file is read into for an upload.
pub const CHUNK: usize = 524_288;
/// How long the cleanup may use the connection that carried an upload.
pub const CLEANUP_WAIT: Duration = Duration::from_secs(2);

/// A remote temporary file that exists or may exist.
#[derive(Clone, Debug)]
struct Temp {
    path: Vec<String>,
    target: Vec<String>,
    /// The rename was sent and not answered.
    renaming: bool,
}

type Registry = Arc<StdMutex<Vec<Temp>>>;

/// Adds a temporary file before the CREATE that makes it. A name already there is refused, so two
/// transfers with the same temporary name are never in flight together.
fn register(reg: &StdMutex<Vec<Temp>>, path: &[String], target: &[String]) -> Result<()> {
    let mut r = reg.lock().unwrap();
    if r.iter().any(|t| t.path == path) {
        return Err(Error::other(format!(
            "{}: a transfer with the same temporary name is running",
            display_path(path)
        )));
    }
    r.push(Temp {
        path: path.to_vec(),
        target: target.to_vec(),
        renaming: false,
    });
    Ok(())
}

fn unregister(reg: &StdMutex<Vec<Temp>>, path: &[String]) {
    reg.lock().unwrap().retain(|t| t.path != path);
}

fn set_renaming(reg: &StdMutex<Vec<Temp>>, path: &[String], on: bool) {
    for t in reg.lock().unwrap().iter_mut().filter(|t| t.path == path) {
        t.renaming = on;
    }
}

/// What a failed (or timed-out: `None`) delete in the cleanup means.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Deleted, or not there: out of the registry.
    Gone,
    /// The connection cannot do it (lost, no credits, out of time, or a handle of our own session
    /// still open on the file): try on one new connection.
    NewConnection,
    /// The server refused for a reason a new connection would not change.
    Stays,
}

pub fn cleanup_verdict(r: Option<&smb2::Result<()>>) -> Verdict {
    use smb2::ErrorKind as K;
    match r {
        None => Verdict::NewConnection,
        Some(Ok(())) => Verdict::Gone,
        Some(Err(e)) => match e.kind() {
            K::NotFound => Verdict::Gone,
            K::ConnectionLost | K::TimedOut | K::SharingViolation | K::Io | K::SessionExpired => {
                Verdict::NewConnection
            }
            _ => Verdict::Stays,
        },
    }
}

/// One wording for a failed rename of a temporary file onto `target` (5.3 of the Stage 2 plan):
/// `is_dir` is what a `stat` of the target found after a collision (None: not asked or nothing).
pub fn rename_error(e: Error, target: &str, replace: bool, is_dir: Option<bool>) -> Error {
    match (e.kind, is_dir, replace) {
        (Kind::AlreadyExists, Some(true), _) => Error::new(
            Kind::IsADirectory,
            format!("{target}: is a directory; write '{target}/' to put the file into it"),
        ),
        (Kind::AlreadyExists, _, false) => Error::new(
            Kind::AlreadyExists,
            format!("{target}: exists on the server; -f replaces it"),
        ),
        (Kind::AlreadyExists, _, true) => {
            Error::other(format!("{target}: the server did not replace it"))
        }
        (Kind::AccessDenied, _, true) => Error::new(
            Kind::AccessDenied,
            format!(
                "{target}: the server refused to replace it (open in another program, \
                 read-only, or not permitted)"
            ),
        ),
        _ => e,
    }
}

/// The create-only CREATE of a temporary file found the name taken.
fn is_collision(e: &smb2::Error) -> bool {
    e.kind() == smb2::ErrorKind::AlreadyExists
}

/// The open of a temporary file failed.
fn temp_open_error(e: smb2::Error, temp: &[String]) -> Error {
    if e.kind() == smb2::ErrorKind::SharingViolation {
        Error::other(format!(
            "{}: in use: another transfer is writing it, or the server has not yet closed the \
             handle of an earlier one; try again later",
            display_path(temp)
        ))
    } else {
        classify(e, &display_path(temp))
    }
}

fn joined(dir: &[String], name: &str) -> Vec<String> {
    let mut v = dir.to_vec();
    v.push(name.to_string());
    v
}

/// Held by whatever must end before the cleanup runs (the local write of a small file).
#[derive(Clone)]
pub struct Alive(#[allow(dead_code)] mpsc::Sender<()>);

pub struct Stat {
    pub is_dir: bool,
    pub size: u64,
}

/// What a download hands its sink: the size at open, then the data in order.
pub enum Part<'a> {
    Start(u64),
    Data(&'a [u8]),
}

/// What a command needs to overlap requests on the one connection; the only way `transfer.rs` and
/// `walk.rs` reach the server. Lives for one command (`Session::remote` … `Session::finish`).
#[derive(Clone)]
pub struct Remote {
    conn: Connection,
    tree: Arc<Tree>,
    walk_buf: u32,
    temps: Registry,
    writer: Arc<Mutex<Option<FileWriter>>>,
    dirty: Arc<AtomicBool>,
    alive: mpsc::Sender<()>,
    /// min(`SMALL`, negotiated read size, negotiated write size).
    pub small: u64,
}

impl Remote {
    fn dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub fn alive(&self) -> Alive {
        Alive(self.alive.clone())
    }

    /// One walk listing: CREATE, one full query, the probe, CLOSE (4 requests, 1 flight).
    pub async fn list(&self, dir: &[String]) -> Result<walk::Listing> {
        let what = display_path(dir);
        let path = tree_path(&self.tree, dir);
        let all = list_dir(&self.conn, self.tree.tree_id, path, self.walk_buf, 1, &what).await?;
        let mut l = walk::Listing {
            entries: Vec::with_capacity(all.len()),
            id: 0,
            parent: 0,
        };
        for e in all {
            match e.name.as_str() {
                "." => l.id = e.file_id,
                ".." => l.parent = e.file_id,
                _ => l.entries.push(e),
            }
        }
        Ok(l)
    }

    /// 2 requests, 1 flight; None when nothing has the name.
    pub async fn stat(&self, path: &[String]) -> Result<Option<Stat>> {
        crate_path(path)?;
        let what = display_path(path);
        let open = CreateDisposition::FileOpen;
        match open_close(&self.conn, &self.tree, path, open, 0, 0, &what).await {
            Ok(c) => Ok(Some(Stat {
                is_dir: c.file_attributes & 0x10 != 0,
                size: c.end_of_file,
            })),
            Err(e) if e.kind == Kind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Creates a directory (2 / 1). With `existing_ok` the open-or-create form: success on a
    /// directory that is there, `not_a_directory` for a file of that name.
    pub async fn mkdir(&self, path: &[String], existing_ok: bool) -> Result<()> {
        crate_path(path)?;
        let disposition = if existing_ok {
            CreateDisposition::FileOpenIf
        } else {
            CreateDisposition::FileCreate
        };
        let what = display_path(path);
        open_close(&self.conn, &self.tree, path, disposition, 1, 0x10, &what)
            .await
            .map(|_| ())
    }

    /// `delete_file` or `delete_directory` (3 / 1).
    pub async fn remove(&self, path: &[String], dir: bool) -> Result<()> {
        let p = crate_path(path)?;
        let mut c = self.conn.clone();
        let r = if dir {
            self.tree.delete_directory(&mut c, &p).await
        } else {
            self.tree.delete_file(&mut c, &p).await
        };
        r.map_err(|e| classify(e, &display_path(path)))
    }

    /// The whole file in one compound (3 / 1). None: it is larger than `expected` now.
    pub async fn read_small(&self, path: &[String], expected: u64) -> Result<Option<Vec<u8>>> {
        let p = crate_path(path)?;
        let what = display_path(path);
        let mut c = self.conn.clone();
        let r = self
            .tree
            .read_file_compound_sized_with_info(&mut c, &p, expected.max(1))
            .await;
        match r {
            Ok((_, info)) if info.is_directory => Err(Error::new(
                Kind::IsADirectory,
                format!("{what}: is a directory"),
            )),
            Ok((data, info)) if data.len() as u64 != info.size => {
                Err(Error::other(format!("{what}: changed while it was read")))
            }
            Ok((data, _)) => Ok(Some(data)),
            Err(smb2::Error::FileTooLargeForSingleRead { .. }) => Ok(None),
            // A name that has become a directory answers the READ so (finding 14).
            Err(e) if e.status() == Some(NtStatus::INVALID_DEVICE_REQUEST) => Err(Error::new(
                Kind::IsADirectory,
                format!("{what}: is a directory"),
            )),
            Err(e) => Err(classify(e, &what)),
        }
    }

    /// The stream path down: `sink` gets the size at open, then every chunk in order. Fails when
    /// fewer bytes arrive than the size at open (a file that shrinks ends the crate's download
    /// without an error).
    pub async fn download(
        &self,
        path: &[String],
        sink: &mut dyn FnMut(Part) -> Result<()>,
    ) -> Result<u64> {
        let p = crate_path(path)?;
        let what = display_path(path);
        let mut c = self.conn.clone();
        let mut dl = self
            .tree
            .download(&mut c, &p)
            .await
            .map_err(|e| classify(e, &what))?;
        if dl.info().is_some_and(|i| i.is_directory) {
            // Size 0: this collects the CLOSE, so nothing is left on the wire.
            while dl.next_chunk().await.is_some() {}
            return Err(Error::new(
                Kind::IsADirectory,
                format!("{what}: is a directory"),
            ));
        }
        let size = dl.size();
        // A refusal here has sent no READ (the crate builds its window on the first chunk) and
        // dropping `dl` closes the handle, so the connection is kept. From the first chunk on a
        // failure may leave READs on the wire: `finish` replaces the connection.
        sink(Part::Start(size))?;
        let mut n = 0u64;
        while let Some(chunk) = dl.next_chunk().await {
            let chunk = chunk.map_err(|e| {
                let e = classify(e, &what);
                if !e.is_lost() {
                    self.dirty();
                }
                e
            })?;
            n += chunk.len() as u64;
            sink(Part::Data(&chunk)).inspect_err(|_| self.dirty())?;
        }
        if n != size {
            return Err(Error::other(format!(
                "{what}: changed during the transfer ({size} bytes when opened, {n} received)"
            )));
        }
        Ok(n)
    }

    /// Deletes a temporary file; out of the registry if that worked or it was not there.
    async fn delete_temp(&self, temp: &[String]) {
        let Ok(p) = crate_path(temp) else { return };
        let mut c = self.conn.clone();
        let r = self.tree.delete_file(&mut c, &p).await;
        if cleanup_verdict(Some(&r)) == Verdict::Gone {
            unregister(&self.temps, temp);
        }
    }

    /// The create-only CREATE of the temporary name `temp` found something there: a leftover of an
    /// earlier transfer, or an object of someone else. Removes it so the CREATE can be repeated
    /// (once: a second collision fails the item). The open asks for the object itself
    /// (FILE_OPEN_REPARSE_POINT), so a link is removed and not what it points to, which a plain
    /// delete does on Samba. On failure the name is out of the registry again: nothing of ours is
    /// there.
    async fn take_name(&self, temp: &[String]) -> Result<()> {
        // FILE_NON_DIRECTORY_FILE | FILE_DELETE_ON_CLOSE | FILE_OPEN_REPARSE_POINT
        const OPTIONS: u32 = 0x40 | 0x1000 | 0x0020_0000;
        let what = display_path(temp);
        let access = FileAccessMask::DELETE | FileAccessMask::SYNCHRONIZE;
        let mut create = open_request(tree_path(&self.tree, temp), access);
        create.create_options = OPTIONS;
        match create_close(&self.conn, &self.tree, create, &what).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind == Kind::NotFound => Ok(()),
            Err(e) if e.is_lost() => Err(e),
            Err(e) => {
                unregister(&self.temps, temp);
                Err(Error::new(
                    e.kind,
                    format!(
                        "{}; the name is taken by an earlier transfer or another program: \
                         try again later",
                        e.message
                    ),
                ))
            }
        }
    }

    /// The rename shared by both upload paths, with its one wording of a failure.
    async fn rename(&self, temp: &[String], target: &[String], replace: bool) -> Result<()> {
        let (tp, fp) = (crate_path(temp)?, crate_path(target)?);
        let shown = display_path(target);
        set_renaming(&self.temps, temp, true);
        let mut c = self.conn.clone();
        let options = RenameOptions::new().replace_if_exists(replace);
        let r = self.tree.rename_with(&mut c, &tp, &fp, options).await;
        let e = match r {
            Ok(()) => {
                unregister(&self.temps, temp);
                return Ok(());
            }
            Err(e) => classify(e, &shown),
        };
        if e.is_lost() {
            return Err(e);
        }
        set_renaming(&self.temps, temp, false);
        self.delete_temp(temp).await;
        let is_dir = if e.kind == Kind::AlreadyExists {
            self.stat(target).await.ok().flatten().map(|s| s.is_dir)
        } else {
            None
        };
        Err(rename_error(e, &shown, replace, is_dir))
    }

    /// The small path up: CREATE + WRITE + FLUSH, CLOSE, the rename (7 requests, 3 flights).
    pub async fn put_small(
        &self,
        dir: &[String],
        temp: &str,
        name: &str,
        data: &[u8],
        replace: bool,
    ) -> Result<()> {
        let (tpath, fpath) = (joined(dir, temp), joined(dir, name));
        let tp = crate_path(&tpath)?;
        crate_path(&fpath)?;
        register(&self.temps, &tpath, &fpath)?;
        let mut c = self.conn.clone();
        // Create-only: never write through whatever already has the name (a link, say).
        let mut r = self
            .tree
            .write_file_compound_exclusive(&mut c, &tp, data)
            .await;
        if r.as_ref().is_err_and(is_collision) {
            self.take_name(&tpath).await?;
            r = self
                .tree
                .write_file_compound_exclusive(&mut c, &tp, data)
                .await;
        }
        match r {
            Ok(n) if n == data.len() as u64 => {}
            Ok(n) => {
                self.delete_temp(&tpath).await;
                return Err(Error::other(format!(
                    "{}: the server confirmed {n} of {} bytes",
                    display_path(&fpath),
                    data.len()
                )));
            }
            Err(e) => {
                let created = !matches!(
                    &e,
                    smb2::Error::Protocol {
                        command: Command::Create,
                        ..
                    }
                );
                let e = if created {
                    classify(e, &display_path(&fpath))
                } else {
                    unregister(&self.temps, &tpath);
                    temp_open_error(e, &tpath)
                };
                if created && !e.is_lost() {
                    self.delete_temp(&tpath).await;
                }
                return Err(e);
            }
        }
        self.rename(&tpath, &fpath, replace).await
    }

    /// The stream path up: the target is checked first (2 / 1, always), then the crate's writer
    /// to the temporary name, `finish`, the rename. Returns the bytes sent.
    pub async fn upload(
        &self,
        dir: &[String],
        temp: &str,
        name: &str,
        replace: bool,
        source: &mut dyn FnMut(&mut [u8]) -> Result<usize>,
    ) -> Result<u64> {
        let (tpath, fpath) = (joined(dir, temp), joined(dir, name));
        let tp = crate_path(&tpath)?;
        let shown = display_path(&fpath);
        match self.stat(&fpath).await? {
            Some(s) if s.is_dir => {
                let e = Error::new(Kind::AlreadyExists, "");
                return Err(rename_error(e, &shown, replace, Some(true)));
            }
            Some(_) if !replace => {
                let e = Error::new(Kind::AlreadyExists, "");
                return Err(rename_error(e, &shown, false, Some(false)));
            }
            _ => {}
        }
        register(&self.temps, &tpath, &fpath)?;
        let tree = &self.tree;
        let mut r = tree
            .create_file_writer_exclusive(self.conn.clone(), &tp)
            .await;
        if r.as_ref().is_err_and(is_collision) {
            self.take_name(&tpath).await?;
            r = tree
                .create_file_writer_exclusive(self.conn.clone(), &tp)
                .await;
        }
        let w = match r {
            Ok(w) => w,
            Err(e) => {
                let e = temp_open_error(e, &tpath);
                if !e.is_lost() {
                    unregister(&self.temps, &tpath);
                }
                return Err(e);
            }
        };
        *self.writer.lock().await = Some(w);
        let mut buf = vec![0u8; CHUNK];
        let mut sent = 0u64;
        loop {
            let n = match source(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    self.abort_writer(&tpath).await;
                    return Err(e);
                }
            };
            if n == 0 {
                break;
            }
            // The lock is held across the write: a dropped future leaves the writer in the slot
            // for the cleanup, with everything on the wire tracked (finding 4).
            let r = match self.writer.lock().await.as_mut() {
                Some(w) => w.write_chunk(&buf[..n]).await,
                None => Err(smb2::Error::Disconnected),
            };
            if let Err(e) = r {
                let e = classify(e, &shown);
                if !e.is_lost() {
                    self.abort_writer(&tpath).await;
                }
                return Err(e);
            }
            sent += n as u64;
        }
        let Some(w) = self.writer.lock().await.take() else {
            return Err(Error::new(
                Kind::ConnectionLost { silent: false },
                "connection lost: the upload's writer is gone",
            ));
        };
        match w.finish().await {
            Ok(confirmed) if confirmed == sent => {}
            Ok(confirmed) => {
                self.delete_temp(&tpath).await;
                return Err(Error::other(format!(
                    "{shown}: the server confirmed {confirmed} of {sent} bytes"
                )));
            }
            Err(e) => {
                // A full disk shows here. The crate has queued the CLOSE.
                let e = classify(e, &shown);
                if !e.is_lost() {
                    self.delete_temp(&tpath).await;
                }
                return Err(e);
            }
        }
        self.rename(&tpath, &fpath, replace).await?;
        Ok(sent)
    }

    /// `abort()` on the writer in the slot (it waits out the writes on the wire and closes), then
    /// the delete of the temporary file.
    async fn abort_writer(&self, temp: &[String]) {
        let w = self.writer.lock().await.take();
        if let Some(w) = w {
            let _ = w.abort().await;
        }
        self.delete_temp(temp).await;
    }
}

/// Deletes every name in the registry, all in flight together. True when a delete needs a new
/// connection (`Verdict::NewConnection`). `gone` gets the targets whose rename was cut and whose
/// temporary file is not there (the rename probably happened).
async fn delete_all(
    conn: &Connection,
    tree: &Arc<Tree>,
    reg: &StdMutex<Vec<Temp>>,
    gone: &mut Vec<String>,
) -> bool {
    let list: Vec<Temp> = reg.lock().unwrap().clone();
    let mut set = JoinSet::new();
    for t in list {
        let (mut c, tree) = (conn.clone(), tree.clone());
        set.spawn(async move {
            let r = match crate_path(&t.path) {
                Ok(p) => tree.delete_file(&mut c, &p).await,
                Err(_) => Ok(()),
            };
            (t, r)
        });
    }
    let mut again = false;
    while let Some(j) = set.join_next().await {
        let Ok((t, r)) = j else { continue };
        match cleanup_verdict(Some(&r)) {
            Verdict::Gone => {
                if t.renaming && r.is_err() {
                    gone.push(display_path(&t.target));
                }
                unregister(reg, &t.path);
            }
            Verdict::NewConnection => again = true,
            Verdict::Stays => {}
        }
    }
    again
}

// ---------------------------------------------------------------------------------------------
// Session

pub struct Session {
    target: Target,
    conn: Connection,
    trees: HashMap<String, Arc<Tree>>,
    share: Option<String>,
    cwd: Vec<String>,
    cache: Cache,
    shares: Option<Arc<Vec<Share>>>,
    /// A method of ours is between its first request and its last answer.
    in_flight: bool,
    /// Remote temporary files that exist or may exist (docs/transfers.md).
    temps: Registry,
    /// The `FileWriter` of the one stream upload that can be running.
    writer: Arc<Mutex<Option<FileWriter>>>,
    /// The receiving half of the current `Remote`'s channel: `recv` is None once every `Remote`
    /// clone and every `Alive` of the command is gone.
    alive: Option<mpsc::Receiver<()>>,
    /// Set by a `Remote` whose failure may have left requests on the wire.
    dirty: Arc<AtomicBool>,
    /// Reconnects so far; the generation the current `Remote` was made on; the generation the
    /// last cleanup made (0: none).
    generation: u64,
    remote_gen: u64,
    cleanup_gen: u64,
}

impl Session {
    pub async fn connect(mut target: Target) -> Result<Session> {
        let (conn, who) = login(&target).await?;
        // With Kerberos the session is the cache's principal from now on (a reconnect that finds
        // another one in the cache fails).
        if let Some((user, realm)) = who {
            target.user = user;
            target.domain = realm;
        }
        Ok(Session::with_conn(target, conn))
    }

    fn with_conn(target: Target, conn: Connection) -> Session {
        Session {
            target,
            conn,
            trees: HashMap::new(),
            share: None,
            cwd: Vec::new(),
            cache: Cache::new(CACHE_CAP),
            shares: None,
            in_flight: false,
            temps: Arc::default(),
            writer: Arc::default(),
            alive: None,
            dirty: Arc::default(),
            generation: 0,
            remote_gen: 0,
            cleanup_gen: 0,
        }
    }

    /// One line for the interactive login message.
    pub fn describe(&self) -> String {
        let dialect = self
            .conn
            .params()
            .map(|p| p.dialect.to_string())
            .unwrap_or_default();
        banner(&self.target, &dialect)
    }

    pub fn host(&self) -> &str {
        &self.target.host
    }
    pub fn share(&self) -> Option<&str> {
        self.share.as_deref()
    }
    pub fn cwd(&self) -> &[String] {
        &self.cwd
    }

    pub fn mark_dead(&self) {
        self.conn.mark_dead();
    }

    /// The connection is known to be gone; the next command that sends reconnects.
    pub fn is_dead(&self) -> bool {
        self.conn.is_disconnected()
    }

    /// After a dropped command: if requests were in flight, the connection is in an unknown state
    /// (credits held, replies on the way) and is replaced by the next command that sends.
    pub fn abandon(&mut self) {
        if self.in_flight {
            self.in_flight = false;
            self.mark_dead();
        }
    }

    /// `//host`, `//host/share/`, `//host/share/a/b`.
    pub fn location(&self) -> String {
        match &self.share {
            None => format!("//{}", self.target.host),
            Some(s) => format!("//{}/{s}/{}", self.target.host, self.cwd.join("/")),
        }
    }

    pub fn resolve(&self, arg: &str) -> Vec<String> {
        resolve_path(&self.cwd, arg)
    }

    /// The only place that reconnects. Sends nothing on a live connection.
    async fn ensure_connected(&mut self) -> Result<()> {
        if self.conn.is_disconnected() {
            match login(&self.target).await {
                Ok((c, _)) => {
                    self.conn = c;
                    self.trees.clear();
                    self.generation += 1;
                }
                Err(e) => {
                    return Err(Error::new(
                        e.kind,
                        format!("reconnect failed: {}", e.message),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The tree for `share`, connected on first use. Every method that sends starts here.
    async fn tree(&mut self, share: &str) -> Result<Arc<Tree>> {
        self.ensure_connected().await?;
        let key = share.to_lowercase();
        if let Some(t) = self.trees.get(&key) {
            return Ok(t.clone());
        }
        let t = Tree::connect(&mut self.conn, share)
            .await
            .map_err(|e| classify(e, &format!("share '{share}'")))?;
        let t = Arc::new(t);
        self.trees.insert(key, t.clone());
        Ok(t)
    }

    fn selected(&self) -> Result<String> {
        self.share
            .clone()
            .ok_or_else(|| Error::usage("no share selected: use <share> first"))
    }

    pub async fn use_share(&mut self, name: &str) -> Result<()> {
        self.in_flight = true;
        let r = self.tree(name).await;
        self.in_flight = false;
        r?;
        self.share = Some(name.to_string());
        self.cwd.clear();
        Ok(())
    }

    pub async fn cd(&mut self, arg: &str) -> Result<()> {
        let share = self.selected()?;
        let target = self.resolve(arg);
        if target.len() <= self.cwd.len() && self.cwd[..target.len()] == target[..] {
            self.cwd = target;
            return Ok(());
        }
        self.in_flight = true;
        let r = self.check_dir(&share, &target).await;
        self.in_flight = false;
        r?;
        self.cwd = target;
        Ok(())
    }

    /// CREATE (attributes only, directory) and CLOSE in one compound: 2 requests, 1 flight.
    async fn check_dir(&mut self, share: &str, target: &[String]) -> Result<()> {
        let tree = self.tree(share).await?;
        let what = display_path(target);
        let open = CreateDisposition::FileOpen;
        open_close(&self.conn, &tree, target, open, 1, 0, &what)
            .await
            .map(|_| ())
    }

    /// Asks the server, sorts by name, refreshes the cache.
    pub async fn list(&mut self, dir: &[String]) -> Result<Arc<Vec<Entry>>> {
        let share = self.selected()?;
        self.in_flight = true;
        let r = self.list_inner(&share, dir).await;
        self.in_flight = false;
        let mut entries = r?;
        retain_real(&mut entries);
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let entries = Arc::new(entries);
        self.cache.put(&share, dir, entries.clone());
        Ok(entries)
    }

    async fn list_inner(&mut self, share: &str, dir: &[String]) -> Result<Vec<Entry>> {
        let tree = self.tree(share).await?;
        let max_transact = self.conn.params().map_or(UNIT, |p| p.max_transact_size);
        let (b, n) = plan(max_transact, self.conn.credits());
        let what = display_path(dir);
        list_dir(&self.conn, tree.tree_id, tree_path(&tree, dir), b, n, &what).await
    }

    /// The cache, else `list`. For completion: a listing the server refused (access denied, not
    /// found, ...) is remembered as empty, so the next Tab costs nothing; `refresh` and the next
    /// `ls` of that directory replace it. A lost connection is not remembered.
    pub async fn list_cached(&mut self, dir: &[String]) -> Result<Arc<Vec<Entry>>> {
        let share = self.selected()?;
        if let Some(e) = self.cache.get(&share, dir) {
            return Ok(e);
        }
        self.ensure_connected().await?;
        let r = self.list(dir).await;
        if let Err(e) = &r
            && !e.is_lost()
        {
            self.cache.put(&share, dir, Arc::new(Vec::new()));
        }
        r
    }

    /// Asks for the share list (srvsvc on `IPC$`, kept in the tree map) and keeps it.
    pub async fn shares(&mut self) -> Result<Arc<Vec<Share>>> {
        self.in_flight = true;
        let r = self.shares_inner().await;
        self.in_flight = false;
        let list = Arc::new(r?);
        self.shares = Some(list.clone());
        Ok(list)
    }

    async fn shares_inner(&mut self) -> Result<Vec<Share>> {
        let ipc = self.tree("IPC$").await?;
        let tid = ipc.tree_id;
        let mut create = open_request("srvsvc".into(), 0);
        create.desired_access =
            FileAccessMask::new(FileAccessMask::FILE_READ_DATA | FileAccessMask::FILE_WRITE_DATA);
        create.create_options = 0;
        let f = ok_frame(
            self.conn.execute(Command::Create, &create, Some(tid)).await,
            "share list",
        )?;
        let fid = CreateResponse::unpack(&mut ReadCursor::new(&f.body))
            .map_err(|_| malformed())?
            .file_id;
        let r = enum_shares(&self.conn, tid, fid).await;
        if let Err(e) = &r
            && e.is_lost()
        {
            return r;
        }
        let closed = self
            .conn
            .execute(
                Command::Close,
                &CloseRequest {
                    flags: 0,
                    file_id: fid,
                },
                Some(tid),
            )
            .await;
        let list = r?;
        closed.map_err(|e| classify(e, "share list"))?;
        Ok(list)
    }

    /// The access the server reports for each of `names`, in order (`probe_access`): up to
    /// ACCESS_CHAIN shares per compound, as many compounds in flight as the credits pay for. A
    /// share without an answer gets None. A connection lost on the way is not an error of the
    /// call: the answers so far come back with it, so the list can still be shown.
    pub async fn access(
        &mut self,
        names: &[String],
    ) -> Result<(Vec<Option<&'static str>>, Option<Error>)> {
        self.ensure_connected().await?;
        self.in_flight = true;
        let r = self.access_inner(names).await;
        self.in_flight = false;
        r
    }

    async fn access_inner(
        &mut self,
        names: &[String],
    ) -> Result<(Vec<Option<&'static str>>, Option<Error>)> {
        let credits = usize::from(self.conn.credits());
        let per = (credits / 2).clamp(1, ACCESS_CHAIN);
        let width = (credits / (2 * per)).max(1);
        let mut out = vec![None; names.len()];
        let mut chunks = names.chunks(per).enumerate();
        let mut set = JoinSet::new();
        loop {
            while set.len() < width
                && let Some((i, chunk)) = chunks.next()
            {
                let (conn, chunk) = (self.conn.clone(), chunk.to_vec());
                set.spawn(async move { (i, probe_access(&conn, &chunk).await) });
            }
            let Some(j) = set.join_next().await else {
                break;
            };
            match j {
                Ok((i, Ok(v))) => out[i * per..i * per + v.len()].copy_from_slice(&v),
                Ok((_, Err(e))) if e.is_lost() => return Ok((out, Some(e))),
                _ => {}
            }
        }
        Ok((out, None))
    }

    pub fn known_shares(&self) -> Option<Arc<Vec<Share>>> {
        self.shares.clone()
    }

    /// Empties the cache and forgets the share list. Sends nothing.
    pub fn refresh(&mut self) {
        self.cache.clear();
        self.shares = None;
    }

    /// Stores a walk's listing on the selected share (`Cache::feed`). Sends nothing.
    pub fn feed(&mut self, fed: &mut HashMap<Key, bool>, dir: &[String], entries: Arc<Vec<Entry>>) {
        if let Some(share) = &self.share {
            self.cache.feed(fed, share, dir, entries);
        }
    }

    /// Drops cached listings on the selected share (`Cache::invalidate`). Sends nothing.
    pub fn invalidate(&mut self, dir: &[String], subtree: bool) {
        if let Some(share) = &self.share {
            self.cache.invalidate(share, dir, subtree);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The last cleanup made a new connection after generation `g`: a lost-connection error from
    /// before it must not mark that connection dead.
    pub fn replaced_by_cleanup_since(&self, g: u64) -> bool {
        self.cleanup_gen > g
    }

    /// The start of every command that uses a `Remote` (`remote()`, the work, `finish()`):
    /// the tree of the selected share (a dead connection is replaced here), sizes, a fresh channel.
    pub async fn remote(&mut self) -> Result<Remote> {
        let share = self.selected()?;
        self.in_flight = true;
        let tree = match self.tree(&share).await {
            Ok(t) => t,
            Err(e) => {
                self.in_flight = false;
                return Err(e);
            }
        };
        let p = self.conn.params();
        let (read, write, transact) = p.map_or((UNIT, UNIT, UNIT), |p| {
            (p.max_read_size, p.max_write_size, p.max_transact_size)
        });
        let (walk_buf, _) = plan(transact.min(WALK_BUFFER), self.conn.credits());
        let (tx, rx) = mpsc::channel(1);
        self.alive = Some(rx);
        self.dirty = Arc::default();
        self.remote_gen = self.generation;
        Ok(Remote {
            conn: self.conn.clone(),
            tree,
            walk_buf,
            temps: self.temps.clone(),
            writer: self.writer.clone(),
            dirty: self.dirty.clone(),
            alive: tx,
            small: SMALL.min(u64::from(read)).min(u64::from(write)),
        })
    }

    /// The end of every command that used a `Remote`: drops it; if temporary files may be left,
    /// the cleanup (on a new connection too, unless the link went silent) and what is still left
    /// goes into the error; `in_flight` cleared; a dirty `Remote` has its connection replaced.
    pub async fn finish<T>(&mut self, remote: Remote, r: Result<T>) -> Result<T> {
        drop(remote);
        let mut r = r;
        let silent = matches!(&r, Err(e) if e.kind == (Kind::ConnectionLost { silent: true }));
        // A connection known to be gone gets no 2 s of cleanup on it (after a silent link that
        // would only add 2 s to the failure); the cleanup goes straight to step 3, or names the
        // files.
        if matches!(&r, Err(e) if e.is_lost()) {
            self.mark_dead();
        }
        let (left, gone) = self.cleanup_inner(!silent).await;
        if let Err(e) = &mut r {
            for g in gone {
                e.message.push_str(&format!(
                    "; the temporary file is gone, so {g} is probably complete; check with ls"
                ));
            }
            if !left.is_empty() {
                e.message.push_str(&format!(
                    "; left on the server: {}; remove with rm once the server has let go of them",
                    left.join(", ")
                ));
                e.left = left;
            }
        }
        self.in_flight = false;
        if self.dirty.load(Ordering::SeqCst) && self.generation == self.remote_gen {
            self.mark_dead();
        }
        r
    }

    /// After a failure or an interrupt: waits for the command's `Remote` clones and `Alive`s,
    /// then ends the writer with `abort()` and deletes every registered temporary file, on this
    /// connection within `CLEANUP_WAIT`, else (if `reconnect`) once on a new connection. Returns
    /// what is still left, as display paths.
    pub async fn cleanup(&mut self, reconnect: bool) -> Vec<String> {
        self.cleanup_inner(reconnect).await.0
    }

    async fn cleanup_inner(&mut self, reconnect: bool) -> (Vec<String>, Vec<String>) {
        // 1. Nothing of the command runs any more, on the wire or on the local disk.
        if let Some(mut rx) = self.alive.take() {
            while rx.recv().await.is_some() {}
        }
        let writer = self.writer.lock().await.take();
        if writer.is_none() && self.temps.lock().unwrap().is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut gone = Vec::new();
        let share = self.share.clone().unwrap_or_default();
        let tree = self.trees.get(&share.to_lowercase()).cloned();
        // 2. On the connection that carried the upload.
        let mut again = true;
        if let (false, Some(tree)) = (self.conn.is_disconnected(), tree) {
            let conn = self.conn.clone();
            let temps = self.temps.clone();
            let step = async {
                if let Some(w) = writer {
                    let _ = w.abort().await;
                }
                delete_all(&conn, &tree, &temps, &mut gone).await
            };
            again = tokio::time::timeout(CLEANUP_WAIT, step)
                .await
                .unwrap_or(true);
            if again {
                self.mark_dead();
            }
        } else {
            drop(writer);
        }
        // 3. Once on a new connection, deletes only.
        if again && reconnect && !self.temps.lock().unwrap().is_empty() {
            self.mark_dead();
            if let Ok(tree) = self.tree(&share).await {
                self.cleanup_gen = self.generation;
                self.in_flight = true;
                let conn = self.conn.clone();
                let temps = self.temps.clone();
                delete_all(&conn, &tree, &temps, &mut gone).await;
                self.in_flight = false;
            }
        }
        (self.take_left(), gone)
    }

    /// Empties the registry and returns what was in it as display paths: a file that the cleanup
    /// could not delete is reported once, by the command that left it, and not tried again by
    /// later commands (which could be on another share or connection).
    pub fn take_left(&self) -> Vec<String> {
        self.temps
            .lock()
            .unwrap()
            .drain(..)
            .map(|t| display_path(&t.path))
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use smb2::msg::directory_info::{DirectoryInfoEntry, pack_entries};
    use smb2::pack::FileTime;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_paths() {
        let cwd = v(&["a", "b"]);
        assert_eq!(resolve_path(&cwd, "."), v(&["a", "b"]));
        assert_eq!(resolve_path(&cwd, ".."), v(&["a"]));
        assert_eq!(resolve_path(&[], ".."), v(&[]));
        assert_eq!(resolve_path(&cwd, "../../../x"), v(&["x"]));
        assert_eq!(resolve_path(&cwd, "/x"), v(&["x"]));
        assert_eq!(resolve_path(&cwd, "\\x\\y"), v(&["x", "y"]));
        assert_eq!(resolve_path(&cwd, "c/d\\e"), v(&["a", "b", "c", "d", "e"]));
        assert_eq!(resolve_path(&cwd, "c/"), v(&["a", "b", "c"]));
        assert_eq!(resolve_path(&cwd, "./-f"), v(&["a", "b", "-f"]));
        assert_eq!(resolve_path(&cwd, "/"), v(&[]));
        assert_eq!(resolve_path(&cwd, ""), v(&["a", "b"]));
    }

    #[test]
    fn wire_paths() {
        assert_eq!(wire_path(None, &v(&["a", "b"])), "a\\b");
        assert_eq!(wire_path(None, &[]), "");
        assert_eq!(wire_path(Some(("srv", "dfs")), &[]), "srv\\dfs");
        assert_eq!(
            wire_path(Some(("srv", "dfs")), &v(&["a", "b"])),
            "srv\\dfs\\a\\b"
        );
    }

    fn proto(status: NtStatus) -> smb2::Error {
        smb2::Error::Protocol {
            status,
            command: Command::Create,
        }
    }

    #[test]
    fn classify_statuses() {
        let cases = [
            (NtStatus::OBJECT_NAME_NOT_FOUND, Kind::NotFound),
            (NtStatus::OBJECT_PATH_NOT_FOUND, Kind::NotFound),
            (NtStatus::BAD_NETWORK_NAME, Kind::NotFound),
            (NtStatus::ACCESS_DENIED, Kind::AccessDenied),
            (NtStatus::NOT_A_DIRECTORY, Kind::NotADirectory),
            (NtStatus::FILE_IS_A_DIRECTORY, Kind::IsADirectory),
            (NtStatus::OBJECT_NAME_COLLISION, Kind::AlreadyExists),
            (NtStatus::DISK_FULL, Kind::DiskFull),
            (NtStatus::OBJECT_NAME_INVALID, Kind::InvalidName),
            (NtStatus::LOGON_FAILURE, Kind::Auth),
            (NtStatus::INVALID_PARAMETER, Kind::Other),
        ];
        for (status, kind) in cases {
            let e = classify(proto(status), "/x");
            assert_eq!(e.kind, kind, "{status}");
            assert!(e.message.starts_with("/x: "), "{}", e.message);
        }
        assert_eq!(
            classify(proto(NtStatus::OBJECT_NAME_NOT_FOUND), "/x").message,
            "/x: not found"
        );
        let e = classify(proto(NtStatus::INVALID_PARAMETER), "/x");
        assert!(
            e.message.contains("STATUS_INVALID_PARAMETER"),
            "{}",
            e.message
        );
        let e = classify(smb2::Error::Disconnected, "/x");
        assert_eq!(e.kind, Kind::ConnectionLost { silent: false });
        assert!(e.message.starts_with("connection lost"));
        let e = classify(smb2::Error::Timeout, "/x");
        assert_eq!(e.kind, Kind::ConnectionLost { silent: true });
        assert!(e.message.contains("--timeout"));
        let e = classify(
            smb2::Error::ServerUnresponsive {
                silent_for: Duration::from_secs(15),
            },
            "/x",
        );
        assert_eq!(e.kind, Kind::ConnectionLost { silent: true });
        assert_eq!(
            e.message,
            "connection lost: no answer from the server for 15 s"
        );
        let e = classify(smb2::Error::SessionExpired, "/x");
        assert_eq!(e.kind, Kind::ConnectionLost { silent: false });
        assert!(e.message.contains("ended the session"), "{}", e.message);
    }

    #[test]
    fn classify_stage2() {
        let e = classify(proto(NtStatus::DIRECTORY_NOT_EMPTY), "/d");
        assert_eq!((e.kind, e.kind.name()), (Kind::NotEmpty, "not_empty"));
        assert_eq!(e.message, "/d: directory not empty");
        let e = classify(proto(NtStatus::SHARING_VIOLATION), "/f");
        assert_eq!(e.kind, Kind::Other);
        assert!(e.message.contains("in use"), "{}", e.message);
        let e = classify(proto(NtStatus::CANNOT_DELETE), "/f");
        assert_eq!(e.kind, Kind::Other);
        assert!(e.message.contains("cannot be deleted"), "{}", e.message);
        assert_eq!(KIND_NAMES.len(), 15);
        assert_eq!(Error::new(Kind::NotEmpty, "x").exit_code(false), 1);
        let e = Error::new(Kind::Other, "x");
        assert!(e.path.is_none() && e.left.is_empty());
    }

    #[test]
    fn cache_invalidate() {
        let mut c = Cache::new(100);
        for d in [&["a"][..], &["a", "b"], &["a", "b", "c"], &["ab"], &[]] {
            c.put("s", &v(d), listing(1));
        }
        c.put("t", &v(&["a"]), listing(1));
        c.invalidate("s", &v(&["a", "b"]), false);
        assert!(c.get("s", &v(&["a", "b"])).is_none(), "the exact entry");
        assert!(c.get("s", &v(&["a", "b", "c"])).is_some());
        c.put("s", &v(&["a", "b"]), listing(1));
        c.invalidate("s", &v(&["A"]), true);
        for d in [&["a"][..], &["a", "b"], &["a", "b", "c"]] {
            assert!(
                c.get("s", &v(d)).is_none(),
                "{d:?}: subtree, keys ignore case"
            );
        }
        assert!(
            c.get("s", &v(&["ab"])).is_some(),
            "a sibling with the same prefix stays"
        );
        assert!(c.get("s", &v(&[])).is_some());
        assert!(c.get("t", &v(&["a"])).is_some(), "other shares untouched");
        c.invalidate("S", &[], true);
        assert!(c.get("s", &v(&["ab"])).is_none() && c.get("s", &v(&[])).is_none());
        assert!(c.get("t", &v(&["a"])).is_some());
        assert_eq!(c.total, 2, "one listing of one entry: 2 units");
        assert_eq!(c.order.len(), 1);
    }

    #[test]
    fn valid_names() {
        for bad in [
            "a:b",
            "x?",
            "star*",
            "quote\"",
            "back\\slash",
            "dot.",
            "space ",
            "c\u{1}c",
            "",
            "n\0ul",
            "p|pe",
            "<x>",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        let fixtures = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {}/testenv/samba/names.sh; printf '%s\\n' \"${{CJK_NAMES[@]}}\" \"${{SPECIAL_NAMES[@]}}\"",
                env!("CARGO_MANIFEST_DIR")
            ))
            .output()
            .unwrap();
        let names = String::from_utf8(fixtures.stdout).unwrap();
        let names: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
        assert!(names.len() >= 20, "{names:?}");
        for n in
            names
                .iter()
                .copied()
                .chain(["a\u{F022}b", ".rsmb-0123456789abcdef.part", ".hidden"])
        {
            assert!(valid_name(n), "{n:?}");
        }
        let e = crate_path(&v(&["ok", "a:b"])).unwrap_err();
        assert_eq!(e.kind, Kind::InvalidName);
        assert!(e.message.contains("'a:b'"), "{}", e.message);
        assert_eq!(crate_path(&v(&["a", "b c"])).unwrap(), "a/b c");
    }

    #[test]
    fn registry_refuses_a_second_use() {
        let reg = StdMutex::new(Vec::new());
        let t = v(&["d", ".rsmb-0000000000000000.part"]);
        register(&reg, &t, &v(&["d", "x"])).unwrap();
        let e = register(&reg, &t, &v(&["d", "y"])).unwrap_err();
        assert_eq!(e.kind, Kind::Other);
        assert!(e.message.contains("same temporary name"), "{}", e.message);
        register(
            &reg,
            &v(&["e", ".rsmb-0000000000000000.part"]),
            &v(&["e", "x"]),
        )
        .unwrap();
        unregister(&reg, &t);
        register(&reg, &t, &v(&["d", "y"])).unwrap();
        assert_eq!(reg.lock().unwrap().len(), 2);
    }

    #[test]
    fn cleanup_goes_to_a_new_connection() {
        let new = Verdict::NewConnection;
        assert_eq!(cleanup_verdict(None), new, "the 2 s ran out");
        assert_eq!(cleanup_verdict(Some(&Err(smb2::Error::Disconnected))), new);
        assert_eq!(
            cleanup_verdict(Some(&Err(proto(NtStatus::SHARING_VIOLATION)))),
            new
        );
        let starved = smb2::Error::CreditStarvation {
            needed: 3,
            available: 0,
            waited: Duration::from_secs(1),
        };
        assert_eq!(cleanup_verdict(Some(&Err(starved))), new);
        assert_eq!(cleanup_verdict(Some(&Err(smb2::Error::Timeout))), new);
        assert_eq!(cleanup_verdict(Some(&Ok(()))), Verdict::Gone);
        assert_eq!(
            cleanup_verdict(Some(&Err(proto(NtStatus::OBJECT_NAME_NOT_FOUND)))),
            Verdict::Gone
        );
        assert_eq!(
            cleanup_verdict(Some(&Err(proto(NtStatus::ACCESS_DENIED)))),
            Verdict::Stays,
            "a second connection would get the same answer"
        );
    }

    #[test]
    fn rename_errors_have_one_wording() {
        let collision = || classify(proto(NtStatus::OBJECT_NAME_COLLISION), "/d/x");
        let e = rename_error(collision(), "/d/x", false, Some(true));
        assert_eq!(e.kind, Kind::IsADirectory);
        assert!(e.message.contains("write '/d/x/'"), "{}", e.message);
        let e = rename_error(collision(), "/d/x", true, Some(true));
        assert_eq!(e.kind, Kind::IsADirectory, "a directory also with -f");
        let e = rename_error(collision(), "/d/x", false, Some(false));
        assert_eq!(e.kind, Kind::AlreadyExists);
        assert_eq!(e.message, "/d/x: exists on the server; -f replaces it");
        let e = rename_error(collision(), "/d/x", true, Some(false));
        assert_eq!(e.kind, Kind::Other);
        assert!(e.message.contains("did not replace"), "{}", e.message);
        let denied = classify(proto(NtStatus::ACCESS_DENIED), "/d/x");
        let e = rename_error(denied.clone(), "/d/x", true, None);
        assert_eq!(e.kind, Kind::AccessDenied);
        assert!(e.message.contains("refused to replace it"), "{}", e.message);
        let e = rename_error(denied, "/d/x", false, None);
        assert_eq!(
            e.message, "/d/x: access denied",
            "without -f the classified error"
        );
    }

    #[test]
    fn guest_refusal_names_the_flag() {
        let e = classify(
            smb2::Error::GuestSessionRefused {
                username: "nosuchuser".into(),
                anonymous: false,
            },
            "login",
        );
        assert_eq!(e.kind, Kind::Auth);
        assert_eq!(e.kind.name(), "auth");
        assert!(e.message.contains("-u Guest --no-pass"), "{}", e.message);
        assert!(e.message.contains("'nosuchuser'"));
    }

    #[test]
    fn exit_codes() {
        let all = [
            Kind::NotFound,
            Kind::AccessDenied,
            Kind::NotADirectory,
            Kind::IsADirectory,
            Kind::AlreadyExists,
            Kind::NotEmpty,
            Kind::DiskFull,
            Kind::InvalidName,
            Kind::Auth,
            Kind::Connect,
            Kind::ConnectionLost { silent: false },
            Kind::ConnectionLost { silent: true },
            Kind::Interrupted,
            Kind::LocalIo,
            Kind::Usage,
            Kind::Other,
        ];
        for k in all {
            let e = Error::new(k, "x");
            let (start, later) = match k {
                Kind::Usage => (2, 2),
                Kind::Interrupted => (130, 130),
                _ => (3, 1),
            };
            assert_eq!(e.exit_code(true), start, "{k:?} at start-up");
            assert_eq!(e.exit_code(false), later, "{k:?} later");
            assert!(KIND_NAMES.contains(&k.name()));
        }
    }

    #[test]
    fn listing_plan() {
        let mib8 = 8 << 20;
        assert_eq!(plan(mib8, 513), (8_323_072, 2));
        assert_eq!(plan(65_536, 16), (65_536, 13));
        assert_eq!(plan(mib8, 32), (1_900_544, 1));
        assert_eq!(plan(1 << 20, 8192), (1_048_576, 13));
        assert_eq!(plan(65_536, 4), (65_536, 1));
        for transact in [mib8, 65_536, 1 << 20] {
            for credits in 4..=600u16 {
                let (b, n) = plan(transact, credits);
                let q = b / UNIT;
                assert_eq!(b % UNIT, 0);
                assert!(b <= transact.max(UNIT));
                let charge = 2 + n as u32 * q + 1;
                assert!(
                    charge <= u32::from(credits),
                    "({transact}, {credits}) -> ({b}, {n})"
                );
                assert!(n as u32 * b + PROBE <= (16 << 20) - (64 << 10));
            }
        }
        assert_eq!(plan(65_536, 1), (65_536, 1));
    }

    #[test]
    fn query_outcomes() {
        assert_eq!(query_outcome(NtStatus::SUCCESS, true), Outcome::Entries);
        assert_eq!(query_outcome(NtStatus::SUCCESS, false), Outcome::Entries);
        assert_eq!(query_outcome(NtStatus::NO_MORE_FILES, true), Outcome::End);
        assert_eq!(query_outcome(NtStatus::NO_MORE_FILES, false), Outcome::End);
        assert_eq!(query_outcome(NtStatus::NO_SUCH_FILE, true), Outcome::End);
        assert_eq!(query_outcome(NtStatus::NO_SUCH_FILE, false), Outcome::Fail);
        assert_eq!(query_outcome(NtStatus::ACCESS_DENIED, true), Outcome::Fail);
        assert_eq!(
            query_outcome(NtStatus::INVALID_PARAMETER, false),
            Outcome::Fail
        );
    }

    fn crate_entry(
        name: &str,
        size: u64,
        mtime: u64,
        attributes: u32,
        id: u64,
    ) -> DirectoryInfoEntry {
        DirectoryInfoEntry {
            file_index: 0,
            creation_time: FileTime(1),
            last_access_time: FileTime(2),
            last_write_time: FileTime(mtime),
            change_time: FileTime(3),
            end_of_file: size,
            allocation_size: size,
            file_attributes: attributes,
            ea_size: 0,
            file_id: Some(id),
            file_name: name.to_string(),
        }
    }

    #[test]
    fn entries_match_the_crates_packer() {
        let long = "x".repeat(250);
        let names = [
            "简体中文",
            "東京　大阪",
            "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}",
            "𠮷野家",
            "😀 smile.txt",
            "  two leading spaces.txt",
            long.as_str(),
            "a",
        ];
        let want: Vec<Entry> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Entry {
                name: n.to_string(),
                size: (i as u64) << 33 | 7,
                mtime: 133_000_000_000_000_000 + i as u64,
                attributes: if i % 2 == 0 { 0x10 } else { 0x20 },
                file_id: 0xdead_beef_0000_0000 + i as u64,
            })
            .collect();
        let packed: Vec<DirectoryInfoEntry> = want
            .iter()
            .map(|e| crate_entry(&e.name, e.size, e.mtime, e.attributes, e.file_id))
            .collect();
        let buf = pack_entries(CLASS, &packed).unwrap();
        let mut got = Vec::new();
        parse_entries(&buf, &mut got).unwrap();
        assert_eq!(got, want);
    }

    /// One entry by hand: fixed part with the fields we read, then the UTF-16 units.
    fn raw_entry(units: &[u16], last: bool) -> Vec<u8> {
        let mut e = vec![0u8; ENTRY_FIXED];
        let len = (ENTRY_FIXED + units.len() * 2).next_multiple_of(8);
        if !last {
            e[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        }
        e[40..48].copy_from_slice(&5u64.to_le_bytes());
        e[60..64].copy_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        for u in units {
            e.extend(u.to_le_bytes());
        }
        e.resize(len, 0);
        e
    }

    #[test]
    fn unpaired_surrogate_is_replaced() {
        let a: Vec<u16> = "abc".encode_utf16().collect();
        let bad = vec![0x61, 0xD800, 0x62];
        let c: Vec<u16> = "東京".encode_utf16().collect();
        let mut buf = raw_entry(&a, false);
        buf.extend(raw_entry(&bad, false));
        buf.extend(raw_entry(&c, true));
        let mut got = Vec::new();
        parse_entries(&buf, &mut got).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].name, "abc");
        assert_eq!(got[1].name, "a\u{FFFD}b");
        assert_eq!(got[2].name, "東京");
        assert_eq!(got[2].size, 5);
    }

    #[test]
    fn truncated_entry_is_an_error() {
        let a: Vec<u16> = "abcdef".encode_utf16().collect();
        let mut buf = raw_entry(&a, true);
        buf[60..64].copy_from_slice(&1000u32.to_le_bytes());
        let mut got = Vec::new();
        assert!(parse_entries(&buf, &mut got).is_err());
    }

    /// A hostile server: entries that overlap (`next` shorter than the entry) would be parsed
    /// again and again from a small buffer (final review, finding 6).
    #[test]
    fn overlapping_entries_are_malformed() {
        let a: Vec<u16> = "abcdef".encode_utf16().collect();
        for next in [1u32, 8, ENTRY_FIXED as u32, ENTRY_FIXED as u32 + 11] {
            let mut buf = raw_entry(&a, false);
            buf[0..4].copy_from_slice(&next.to_le_bytes());
            buf.extend(vec![0u8; 4096]);
            let mut got = Vec::new();
            assert!(parse_entries(&buf, &mut got).is_err(), "next = {next}");
            assert!(got.len() <= 1, "next = {next}");
        }
        // The shortest honest `next`, the end of the name, still parses.
        let mut buf = raw_entry(&a, false);
        buf[0..4].copy_from_slice(&((ENTRY_FIXED + 12) as u32).to_le_bytes());
        buf.truncate(ENTRY_FIXED + 12);
        buf.extend(raw_entry(&a, true));
        let mut got = Vec::new();
        parse_entries(&buf, &mut got).unwrap();
        assert_eq!(got.len(), 2);
    }

    /// SUCCESS with nothing in the buffer, where the listing needs progress, is malformed: the
    /// loop that asks again would never end.
    #[test]
    fn empty_success_is_malformed() {
        let mut h = smb2::msg::header::Header::new_request(Command::QueryDirectory);
        h.status = NtStatus::SUCCESS;
        let mut body = vec![0u8; 8];
        body[0] = 9;
        body[2] = 72; // the buffer follows the 8 bytes, after the 64 byte header
        let f = Frame {
            header: h,
            raw: Vec::new(),
            body,
        };
        let mut out = Vec::new();
        let r = take_queries(vec![Ok(f)].into_iter(), false, &mut out, "x");
        assert!(r.is_err(), "{r:?}");
    }

    /// The share list is limited while it is collected, and a fragment that adds nothing and is
    /// not the last one cannot go on for ever.
    #[test]
    fn share_fragments_are_bounded() {
        let mut stub = Vec::new();
        assert!(add_fragment(&mut stub, &[1; 1000], false).is_ok());
        assert!(add_fragment(&mut stub, &[], false).is_err(), "no progress");
        assert!(
            add_fragment(&mut stub, &[], true).is_ok(),
            "an empty last one ends it"
        );
        assert!(
            add_fragment(&mut stub, &vec![1; STUB_MAX], false).is_err(),
            "too large"
        );
        assert!(stub.len() <= STUB_MAX);
    }

    /// The masks Samba 4.23.8 sends (`share`, `readonly`) and the generic bits.
    #[test]
    fn share_access_names() {
        assert_eq!(access_name(0x001f_01ff), "rw");
        assert_eq!(
            access_name(0x001f_00a9),
            "r",
            "DELETE and WRITE_DAC are not write"
        );
        assert_eq!(access_name(0x0012_0116), "w");
        assert_eq!(access_name(0x0010_0000), "none");
        assert_eq!(access_name(0x1000_0000), "rw");
        assert_eq!(access_name(0x8000_0000), "r");
    }

    #[test]
    fn safe_escapes_controls_and_bidi() {
        assert_eq!(safe("plain 東京 😀.txt"), "plain 東京 😀.txt");
        assert!(matches!(safe("abc"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(safe("a\u{9b}31mX"), "a\\u{9b}31mX");
        assert_eq!(
            safe("\x1b[2J\n\t\x7f\0"),
            "\\u{1b}[2J\\u{a}\\u{9}\\u{7f}\\u{0}"
        );
        assert_eq!(safe("b\u{202e}exe"), "b\\u{202e}exe");
        for c in [
            '\u{202a}', '\u{2066}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{61c}', '\u{80}',
            '\u{9f}',
        ] {
            assert_ne!(safe(&c.to_string()), c.to_string(), "{c:?}");
        }
        // Not escaped: a zero-width joiner (emoji, scripts), U+00A0, and a backslash.
        assert_eq!(safe("\u{200d}\u{a0}\\"), "\u{200d}\u{a0}\\");
    }

    #[test]
    fn share_list_overflow() {
        assert!(share_pdu(&vec![0; 65_511]).is_ok());
        let e = share_pdu(&vec![0; 65_512]).unwrap_err();
        assert_eq!(e.kind, Kind::Other);
        assert_eq!(
            e.message,
            "the share list is too large for rsmb (65512 bytes; the limit is 65,511)"
        );
    }

    #[test]
    fn proxy_urls() {
        assert_eq!(
            parse_proxy("socks5://10.0.0.1:1080").unwrap(),
            Proxy {
                host: "10.0.0.1".into(),
                port: 1080,
                auth: None
            }
        );
        assert_eq!(
            parse_proxy("socks5://u:p@proxy.example:1081").unwrap(),
            Proxy {
                host: "proxy.example".into(),
                port: 1081,
                auth: Some(("u".into(), "p".into()))
            }
        );
        let p = parse_proxy("socks5://us%40er:p%3Ass%2F%25@[::1]:1080").unwrap();
        assert_eq!(
            p,
            Proxy {
                host: "::1".into(),
                port: 1080,
                auth: Some(("us@er".into(), "p:ss/%".into()))
            }
        );
        assert_eq!(parse_proxy("socks5://[fe80::1]:9").unwrap().host, "fe80::1");
        // socks5h means the same; one trailing / is dropped.
        assert_eq!(
            parse_proxy("socks5h://h:1").unwrap(),
            parse_proxy("socks5://h:1").unwrap()
        );
        assert_eq!(
            parse_proxy("socks5://h:1/").unwrap(),
            parse_proxy("socks5://h:1").unwrap()
        );
        assert_eq!(parse_proxy("socks5://h:65535").unwrap().port, 65535);
        // 255 bytes after decoding are accepted, 256 are not; empty halves are not.
        let n255 = "a".repeat(255);
        let n256 = "a".repeat(256);
        let enc255 = "%61".repeat(255);
        for ok in [
            format!("socks5://{n255}:pw@h:1"),
            format!("socks5://u:{n255}@h:1"),
            format!("socks5://u:{enc255}@h:1"),
        ] {
            assert!(parse_proxy(&ok).is_ok(), "{ok}");
        }
        let long = [
            format!("socks5://{n256}:pw@h:1"),
            format!("socks5://u:{n256}@h:1"),
            format!("socks5://u:{enc255}%61@h:1"),
        ];
        let refused = [
            "socks5://::1:1080",
            "socks5://[::1]",
            "socks5://[::1:1080",
            "socks5://[host]:1080",
            "socks5://u:%zz@host:1",
            "socks5://u:%4@host:1",
            "socks5://u:%ff@host:1",
            "socks5://host",
            "socks5://host:",
            "socks5://h:1//",
            "socks5://h:1/x",
            "socks5://h/x:1",
            "socks5://:pw@h:1",
            "socks5://user:@h:1",
            "socks5://u@host:1080",
            "SOCKS5://h:1",
            "socks4://h:1",
            "socks4a://h:1",
            "socks://h:1",
            "http://host:1080",
            "https://h:1",
            "h:1",
            "",
            "socks5://h:0",
            "socks5://h:65536",
            "socks5://h:+1",
            // The forms of finding 5: no part of them may show in the message.
            "socks5://user1:secretpw@host",
            "socks5://alice:hunter2",
            "user1:secretpw@host:1",
            "socks5://user1:sec%zzret@host:1",
        ];
        for bad in refused
            .iter()
            .copied()
            .chain(long.iter().map(String::as_str))
        {
            let e = parse_proxy(bad).unwrap_err();
            assert_eq!(e.kind, Kind::Usage, "{bad}");
            assert_eq!(e.message, PROXY_USAGE, "{bad}: always the same message");
        }
        assert!(PROXY_USAGE.contains("expected socks5://") && PROXY_USAGE.contains("SOCKS5 only"));
        for secret in ["user1", "secretpw", "alice", "hunter2", "sec%zzret"] {
            assert!(!PROXY_USAGE.contains(secret), "{secret}");
        }
    }

    fn listing(n: usize) -> Arc<Vec<Entry>> {
        Arc::new(
            (0..n)
                .map(|i| Entry {
                    name: format!("f{i}"),
                    size: 0,
                    mtime: 0,
                    attributes: 0,
                    file_id: 0,
                })
                .collect(),
        )
    }

    #[test]
    fn cache_cap() {
        // A listing of 4 entries costs 5 units.
        let mut c = Cache::new(10);
        c.put("s", &v(&["a"]), listing(4));
        c.put("s", &v(&["b"]), listing(4));
        c.put("s", &v(&["c"]), listing(4));
        assert!(
            c.get("s", &v(&["a"])).is_none(),
            "the oldest is dropped at the cap"
        );
        assert!(c.get("s", &v(&["b"])).is_some());
        assert!(c.get("s", &v(&["c"])).is_some());
        assert_eq!(c.total, 10);
        c.put("s", &v(&["big"]), listing(10));
        assert!(
            c.get("s", &v(&["big"])).is_none(),
            "a listing above the cap is not stored"
        );
        assert_eq!(c.total, 10);
        assert!(c.get("s", &v(&["b"])).is_some() && c.get("s", &v(&["c"])).is_some());
        c.put("s", &v(&["b"]), listing(2));
        assert_eq!(c.get("s", &v(&["b"])).unwrap().len(), 2);
        assert_eq!(c.total, 8);
        c.clear();
        assert_eq!(c.total, 0);
        assert!(c.order.is_empty());
        assert!(c.get("s", &v(&["c"])).is_none());
    }

    #[test]
    fn cache_units_and_order() {
        let mut c = Cache::new(100);
        c.put("s", &v(&["a"]), listing(3));
        assert_eq!(c.total, 4, "a listing costs len + 1");
        c.put("s", &v(&["e"]), listing(0));
        assert_eq!(c.total, 5, "an empty listing costs 1");
        // 95 empty listings fill the cap; the next one drops the oldest, "a".
        for i in 0..95 {
            c.put("s", &v(&[&format!("x{i}")]), listing(0));
        }
        assert_eq!(c.total, 100);
        c.put("s", &v(&["last"]), listing(0));
        assert!(c.get("s", &v(&["a"])).is_none(), "the oldest goes first");
        assert!(c.get("s", &v(&["e"])).is_some());
        assert_eq!(c.total, 97);
        // A put over the same key and an invalidate leave no stale index entry.
        c.put("s", &v(&["e"]), listing(2));
        c.put("s", &v(&["e"]), listing(1));
        c.invalidate("s", &v(&["x0"]), false);
        assert_eq!(c.order.len(), c.map.len());
        assert_eq!(
            c.total,
            c.map.values().map(|(l, _)| l.len() + 1).sum::<usize>()
        );
        c.invalidate("s", &[], true);
        assert!(c.map.is_empty() && c.order.is_empty() && c.total == 0);
        // Eviction is not quadratic: 200,000 listings of one entry into a cap of 100,000.
        let mut c = Cache::new(100_000);
        let one = listing(1);
        let t0 = std::time::Instant::now();
        for i in 0..200_000 {
            c.put("s", &v(&[&format!("d{i}")]), one.clone());
        }
        let took = t0.elapsed();
        assert_eq!(c.map.len(), 50_000);
        assert!(c.get("s", &v(&["d199999"])).is_some() && c.get("s", &v(&["d0"])).is_none());
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[test]
    fn cache_feed_collision() {
        let mut c = Cache::new(100);
        let mut fed = HashMap::new();
        c.feed(&mut fed, "s", &v(&["cs", "Foo"]), listing(1));
        assert_eq!(c.get("s", &v(&["cs", "Foo"])).unwrap().len(), 1);
        c.feed(&mut fed, "s", &v(&["cs", "foo"]), listing(2));
        assert!(
            c.get("s", &v(&["cs", "Foo"])).is_none(),
            "neither is stored"
        );
        assert!(c.get("s", &v(&["cs", "foo"])).is_none());
        c.feed(&mut fed, "s", &v(&["cs", "FOO"]), listing(3));
        assert!(
            c.get("s", &v(&["cs", "FOO"])).is_none(),
            "a third stores nothing"
        );
        c.feed(&mut fed, "s", &v(&["other"]), listing(1));
        assert!(
            c.get("s", &v(&["other"])).is_some(),
            "another directory as usual"
        );
        assert_eq!(c.order.len(), c.map.len());
        let mut fed = HashMap::new();
        c.feed(&mut fed, "s", &v(&["cs", "Foo"]), listing(1));
        assert!(
            c.get("s", &v(&["cs", "Foo"])).is_some(),
            "a new walk stores it again"
        );
    }

    #[test]
    fn cache_feed_collision_seen_in_the_parent() {
        let dirs = |names: &[&str]| {
            Arc::new(
                names
                    .iter()
                    .map(|n| Entry {
                        name: n.to_string(),
                        size: 0,
                        mtime: 0,
                        attributes: 0x10,
                        file_id: 0,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let mut c = Cache::new(100);
        let mut fed = HashMap::new();
        // `cs` holds Foo and foo; the walk lists only foo (Foo was denied, gone or skipped), and
        // then foo's own subdirectory x.
        c.feed(&mut fed, "s", &v(&["cs"]), dirs(&["Foo", "foo", "plain"]));
        c.feed(&mut fed, "s", &v(&["cs", "foo"]), dirs(&["x"]));
        c.feed(&mut fed, "s", &v(&["cs", "foo", "x"]), listing(2));
        c.feed(&mut fed, "s", &v(&["cs", "plain"]), listing(1));
        assert!(c.get("s", &v(&["cs"])).is_some());
        assert!(
            c.get("s", &v(&["cs", "Foo"])).is_none() && c.get("s", &v(&["cs", "foo"])).is_none(),
            "a name that collides in the parent's listing is stored for neither"
        );
        assert!(
            c.get("s", &v(&["cs", "foo", "x"])).is_none(),
            "nor what is below it"
        );
        assert!(c.get("s", &v(&["cs", "plain"])).is_some());
        assert_eq!(c.order.len(), c.map.len());
    }

    #[test]
    fn cache_ignores_case() {
        let mut c = Cache::new(10);
        c.put("Share", &v(&["Tree"]), listing(1));
        assert!(c.get("share", &v(&["tree"])).is_some());
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    async fn pair() -> (TcpStream, TcpStream) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).await.unwrap();
        let (b, _) = l.accept().await.unwrap();
        (a, b)
    }

    fn local_target(port: u16, proxy: Option<Proxy>) -> Target {
        Target {
            host: "127.0.0.1".into(),
            port,
            user: String::new(),
            password: String::new(),
            domain: String::new(),
            proxy,
            timeout: Duration::from_secs(5),
            kerberos: false,
        }
    }

    fn target_to(host: &str, proxy: Proxy) -> Target {
        Target {
            host: host.into(),
            port: 445,
            proxy: Some(proxy),
            ..local_target(445, None)
        }
    }

    /// What a fake SOCKS5 proxy read, one entry per message.
    type Seen = Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

    /// A fake SOCKS5 proxy on `bind` for one connection. It records every message it reads and
    /// answers the greeting with `first` (`[5, 0]`, `[5, 2]`, `[5, 0xff]`, any other bytes; empty:
    /// it closes), a login with `[1, login]`, the request with reply code `reply`; after a
    /// refusal it closes, after a grant it holds the connection.
    async fn fake_proxy(bind: &str, first: &'static [u8], login: u8, reply: u8) -> (Proxy, Seen) {
        let l = tokio::net::TcpListener::bind(bind).await.unwrap();
        let a = l.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let rec = seen.clone();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            async fn take(c: &mut TcpStream, n: usize) -> std::io::Result<Vec<u8>> {
                let mut b = vec![0u8; n];
                c.read_exact(&mut b).await.map(|_| b)
            }
            let Ok(mut g) = take(&mut c, 2).await else {
                return;
            };
            g.extend(take(&mut c, g[1] as usize).await.unwrap());
            rec.lock().unwrap().push(g);
            if first.is_empty() {
                return;
            }
            c.write_all(first).await.unwrap();
            if first == [5, 2] {
                let mut m = take(&mut c, 2).await.unwrap();
                m.extend(take(&mut c, m[1] as usize).await.unwrap());
                let n = take(&mut c, 1).await.unwrap();
                m.extend(&n);
                m.extend(take(&mut c, n[0] as usize).await.unwrap());
                rec.lock().unwrap().push(m);
                c.write_all(&[1, login]).await.unwrap();
                if login != 0 {
                    return;
                }
            } else if first != [5, 0] {
                return;
            }
            let mut r = take(&mut c, 4).await.unwrap();
            let len = match r[3] {
                1 => 4,
                4 => 16,
                _ => {
                    let n = take(&mut c, 1).await.unwrap();
                    r.extend(&n);
                    n[0] as usize
                }
            };
            r.extend(take(&mut c, len + 2).await.unwrap());
            rec.lock().unwrap().push(r);
            c.write_all(&[5, reply, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            if reply == 0 {
                let mut sink = Vec::new();
                let _ = c.read_to_end(&mut sink).await;
            }
        });
        let proxy = Proxy {
            host: a.ip().to_string(),
            port: a.port(),
            auth: None,
        };
        (proxy, seen)
    }

    /// The bytes rsmb puts on the wire to the proxy: a name goes as a name (the proxy resolves
    /// it), an address as an address; percent-decoded credentials; `TCP_NODELAY` on our socket.
    #[test]
    fn socks5_handshake_bytes() {
        rt().block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let s = open_tcp(&local_target(port, None), DIAL_TIMEOUT)
                .await
                .unwrap();
            assert!(s.nodelay().unwrap(), "direct");

            let name = "fileserver.invalid";
            let (p, seen) = fake_proxy("127.0.0.1:0", &[5, 0], 0, 0).await;
            let s = open_tcp(&target_to(name, p), DIAL_TIMEOUT).await.unwrap();
            assert!(s.nodelay().unwrap(), "through the proxy");
            let mut want = vec![5, 1, 0, 3, name.len() as u8];
            want.extend(name.as_bytes());
            want.extend(445u16.to_be_bytes());
            assert_eq!(
                *seen.lock().unwrap(),
                [vec![5, 1, 0], want],
                "greeting without credentials; a name, not an address"
            );

            for (host, atyp, addr) in [
                ("10.1.2.3", 1, vec![10, 1, 2, 3]),
                ("::1", 4, {
                    let mut v = vec![0u8; 15];
                    v.push(1);
                    v
                }),
            ] {
                let (p, seen) = fake_proxy("127.0.0.1:0", &[5, 0], 0, 0).await;
                open_tcp(&target_to(host, p), DIAL_TIMEOUT).await.unwrap();
                let mut want = vec![5, 1, 0, atyp];
                want.extend(addr);
                want.extend(445u16.to_be_bytes());
                assert_eq!(seen.lock().unwrap()[1], want, "{host}");
            }

            let (p, seen) = fake_proxy("[::1]:0", &[5, 2], 0, 0).await;
            let url = format!(
                "socks5://us%40er:p%3Ass%2F%25w%C3%B6rd@[{}]:{}",
                p.host, p.port
            );
            let p = parse_proxy(&url).unwrap();
            open_tcp(&target_to(name, p), DIAL_TIMEOUT).await.unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0], [5, 2, 0, 2], "both methods offered");
            let mut login = vec![1, 5];
            login.extend(b"us@er");
            login.push(11);
            login.extend("p:ss/%wörd".as_bytes());
            assert_eq!(seen[1], login);
        });
    }

    /// Each answer of a proxy in our words: kind connect, `proxy <host>:<port>: `, the target
    /// named when the proxy answered for it, never the password.
    #[test]
    fn proxy_errors() {
        rt().block_on(async {
            let pw = "Wr0ngSecret";
            let cases: [(&'static [u8], bool, u8, u8, &str); 14] = [
                (&[5, 0xff], false, 0, 0, "wants a user name and password: --proxy socks5://user:pass@"),
                (&[5, 0xff], true, 0, 0, "accepts neither a session without login nor a user name and password"),
                (&[5, 2], true, 1, 0, "user name or password refused"),
                (&[5, 0], false, 0, 1, "cannot reach fileserver.invalid:445: the proxy reports a general failure"),
                (&[5, 0], false, 0, 3, "cannot reach fileserver.invalid:445: network unreachable"),
                (&[5, 0], false, 0, 4, "cannot reach fileserver.invalid:445: host unreachable"),
                (&[5, 0], false, 0, 5, "cannot reach fileserver.invalid:445: connection refused"),
                (&[5, 0], false, 0, 6, "cannot reach fileserver.invalid:445: timed out on the way"),
                (&[5, 0], false, 0, 2, "cannot reach fileserver.invalid:445: not allowed by the proxy's rules"),
                (&[5, 0], false, 0, 7, "cannot reach fileserver.invalid:445: the proxy does not support CONNECT"),
                (&[5, 0], false, 0, 8, "cannot reach fileserver.invalid:445: the proxy does not support this kind of address"),
                (b"HTTP/1.1 400 Bad Request\r\n\r\n", false, 0, 0, "not a SOCKS5 answer (is it a SOCKS5 proxy?)"),
                (&[], false, 0, 0, "closed the connection during the SOCKS5 handshake (is it a SOCKS5 proxy?)"),
                (&[], true, 0, 0, "closed the connection during the SOCKS5 handshake"),
            ];
            for (first, creds, login, reply, words) in cases {
                let (mut p, _) = fake_proxy("127.0.0.1:0", first, login, reply).await;
                if creds {
                    p.auth = Some(("tester".into(), pw.into()));
                }
                let prefix = format!("proxy {}: ", hostport(&p.host, p.port));
                let e = open_tcp(&target_to("fileserver.invalid", p), DIAL_TIMEOUT)
                    .await
                    .unwrap_err();
                assert_eq!(e.kind, Kind::Connect, "{words}");
                assert!(e.message.starts_with(&prefix), "{}", e.message);
                assert!(e.message.contains(words), "{words}: {}", e.message);
                assert!(!e.message.contains(pw), "{}", e.message);
                assert_eq!(e.exit_code(true), 3);
            }
        });
    }

    /// A proxy that accepts TCP and never answers: the message says the TCP connection stood and
    /// names the three causes rsmb cannot tell apart (finding 3); the limit is kept.
    #[test]
    fn dial_says_how_far_it_got() {
        assert_eq!(
            DIAL_TIMEOUT,
            Duration::from_secs(10),
            "the manual says 10 s"
        );
        assert_eq!(hostport("::1", 1080), "[::1]:1080");
        assert_eq!(hostport("h", 445), "h:445");
        assert_eq!(span(Duration::from_secs(10)), "10 s");
        rt().block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let a = l.local_addr().unwrap();
            tokio::spawn(async move {
                let (_c, _) = l.accept().await.unwrap();
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
            let p = Proxy {
                host: a.ip().to_string(),
                port: a.port(),
                auth: None,
            };
            let t0 = std::time::Instant::now();
            let e = open_tcp(
                &target_to("fileserver.invalid", p),
                Duration::from_millis(200),
            )
            .await
            .unwrap_err();
            assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
            assert_eq!(e.kind, Kind::Connect);
            for w in [
                &format!("proxy {a}: connected to the proxy, but the SOCKS5 handshake did not finish within 200 ms"),
                "the proxy is silent",
                "is not a SOCKS5 proxy",
                "cannot reach fileserver.invalid:445",
            ] {
                assert!(e.message.contains(w), "{w}: {}", e.message);
            }
        });
    }

    #[test]
    fn banner_names_the_proxy() {
        let mut t = local_target(445, None);
        t.host = "fileserver.example".into();
        t.user = "tester".into();
        t.domain = "TESTDOM".into();
        let direct = banner(&t, "SMB 3.1.1");
        assert_eq!(
            direct,
            "logged in as 'TESTDOM\\tester' on fileserver.example:445 (SMB 3.1.1)"
        );
        assert!(!direct.contains("proxy"));
        t.proxy = Some(Proxy {
            host: "10.0.0.1".into(),
            port: 1081,
            auth: Some(("puser".into(), "ppass".into())),
        });
        let via = banner(&t, "SMB 3.1.1");
        assert_eq!(
            via,
            "logged in as 'TESTDOM\\tester' on fileserver.example:445 through proxy 10.0.0.1:1081 \
             (SMB 3.1.1)"
        );
        assert!(!via.contains("puser") && !via.contains("ppass"));
        assert!(!via.contains("Kerberos"));
        t.kerberos = true;
        t.domain = "RSMB.TEST".into();
        let k = banner(&t, "SMB 3.1.1");
        assert!(
            k.contains("'RSMB.TEST\\tester' (Kerberos) on fileserver.example:445 through proxy"),
            "{k}"
        );
    }

    #[test]
    fn receive_reports_progress() {
        rt().block_on(async {
            let (ours, mut peer) = pair().await;
            let (_tx, rx) = transport(ours);
            let rx = Arc::new(rx);
            let p1 = rx.receive_progress().unwrap();
            let p2 = rx.receive_progress().unwrap();
            assert!(Arc::ptr_eq(&p1, &p2), "the same Arc every time");
            const LEN: usize = 200_000;
            let body: Vec<u8> = (0..LEN).map(|i| i as u8).collect();
            peer.write_all(&(LEN as u32).to_be_bytes()).await.unwrap();
            peer.write_all(&body[..LEN / 2]).await.unwrap();
            let r = rx.clone();
            let pending = tokio::spawn(async move { r.receive().await });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let s = p1.snapshot();
                if let Some(f) = s.frame
                    && f.received > 0
                {
                    assert_eq!(f.len, LEN as u64);
                    assert!(f.received < LEN as u64);
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "no progress seen: {s:?}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(!pending.is_finished());
            peer.write_all(&body[LEN / 2..]).await.unwrap();
            let got = pending.await.unwrap().unwrap();
            assert_eq!(got, body);
            assert!(p1.snapshot().frame.is_none());

            // A peer that closes inside a frame.
            peer.write_all(&1000u32.to_be_bytes()).await.unwrap();
            peer.write_all(&[1, 2, 3]).await.unwrap();
            drop(peer);
            let e = rx.receive().await.unwrap_err();
            assert!(matches!(e, smb2::Error::Disconnected), "{e:?}");
            assert!(p1.snapshot().frame.is_none());
        });
    }

    #[test]
    fn abandon_marks_dead_only_in_flight() {
        rt().block_on(async {
            let (ours, _peer) = pair().await;
            let (tx, rx) = transport(ours);
            let conn = Connection::from_transport(Box::new(tx), Box::new(rx), "127.0.0.1");
            let mut s = Session::with_conn(local_target(445, None), conn);
            s.abandon();
            assert!(!s.conn.is_disconnected());
            s.in_flight = true;
            s.abandon();
            assert!(s.conn.is_disconnected());
            assert!(!s.in_flight);
        });
    }

    // -----------------------------------------------------------------------------------------
    // Kerberos

    use smb2::auth::kerberos::ccache::{CcacheCredential, CcachePrincipal};

    fn princ(parts: &[&str], realm: &str) -> CcachePrincipal {
        CcachePrincipal {
            name_type: 1,
            realm: realm.into(),
            components: parts.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn cred(server: &[&str], realm: &str, end: u32) -> CcacheCredential {
        CcacheCredential {
            client: princ(&["tester"], "RSMB.TEST"),
            server: princ(server, realm),
            key_etype: 18,
            key_data: vec![0; 32],
            authtime: 0,
            starttime: 0,
            endtime: end,
            renew_till: 0,
            ticket: Vec::new(),
        }
    }

    fn ccache(credentials: Vec<CcacheCredential>) -> CCache {
        CCache {
            version: 0x0504,
            default_principal: princ(&["tester"], "RSMB.TEST"),
            credentials,
        }
    }

    fn kt(host: &str, user: &str, domain: &str) -> Target {
        Target {
            host: host.into(),
            user: user.into(),
            domain: domain.into(),
            kerberos: true,
            ..local_target(445, None)
        }
    }

    #[test]
    fn ccache_paths() {
        for v in [None, Some(OsStr::new(""))] {
            let (p, default) = ccache_path(v, 1000).unwrap();
            assert_eq!(
                (p, default),
                (PathBuf::from("/tmp/krb5cc_1000"), true),
                "{v:?}"
            );
        }
        for (v, want) in [
            ("FILE:/tmp/x", "/tmp/x"),
            ("/tmp/a:b", "/tmp/a:b"),
            ("./c", "./c"),
            ("plain", "plain"),
        ] {
            let (p, default) = ccache_path(Some(OsStr::new(v)), 1000).unwrap();
            assert_eq!((p, default), (PathBuf::from(want), false), "{v}");
        }
        // A name that is not UTF-8 is the variable's, never the default (finding 2).
        for (v, want) in [
            (&b"/tmp/c\xe9.cc"[..], &b"/tmp/c\xe9.cc"[..]),
            (b"FILE:/tmp/c\xe9.cc", b"/tmp/c\xe9.cc"),
        ] {
            let (p, default) = ccache_path(Some(OsStr::from_bytes(v)), 1000).unwrap();
            assert_eq!((p.as_os_str().as_bytes(), default), (want, false));
        }
        let e = ccache_path(Some(OsStr::from_bytes(b"KCM\xe9:1")), 1000).unwrap_err();
        assert!(e.message.contains("'KCM\u{fffd}'"), "{}", e.message);
        for (v, ty) in [
            ("KCM:", "KCM"),
            ("KCM:1000", "KCM"),
            ("KEYRING:persistent:1000", "KEYRING"),
            ("DIR:/run/x", "DIR"),
            ("MEMORY:y", "MEMORY"),
            ("API:z", "API"),
        ] {
            let e = ccache_path(Some(OsStr::new(v)), 1000).unwrap_err();
            assert_eq!(e.kind, Kind::Auth, "{v}");
            assert!(e.message.starts_with("Kerberos: "), "{}", e.message);
            assert!(e.message.contains(&format!("'{ty}'")), "{}", e.message);
            assert!(e.message.contains("cache file"), "{}", e.message);
        }
    }

    #[test]
    fn cache_owner_check() {
        assert!(cache_owner(1000, 1000, "/tmp/c").is_ok());
        let e = cache_owner(0, 1000, "/tmp/c").unwrap_err();
        assert_eq!(e.kind, Kind::Auth);
        assert!(
            e.message.starts_with("Kerberos: ticket cache /tmp/c"),
            "{}",
            e.message
        );
        for w in ["uid 0", "uid 1000", "own user only"] {
            assert!(e.message.contains(w), "{w}: {}", e.message);
        }
    }

    #[test]
    fn ticket_choice() {
        const NOW: u64 = 1_795_000_000;
        let valid = (NOW + 3600) as u32;
        let tgt = || cred(&["krbtgt", "RSMB.TEST"], "RSMB.TEST", valid);
        let svc = |host: &str, end| cred(&["cifs", host], "RSMB.TEST", end);
        let pick = |c: CCache, t: &Target, now| pick_ticket(c, t, now, "/c", false);
        let auth_err = |r: Result<Ticket>| {
            let e = r.err().expect("an error");
            assert_eq!(e.kind, Kind::Auth, "{}", e.message);
            assert!(e.message.starts_with("Kerberos: "), "{}", e.message);
            e.message
        };

        // A valid service ticket; the user and realm as the cache spells them.
        let host = kt("dc.rsmb.test", "", "");
        let k = pick(ccache(vec![svc("dc.rsmb.test", valid), tgt()]), &host, NOW).unwrap();
        assert!(k.service);
        assert_eq!((k.user.as_str(), k.realm.as_str()), ("tester", "RSMB.TEST"));
        // -H in another case still finds it.
        let upper = kt("DC.RSMB.TEST", "", "");
        assert!(
            pick(ccache(vec![svc("dc.rsmb.test", valid)]), &upper, NOW)
                .unwrap()
                .service
        );
        // A ticket-granting ticket only.
        assert!(!pick(ccache(vec![tgt()]), &host, NOW).unwrap().service);
        // An expired service ticket beside a valid TGT: the KDC is asked, the entry is gone.
        let k = pick(ccache(vec![svc("dc.rsmb.test", 5), tgt()]), &host, NOW).unwrap();
        assert!(!k.service);
        assert_eq!(k.cache.credentials.len(), 1);
        assert_eq!(k.cache.credentials[0].server.components[0], "krbtgt");
        // Everything expired: the latest end is named, written by hand (finding 10).
        let end = 1_795_029_928;
        let old = ccache(vec![
            svc("dc.rsmb.test", end - 100),
            cred(&["krbtgt", "RSMB.TEST"], "RSMB.TEST", end),
        ]);
        let m = auth_err(pick(old.clone(), &host, u64::from(end) + 1));
        assert!(m.contains("krbtgt/RSMB.TEST@RSMB.TEST"), "{m}");
        assert!(m.contains("expired at 2026-11-18T19:25:28Z"), "{m}");
        assert!(m.contains("run kinit"), "{m}");
        assert!(!m.contains("KRB5CCNAME is not set"), "{m}");
        let m = auth_err(pick_ticket(
            old,
            &host,
            u64::from(end) + 1,
            "/tmp/krb5cc_7",
            true,
        ));
        assert!(m.contains("expired at"), "{m}");
        assert!(
            m.contains("KRB5CCNAME is not set, so the default file /tmp/krb5cc_7 was used"),
            "{m}"
        );
        // Tickets for another host and no TGT.
        let other = || ccache(vec![svc("other.rsmb.test", valid)]);
        let m = auth_err(pick(other(), &host, NOW));
        assert!(
            m.contains("holds no ticket for cifs/dc.rsmb.test@RSMB.TEST"),
            "{m}"
        );
        assert!(m.contains("run kinit tester@RSMB.TEST"), "{m}");
        assert!(!m.contains("KRB5CCNAME is not set"), "{m}");
        let m = auth_err(pick_ticket(other(), &host, NOW, "/d", true));
        assert!(
            m.contains("KRB5CCNAME is not set, so the default file"),
            "{m}"
        );
        // Configuration entries (end time 0) never count, not even as expired.
        let conf = cred(&["krb5_ccache_conf_data", "fast_avail"], "X-CACHECONF:", 0);
        let m = auth_err(pick(ccache(vec![conf]), &host, NOW));
        assert!(m.contains("holds no ticket"), "{m}");
        // -u and -d are compared without regard to case; the cache's spelling is kept.
        let k = pick(
            ccache(vec![tgt()]),
            &kt("dc.rsmb.test", "TESTER", "rsmb.test"),
            NOW,
        )
        .unwrap();
        assert_eq!((k.user.as_str(), k.realm.as_str()), ("tester", "RSMB.TEST"));
        let m = auth_err(pick(
            ccache(vec![tgt()]),
            &kt("dc.rsmb.test", "other", ""),
            NOW,
        ));
        for w in ["tester@RSMB.TEST", "'other'", "not to '"] {
            assert!(m.contains(w), "{w}: {m}");
        }
        let m = auth_err(pick(ccache(vec![tgt()]), &kt("h", "", "OTHER.TEST"), NOW));
        assert!(m.contains("RSMB.TEST") && m.contains("OTHER.TEST"), "{m}");
        // A principal with two name parts.
        let mut two = ccache(vec![tgt()]);
        two.default_principal = princ(&["host", "x"], "RSMB.TEST");
        let m = auth_err(pick(two, &host, NOW));
        assert!(m.contains("more than one name part"), "{m}");
    }

    #[test]
    fn krb5_conf_kdc() {
        let conf = "\
# comment
[libdefaults]
\tdefault_realm = RSMB.TEST
\tkdc = wrong.libdefaults
[realms]
OTHER.TEST = {
  kdc = other.kdc
}
RSMB.TEST = {
  auth_to_local_names = {
    kdc = nested.wrong
  }
  ; kdc = commented.out
  kdc=first.kdc:750
  kdc = second.kdc
}
THIRD.TEST = {
  kdc = third.kdc
}
[domain_realm]
  kdc = wrong.domain_realm
";
        assert_eq!(krb5_kdc(conf, "RSMB.TEST"), Some(("first.kdc".into(), 750)));
        assert_eq!(krb5_kdc(conf, "OTHER.TEST"), Some(("other.kdc".into(), 88)));
        assert_eq!(krb5_kdc(conf, "THIRD.TEST"), Some(("third.kdc".into(), 88)));
        assert_eq!(
            krb5_kdc(conf, "rsmb.test"),
            None,
            "the realm is compared exactly"
        );
        assert_eq!(krb5_kdc(conf, "NONE.TEST"), None);
        // A nested block before the kdc, then the next realm's kdc: not taken for this one.
        let nested =
            "[realms]\nA = {\n  x = {\n    kdc = in.nested\n  }\n}\nB = {\n  kdc = b.kdc\n}\n";
        assert_eq!(krb5_kdc(nested, "A"), None);
        let one = |v: &str| krb5_kdc(&format!("[realms]\nR = {{\n kdc = {v}\n}}\n"), "R");
        for (v, host, port) in [
            ("h", "h", 88),
            ("h:750", "h", 750),
            ("[::1]:88", "::1", 88),
            ("[2001:db8::1]", "2001:db8::1", 88),
            ("2001:db8::1", "2001:db8::1", 88),
            ("tcp/h", "h", 88),
            ("udp/h:88", "h", 88),
        ] {
            assert_eq!(one(v), Some((host.to_string(), port)), "{v}");
        }
        assert_eq!(one("https://kdcproxy/KdcProxy"), None);
        let skip = "[realms]\nR = {\n kdc = https://p/KdcProxy\n kdc = real.kdc\n}\n";
        assert_eq!(krb5_kdc(skip, "R"), Some(("real.kdc".into(), 88)));
        assert_eq!(krb5_kdc("", "R"), None);
        assert_eq!(krb5_kdc("[libdefaults]\nkdc = x\n", "R"), None);
    }

    #[test]
    fn kdc_files() {
        assert_eq!(krb5_files(None), [PathBuf::from("/etc/krb5.conf")]);
        assert_eq!(
            krb5_files(Some(OsStr::new(""))),
            [PathBuf::from("/etc/krb5.conf")]
        );
        assert_eq!(
            krb5_files(Some(OsStr::new("/a:/b"))),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        // Not UTF-8 is the variable's too, never /etc/krb5.conf (finding 2).
        assert_eq!(
            krb5_files(Some(OsStr::from_bytes(b"/a\xe9::/b"))),
            [
                PathBuf::from(OsStr::from_bytes(b"/a\xe9")),
                PathBuf::from("/b")
            ]
        );
        let dir = std::env::temp_dir().join(format!("rsmb-unit-kdc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, text: &str| {
            let p = dir.join(name);
            std::fs::write(&p, text).unwrap();
            p
        };
        let other = file("other.conf", "[realms]\nOTHER.TEST = {\n kdc = o\n}\n");
        let first = file("first.conf", "[realms]\nRSMB.TEST = {\n kdc = k1:8888\n}\n");
        let second = file("second.conf", "[realms]\nRSMB.TEST = {\n kdc = k2\n}\n");
        let missing = dir.join("missing.conf");
        let files = [
            missing.clone(),
            dir.clone(),
            other.clone(),
            first.clone(),
            second,
        ];
        assert_eq!(
            kdc_for(&files, "RSMB.TEST"),
            ("k1".to_string(), 8888, Some(first))
        );
        assert_eq!(
            kdc_for(&[missing, dir.clone(), other], "RSMB.TEST"),
            ("rsmb.test".to_string(), 88, None)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO or a device is not read (finding 4): `open` on a FIFO with no writer would wait for
    /// good. Both functions end at once; a hang fails on the time bound, not the suite.
    #[test]
    fn non_regular_files_are_not_read() {
        let dir = std::env::temp_dir().join(format!("rsmb-unit-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let (f, d) = (fifo.clone(), dir.clone());
        std::thread::spawn(move || {
            let e = open_regular(&f).unwrap_err();
            let kdc = kdc_for(&[f, d, PathBuf::from("/dev/null")], "RSMB.TEST");
            let _ = tx.send((e.to_string(), kdc));
        });
        let (e, kdc) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a FIFO blocked the read");
        assert_eq!(e, "not a regular file");
        assert_eq!(kdc, ("rsmb.test".to_string(), 88, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn kdc_errors() {
        let e = |code, text| kdc_error(code, text, "fs.rsmb.test", "tester", "RSMB.TEST");
        let m = e(7, None);
        assert!(
            m.contains("the realm RSMB.TEST has no service cifs/fs.rsmb.test"),
            "{m}"
        );
        let m = e(13, None);
        for w in ["KDC_ERR_BADOPTION", "kinit -f", "kvno cifs/fs.rsmb.test"] {
            assert!(m.contains(w), "{w}: {m}");
        }
        assert!(e(32, None).contains("the ticket-granting ticket has expired"));
        assert!(e(37, None).contains("clocks"));
        let m = e(36, None);
        assert!(
            m.contains("'tester@RSMB.TEST'") && m.contains("KRB_AP_ERR_BADMATCH"),
            "{m}"
        );
        assert_eq!(e(99, None), "KDC error 99");
        assert_eq!(e(99, Some("odd")), "KDC error 99: odd");
    }

    #[test]
    fn kerberos_refusals() {
        let r = |e| kerberos_refusal(e, "login as 'R\\u' at h:445", "h", "R");
        let proto = |status| smb2::Error::Protocol {
            status,
            command: Command::SessionSetup,
        };
        let e = r(proto(NtStatus::LOGON_FAILURE));
        assert_eq!(e.kind, Kind::Auth);
        assert!(e.message.starts_with("Kerberos: login as"), "{}", e.message);
        assert!(e.message.contains("login refused"), "{}", e.message);
        let e = r(proto(NtStatus::INVALID_PARAMETER));
        assert_eq!(e.kind, Kind::Auth);
        assert!(e.message.starts_with("Kerberos: login as"), "{}", e.message);
        for w in [
            "the server did not accept the ticket",
            "STATUS_INVALID_PARAMETER",
        ] {
            assert!(e.message.contains(w), "{w}: {}", e.message);
        }
        for crate_err in [
            smb2::Error::Auth {
                message: "TGS session key has unsupported etype 20".into(),
            },
            smb2::Error::invalid_data("Kerberos: unexpected end of data"),
        ] {
            let text = crate_err.to_string();
            let e = r(crate_err);
            assert_eq!(e.kind, Kind::Auth);
            assert!(e.message.starts_with("Kerberos: login as"), "{}", e.message);
            assert!(e.message.contains("could not be used"), "{}", e.message);
            assert!(e.message.contains(&text), "{}", e.message);
            assert!(!e.message.contains("server"), "{}", e.message);
        }
        assert!(r(smb2::Error::Disconnected).is_lost());
    }

    /// A KRB-ERROR with `code`, as a KDC sends it (only the fields the crate requires).
    fn krb_error(code: u8) -> Vec<u8> {
        fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
            let mut v = vec![tag, body.len() as u8];
            v.extend(body);
            v
        }
        let gs = |s: &str| tlv(0x1b, s.as_bytes());
        let sname = tlv(
            0x30,
            &[
                tlv(0xa0, &tlv(0x02, &[2])),
                tlv(0xa1, &tlv(0x30, &[gs("cifs"), gs("h")].concat())),
            ]
            .concat(),
        );
        let fields = [
            tlv(0xa6, &tlv(0x02, &[code])),
            tlv(0xa9, &gs("RSMB.TEST")),
            tlv(0xaa, &sname),
        ];
        tlv(0x7e, &tlv(0x30, &fields.concat()))
    }

    /// How the fake KDC treats a request.
    #[derive(Clone, Copy)]
    enum Fake {
        Answer,
        Refuse,
        Silent,
        Close,
    }

    /// A fake KDC on a local TCP port: each connection's framed request is recorded, then answered
    /// as `how` says. Returns its address, what it read, and how many connections it accepted.
    async fn fake_kdc(
        how: Fake,
    ) -> (
        std::net::SocketAddr,
        Seen,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (rec, count) = (seen.clone(), accepted.clone());
        tokio::spawn(async move {
            loop {
                let (mut c, _) = l.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                let rec = rec.clone();
                tokio::spawn(async move {
                    let mut h = [0u8; 4];
                    c.read_exact(&mut h).await.unwrap();
                    let mut body = vec![0u8; u32::from_be_bytes(h) as usize];
                    c.read_exact(&mut body).await.unwrap();
                    rec.lock().unwrap().push([&h[..], &body].concat());
                    let answer = match how {
                        Fake::Answer => [b"answer to ".as_slice(), &body].concat(),
                        Fake::Refuse => krb_error(7),
                        Fake::Silent => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            return;
                        }
                        Fake::Close => return,
                    };
                    let mut m = (answer.len() as u32).to_be_bytes().to_vec();
                    m.extend(answer);
                    c.write_all(&m).await.unwrap();
                });
            }
        });
        (a, seen, accepted)
    }

    fn relay_kdc(host: &str, port: u16, proxy: Option<Proxy>, from: Option<&str>) -> Kdc {
        Kdc {
            target: Target {
                host: host.into(),
                port,
                ..local_target(port, proxy)
            },
            from: from.map(PathBuf::from),
            files: vec![PathBuf::from("/nonexistent/krb5.conf")],
            host: "fs.rsmb.test".into(),
            user: "tester".into(),
            realm: "RSMB.TEST".into(),
        }
    }

    /// The relay on a new loopback socket, run as a task: its address and its end.
    async fn start_relay(
        kdc: Kdc,
        limit: Duration,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<Error>) {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let a = sock.local_addr().unwrap();
        (
            a,
            tokio::spawn(async move { kdc_relay(sock, &kdc, limit).await }),
        )
    }

    /// The crate's side: one datagram to the relay.
    async fn crate_sends(relay: std::net::SocketAddr, what: &[u8]) -> UdpSocket {
        let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c.send_to(what, relay).await.unwrap();
        c
    }

    async fn relay_error(h: tokio::task::JoinHandle<Error>) -> Error {
        let e = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("the relay ended")
            .unwrap();
        assert!(
            e.message.starts_with("Kerberos: asking the KDC "),
            "{}",
            e.message
        );
        e
    }

    /// Finding 1: only the first sender is served. A datagram from another socket reaches no KDC
    /// and gets no answer, and the first sender's exchange goes on.
    #[test]
    fn kdc_relay_serves_the_first_sender_only() {
        rt().block_on(async {
            let (a, seen, _) = fake_kdc(Fake::Answer).await;
            let (relay, h) =
                start_relay(relay_kdc("127.0.0.1", a.port(), None, None), DIAL_TIMEOUT).await;
            let ours = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut buf = [0u8; 100];
            ours.send_to(b"ours", relay).await.unwrap();
            let (n, _) = tokio::time::timeout(Duration::from_secs(5), ours.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&buf[..n], b"answer to ours");
            stranger.send_to(b"stranger", relay).await.unwrap();
            let ms300 = Duration::from_millis(300);
            assert!(
                tokio::time::timeout(ms300, stranger.recv_from(&mut buf))
                    .await
                    .is_err(),
                "the stranger got an answer"
            );
            ours.send_to(b"again", relay).await.unwrap();
            let (n, _) = tokio::time::timeout(Duration::from_secs(5), ours.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&buf[..n], b"answer to again");
            let got = seen.lock().unwrap().clone();
            assert_eq!(got.len(), 2, "the KDC saw {} requests", got.len());
            assert!(got.iter().all(|m| !m.ends_with(b"stranger")));
            assert!(!h.is_finished());
            h.abort();
        });
    }

    #[test]
    fn kdc_relay_carries_the_exchange() {
        rt().block_on(async {
            let ms200 = Duration::from_millis(200);
            // The answer is handed back, one datagram for one, a second one too.
            let (a, seen, _) = fake_kdc(Fake::Answer).await;
            let (relay, h) = start_relay(relay_kdc("127.0.0.1", a.port(), None, None), DIAL_TIMEOUT).await;
            let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            for req in [&b"first request"[..], b"second"] {
                c.send_to(req, relay).await.unwrap();
                let mut buf = [0u8; 100];
                let (n, from) = tokio::time::timeout(Duration::from_secs(5), c.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(from, relay);
                assert_eq!(&buf[..n], [b"answer to ".as_slice(), req].concat());
            }
            let got = seen.lock().unwrap().clone();
            assert_eq!(got.len(), 2);
            assert_eq!(got[0], [&13u32.to_be_bytes()[..], b"first request"].concat());
            assert!(!h.is_finished());
            h.abort();

            // A KRB-ERROR ends the login, and the crate gets nothing.
            let (a, _, _) = fake_kdc(Fake::Refuse).await;
            let (relay, h) = start_relay(relay_kdc("127.0.0.1", a.port(), None, None), DIAL_TIMEOUT).await;
            let c = crate_sends(relay, b"req").await;
            let e = relay_error(h).await;
            assert_eq!(e.kind, Kind::Auth, "{}", e.message);
            assert!(e.message.contains("has no service cifs/fs.rsmb.test"), "{}", e.message);
            let mut buf = [0u8; 100];
            assert!(
                tokio::time::timeout(ms200, c.recv_from(&mut buf)).await.is_err(),
                "the refusal was handed on"
            );

            // Connected and silent: one message, every time (one timer at a time); 20 at once.
            let (a, _, _) = fake_kdc(Fake::Silent).await;
            let t0 = Instant::now();
            let mut runs = Vec::new();
            for _ in 0..20 {
                let (relay, h) = start_relay(relay_kdc("127.0.0.1", a.port(), None, None), ms200).await;
                runs.push((crate_sends(relay, b"req").await, h));
            }
            for (_c, h) in runs {
                let e = relay_error(h).await;
                assert_eq!(e.kind, Kind::Connect, "{}", e.message);
                assert!(
                    e.message.contains("connected to the KDC, but it did not answer within 200 ms"),
                    "{}",
                    e.message
                );
            }
            assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());

            // A closed port.
            let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = closed.local_addr().unwrap().port();
            drop(closed);
            let (relay, h) = start_relay(relay_kdc("127.0.0.1", port, None, None), DIAL_TIMEOUT).await;
            let _c = crate_sends(relay, b"req").await;
            let e = relay_error(h).await;
            assert_eq!(e.kind, Kind::Connect);
            assert!(e.message.contains("Connection refused"), "{}", e.message);

            // A KDC that closes after the request.
            let (a, _, _) = fake_kdc(Fake::Close).await;
            let (relay, h) = start_relay(relay_kdc("127.0.0.1", a.port(), None, None), DIAL_TIMEOUT).await;
            let _c = crate_sends(relay, b"req").await;
            let e = relay_error(h).await;
            assert_eq!(e.kind, Kind::Connect);
            assert!(
                e.message.contains("the KDC closed the connection without an answer"),
                "{}",
                e.message
            );

            // Through a proxy: the KDC's name goes to the proxy as a name.
            let (p, seen) = fake_proxy("127.0.0.1:0", &[5, 0], 0, 0).await;
            let (relay, h) = start_relay(relay_kdc("kdc.invalid", 88, Some(p), None), ms200).await;
            let _c = crate_sends(relay, b"req").await;
            relay_error(h).await;
            let mut want = vec![5, 1, 0, 3, 11];
            want.extend(b"kdc.invalid");
            want.extend(88u16.to_be_bytes());
            assert_eq!(seen.lock().unwrap()[1], want);
            // The proxy cannot reach the KDC: our words, and nothing goes around the proxy to a
            // KDC this host reaches directly.
            for host in ["kdc.invalid", "127.0.0.1"] {
                let (a, _, accepted) = fake_kdc(Fake::Answer).await;
                let (p, _) = fake_proxy("127.0.0.1:0", &[5, 0], 0, 5).await;
                let proxy = hostport(&p.host, p.port);
                let (relay, h) = start_relay(relay_kdc(host, a.port(), Some(p), None), DIAL_TIMEOUT).await;
                let _c = crate_sends(relay, b"req").await;
                let e = relay_error(h).await;
                assert_eq!(e.kind, Kind::Connect);
                let words = format!("proxy {proxy}: cannot reach {host}:{}: connection refused", a.port());
                assert!(e.message.contains(&words), "{words}: {}", e.message);
                assert_eq!(accepted.load(Ordering::SeqCst), 0, "a connection around the proxy");
            }
            // A proxy that accepts and never answers: open_tcp's handshake message, every time.
            let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let sa = silent.local_addr().unwrap();
            tokio::spawn(async move {
                let mut held = Vec::new();
                while let Ok((c, _)) = silent.accept().await {
                    held.push(c);
                }
            });
            let mut runs = Vec::new();
            for _ in 0..20 {
                let p = Proxy { host: sa.ip().to_string(), port: sa.port(), auth: None };
                let (relay, h) = start_relay(relay_kdc("kdc.invalid", 88, Some(p), None), ms200).await;
                runs.push((crate_sends(relay, b"req").await, h));
            }
            for (_c, h) in runs {
                let e = relay_error(h).await;
                assert_eq!(e.kind, Kind::Connect);
                assert!(
                    e.message.contains(
                        "connected to the proxy, but the SOCKS5 handshake did not finish within 200 ms"
                    ),
                    "{}",
                    e.message
                );
            }

            // The tails: where the address came from, and what to do.
            let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = closed.local_addr().unwrap().port();
            drop(closed);
            let (relay, h) =
                start_relay(relay_kdc("127.0.0.1", port, None, Some("/home/u/k.conf")), DIAL_TIMEOUT).await;
            let _c = crate_sends(relay, b"req").await;
            let e = relay_error(h).await;
            assert!(
                e.message.contains("(the KDC is the one /home/u/k.conf names for RSMB.TEST)"),
                "{}",
                e.message
            );
            let (relay, h) = start_relay(relay_kdc("127.0.0.1", port, None, None), DIAL_TIMEOUT).await;
            let _c = crate_sends(relay, b"req").await;
            let e = relay_error(h).await;
            for w in ["so the realm's name was tried", "KRB5_CONFIG", "kvno cifs/fs.rsmb.test"] {
                assert!(e.message.contains(w), "{w}: {}", e.message);
            }
            assert!(!e.message.contains("/etc/krb5.conf"), "{}", e.message);
        });
    }
}
