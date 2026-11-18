//! Stage 0 spike for the `smb2` crate (docs/plans/approved-plan.md, "Spikes").
//!
//! `spike-smb2 <step>...` runs the given steps (1-11, 13; step 12 is in run.sh) against the
//! test environment and prints one `ROW\t…` line per result. Throwaway measurement code.

use std::collections::BTreeSet;
use std::error::Error;
use std::io::Write as _;
use std::process::Command as Proc;
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use smb2::msg::close::CloseRequest;
use smb2::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use smb2::msg::read::{ReadRequest, ReadResponse};
use smb2::msg::write::WriteRequest;
use smb2::pack::{ReadCursor, Unpack};
use smb2::transport::{TransportReceive, TransportSend};
use smb2::types::flags::FileAccessMask;
use smb2::types::status::NtStatus;
use smb2::types::{Command, FileId, OplockLevel, TreeId};
use smb2::client::Connection;
use smb2::{CompoundOp, RenameOptions, Session, ShareInfo, Tree};
use smbtap::Tap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AMutex, Semaphore};
use tokio::task::JoinSet;

type R<T> = Result<T, Box<dyn Error + Send + Sync>>;

const FAST: &str = "10.213.45.10:445";
const SLOW: &str = "10.213.45.30:445";
const EDGE: &str = "10.213.45.11:445";
const BAD: &str = "10.213.45.31:445";
const HOLE: &str = "10.213.45.30:4450";
const PROXY: &str = "10.213.45.20";
const PRIVATE: &str = "fileserver.rsmb.test";
const USER: &str = "tester";
const PASS: &str = "testpass";
const DOM: &str = "TESTDOM";
const UP: &str = "upload/spike-smb2";
const BIG: &str = "large/big.bin";
const BIG_SIZE: u64 = 1_181_116_007;
const CTL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testenv/ctl.sh");

// ---------------------------------------------------------------- output

fn row(step: u32, what: &str, ep: &str, result: &str, detail: &str) {
    let clean = |s: &str| s.replace(['\t', '\n'], " ");
    println!(
        "ROW\t{step}\t{}\t{}\t{}\t{}",
        clean(what),
        clean(ep),
        clean(result),
        clean(detail)
    );
    std::io::stdout().flush().ok();
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1e3)
}

fn mbps(bytes: u64, d: Duration) -> String {
    format!("{:.1} MB/s", bytes as f64 / d.as_secs_f64() / 1e6)
}

/// Endpoint label with the TCP connect time measured just before (median of 5).
async fn ep(addr: &str) -> String {
    let mut v = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        if tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
            .await
            .is_ok_and(|r| r.is_ok())
        {
            v.push(t.elapsed());
        }
    }
    v.sort();
    let name = match addr {
        FAST => "fast",
        SLOW => "slow",
        EDGE => "edge",
        BAD => "wan-bad",
        HOLE => "slow:4450",
        _ => addr,
    };
    match v.get(v.len() / 2) {
        Some(d) => format!("{name} rtt {:.2}ms", d.as_secs_f64() * 1e3),
        None => format!("{name} rtt ?"),
    }
}

macro_rules! timed {
    ($n:expr, $body:expr) => {{
        let mut v = Vec::new();
        for _ in 0..$n {
            let t0 = Instant::now();
            $body;
            v.push(t0.elapsed());
        }
        v.sort();
        v[v.len() / 2]
    }};
}

// ---------------------------------------------------------------- transport

/// Our own transport: one `write_all` per frame (header and body in one buffer).
struct Tx<S>(AMutex<WriteHalf<S>>);
struct Rx<S>(AMutex<ReadHalf<S>>);

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Send + 'static> TransportSend for Tx<S> {
    async fn send(&self, data: &[u8]) -> smb2::Result<()> {
        if data.len() >= 1 << 24 {
            return Err(smb2::Error::invalid_data("frame too large"));
        }
        let mut frame = Vec::with_capacity(data.len() + 4);
        frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
        frame.extend_from_slice(data);
        self.0
            .lock()
            .await
            .write_all(&frame)
            .await
            .map_err(smb2::Error::Io)
    }
}

fn io_err(e: std::io::Error) -> smb2::Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        smb2::Error::Disconnected
    } else {
        smb2::Error::Io(e)
    }
}

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Send + 'static> TransportReceive for Rx<S> {
    async fn receive(&self) -> smb2::Result<Vec<u8>> {
        let mut r = self.0.lock().await;
        let mut h = [0u8; 4];
        r.read_exact(&mut h).await.map_err(io_err)?;
        if h[0] != 0 {
            return Err(smb2::Error::invalid_data("bad frame header"));
        }
        let n = u32::from_be_bytes(h) as usize;
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf).await.map_err(io_err)?;
        Ok(buf)
    }
}

fn conn_from<S: AsyncRead + AsyncWrite + Send + 'static>(s: S, server: &str) -> Connection {
    let (r, w) = tokio::io::split(s);
    Connection::from_transport(
        Box::new(Tx(AMutex::new(w))),
        Box::new(Rx(AMutex::new(r))),
        server,
    )
}

async fn dial(addr: &str) -> R<Connection> {
    let s = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    s.set_nodelay(true)?;
    Ok(conn_from(s, addr.rsplit_once(':').unwrap().0))
}

/// SOCKS5 to `fileserver.rsmb.test:445`; the proxy resolves the name.
async fn dial_socks(port: u16, auth: Option<(&str, &str)>) -> R<Connection> {
    use tokio_socks::tcp::Socks5Stream;
    let tcp = TcpStream::connect((PROXY, port)).await?;
    tcp.set_nodelay(true)?;
    let s = match auth {
        None => Socks5Stream::connect_with_socket(tcp, (PRIVATE, 445)).await?,
        Some((u, p)) => {
            Socks5Stream::connect_with_password_and_socket(tcp, (PRIVATE, 445), u, p).await?
        }
    };
    Ok(conn_from(s, PRIVATE))
}

async fn login(mut c: Connection, user: &str, pass: &str, dom: &str) -> R<Connection> {
    c.negotiate().await?;
    Session::setup(&mut c, user, pass, dom).await?;
    Ok(c)
}

async fn open(addr: &str, share: &str) -> R<(Connection, Arc<Tree>)> {
    let mut c = login(dial(addr).await?, USER, PASS, DOM).await?;
    let t = Tree::connect(&mut c, share).await?;
    Ok((c, Arc::new(t)))
}

async fn to<T>(secs: u64, f: impl Future<Output = R<T>>) -> R<T> {
    match tokio::time::timeout(Duration::from_secs(secs), f).await {
        Ok(r) => r,
        Err(_) => Err(format!("TIMEOUT after {secs} s").into()),
    }
}

// ---------------------------------------------------------------- tap

fn settle(tap: &Tap) -> bool {
    tokio::task::block_in_place(|| tap.settle(Duration::from_millis(300), Duration::from_secs(60)))
}

fn summary(s: &smbtap::Stats, at_return: u64) -> String {
    let cmds: Vec<String> = s.per_command.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    format!(
        "{} req / {} flights at return, {} settled [{}] peak-out {} interim {} conns {}",
        s.requests,
        at_return,
        s.flights,
        cmds.join(","),
        s.peak_outstanding,
        s.interim,
        s.connections
    )
}

/// Run `f` with the tap reset before; return its value, the stats and a summary string.
async fn count<T>(tap: &Tap, f: impl Future<Output = R<T>>) -> R<(T, smbtap::Stats, String)> {
    settle(tap);
    tap.reset();
    let v = f.await?;
    let at_return = tap.stats().flights;
    settle(tap);
    let s = tap.stats();
    let line = summary(&s, at_return);
    Ok((v, s, line))
}

// ---------------------------------------------------------------- server side, helpers

fn docker_out(container: &str, cmd: &str) -> Vec<u8> {
    Proc::new("docker")
        .args(["exec", container, "sh", "-c", cmd])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default()
}

fn server_sha(container: &str, path: &str) -> String {
    let out = docker_out(container, &format!("sha256sum '/srv/share/{path}'"));
    String::from_utf8_lossy(&out).split(' ').next().unwrap_or("").to_string()
}

fn server_names(container: &str, dir: &str) -> BTreeSet<Vec<u8>> {
    docker_out(container, &format!("ls -A '/srv/share/{dir}'"))
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| l.to_vec())
        .collect()
}

fn ctl(args: &[&str]) {
    let _ = Proc::new(CTL).args(args).status();
}

fn rss_reset() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn rss_peak_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// sha256 computed on its own thread so hashing does not slow the transfer loop.
struct Hasher {
    tx: Option<SyncSender<Vec<u8>>>,
    h: std::thread::JoinHandle<String>,
}

impl Hasher {
    fn new() -> Self {
        let (tx, rx) = sync_channel::<Vec<u8>>(16);
        let h = std::thread::spawn(move || {
            let mut h = Sha256::new();
            for b in rx {
                h.update(&b);
            }
            hex(&h.finalize())
        });
        Hasher { tx: Some(tx), h }
    }
    fn push(&self, b: Vec<u8>) {
        let tx = self.tx.as_ref().unwrap();
        tokio::task::block_in_place(|| tx.send(b).ok());
    }
    fn finish(mut self) -> String {
        drop(self.tx.take());
        self.h.join().unwrap()
    }
}

/// Streamed download; returns (sha256, bytes).
async fn download(conn: &mut Connection, tree: &Tree, path: &str) -> R<(String, u64)> {
    let mut d = tree.download(conn, path).await?;
    let h = Hasher::new();
    let mut n = 0u64;
    while let Some(c) = d.next_chunk().await {
        let c = c?;
        n += c.len() as u64;
        h.push(c);
    }
    Ok((h.finish(), n))
}

fn fill(buf: &mut [u8], state: &mut u64) {
    for ch in buf.chunks_mut(8) {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        ch.copy_from_slice(&state.to_le_bytes()[..ch.len()]);
    }
}

/// Streamed upload of `size` pseudo-random bytes; returns (sha256, bytes confirmed).
async fn upload(conn: &Connection, tree: &Arc<Tree>, path: &str, size: u64) -> R<(String, u64)> {
    let mut w = tree.create_file_writer(conn.clone(), path).await?;
    let h = Hasher::new();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut left = size;
    while left > 0 {
        let n = left.min(1 << 20) as usize;
        let mut buf = vec![0u8; n];
        fill(&mut buf, &mut state);
        w.write_chunk(&buf).await?;
        h.push(buf);
        left -= n as u64;
    }
    let written = w.finish().await?;
    Ok((h.finish(), written))
}

fn kind_of(e: &(dyn Error + Send + Sync + 'static)) -> String {
    match e.downcast_ref::<smb2::Error>() {
        Some(se) => format!(
            "{:?} status={}",
            se.kind(),
            se.status().map(|s| s.to_string()).unwrap_or("-".into())
        ),
        None => format!("{e}"),
    }
}

// ---------------------------------------------------------------- raw requests

fn create_req(path: &str, access: u32, options: u32, disp: CreateDisposition) -> CreateRequest {
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
        create_disposition: disp,
        create_options: options,
        name: path.replace('/', "\\"),
        create_contexts: vec![],
    }
}

const FILE_DIRECTORY_FILE: u32 = 1;

/// Directory-exists check: CREATE (directory only) + CLOSE, as a compound or as two requests.
async fn dircheck(conn: &Connection, tid: TreeId, path: &str, access: u32, compound: bool) -> R<NtStatus> {
    let create = create_req(path, access, FILE_DIRECTORY_FILE, CreateDisposition::FileOpen);
    if compound {
        let close = CloseRequest { flags: 0, file_id: FileId::SENTINEL };
        let ops = [
            CompoundOp::new(Command::Create, &create, Some(tid)),
            CompoundOp::new(Command::Close, &close, Some(tid)),
        ];
        let mut res = conn.execute_compound(&ops).await?;
        return Ok(match res.remove(0) {
            Ok(f) => f.header.status,
            Err(e) => e.status().ok_or(e)?,
        });
    }
    let f = conn.execute(Command::Create, &create, Some(tid)).await?;
    if f.header.status == NtStatus::SUCCESS {
        let id = CreateResponse::unpack(&mut ReadCursor::new(&f.body))?.file_id;
        conn.execute(Command::Close, &CloseRequest { flags: 0, file_id: id }, Some(tid))
            .await?;
    }
    Ok(f.header.status)
}

/// Every share (hidden ones included) through the crate's public srvsvc building blocks.
/// The crate's own `list_shares` drops `$` shares and non-disk shares.
async fn all_shares(conn: &mut Connection) -> R<Vec<ShareInfo>> {
    let ipc = Tree::connect(conn, "IPC$").await?;
    let tid = Some(ipc.tree_id);
    let create = create_req(
        "srvsvc",
        FileAccessMask::FILE_READ_DATA | FileAccessMask::FILE_WRITE_DATA,
        0,
        CreateDisposition::FileOpen,
    );
    let f = conn.execute(Command::Create, &create, tid).await?;
    if f.header.status != NtStatus::SUCCESS {
        return Err(format!("open srvsvc: {}", f.header.status).into());
    }
    let fid = CreateResponse::unpack(&mut ReadCursor::new(&f.body))?.file_id;
    let write = |data: Vec<u8>| WriteRequest {
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
    let read_msg = async |conn: &Connection| -> R<Vec<u8>> {
        let mut msg = Vec::new();
        loop {
            let req = ReadRequest {
                padding: 0x50,
                flags: 0,
                length: 65536,
                offset: 0,
                file_id: fid,
                minimum_count: 0,
                channel: 0,
                remaining_bytes: 0,
                read_channel_info: vec![],
            };
            let f = conn.execute(Command::Read, &req, tid).await?;
            let st = f.header.status;
            if !st.is_success_or_partial() {
                return Err(format!("pipe read: {st}").into());
            }
            msg.extend(ReadResponse::unpack(&mut ReadCursor::new(&f.body))?.data);
            if st != NtStatus::BUFFER_OVERFLOW {
                return Ok(msg);
            }
        }
    };
    conn.execute(Command::Write, &write(smb2::rpc::build_srvsvc_bind(1)), tid).await?;
    smb2::rpc::parse_bind_ack(&read_msg(conn).await?)?;
    let server = format!(r"\\{}", conn.server_name());
    conn.execute(
        Command::Write,
        &write(smb2::rpc::srvsvc::build_net_share_enum_all(2, &server)),
        tid,
    )
    .await?;
    let mut stub = Vec::new();
    loop {
        let pdu = read_msg(conn).await?;
        let (s, last) = smb2::rpc::parse_response_fragment(&pdu)?;
        stub.extend_from_slice(s);
        if last {
            break;
        }
    }
    conn.execute(Command::Close, &CloseRequest { flags: 0, file_id: fid }, tid).await?;
    ipc.disconnect(conn).await?;
    // The stub parser is pub(crate); wrap the reassembled stub in one synthetic RESPONSE PDU.
    let mut pdu = vec![5, 0, 2, 3, 0x10, 0, 0, 0];
    pdu.extend(((24 + stub.len()) as u16).to_le_bytes());
    pdu.extend([0, 0]);
    pdu.extend(2u32.to_le_bytes());
    pdu.extend((stub.len() as u32).to_le_bytes());
    pdu.extend([0, 0, 0, 0]);
    if 24 + stub.len() > u16::MAX as usize {
        return Err(format!("stub {} bytes does not fit one PDU", stub.len()).into());
    }
    pdu.extend(stub);
    Ok(smb2::rpc::srvsvc::parse_net_share_enum_all_response(&pdu)?)
}

fn names(es: &[smb2::DirectoryEntry]) -> BTreeSet<Vec<u8>> {
    es.iter()
        .filter(|e| e.name != "." && e.name != "..")
        .map(|e| e.name.as_bytes().to_vec())
        .collect()
}

fn set_cmp(got: &BTreeSet<Vec<u8>>, want: &BTreeSet<Vec<u8>>) -> String {
    if got == want {
        return format!("set equal ({})", got.len());
    }
    let extra: Vec<String> = got.difference(want).take(3).map(|b| String::from_utf8_lossy(b).into()).collect();
    let missing: Vec<String> = want.difference(got).take(3).map(|b| String::from_utf8_lossy(b).into()).collect();
    format!(
        "SET DIFFERS got {} want {}; extra {:?} missing {:?}",
        got.len(),
        want.len(),
        extra,
        missing
    )
}

// ---------------------------------------------------------------- steps

async fn step1() -> R<()> {
    for addr in [FAST, SLOW] {
        let e = ep(addr).await;
        let t = timed!(5, {
            open(addr, "share").await?;
        });
        let tap = Tap::start(addr)?;
        tap.reset();
        let ((c, tree), _, line) = count(&tap, open(&tap.addr().to_string(), "share")).await?;
        let n = tree.list_directory(&mut c.clone(), "").await?.len();
        row(1, "NTLM TESTDOM\\tester + tree connect share", &e, "OK", &format!("connect+login+tcon median {} | {line} | root entries {n}", ms(t)));
    }
    let e = ep(FAST).await;
    // Guest and anonymous: read guest/hello.txt, be refused on share.
    for (what, user) in [("guest (user 'Guest')", "Guest"), ("anonymous (user '')", "")] {
        let r: R<String> = async {
            let mut c = login(dial(FAST).await?, user, "", "").await?;
            let g = Tree::connect(&mut c, "guest").await?;
            let data = g.read_file_compound(&mut c, "hello.txt").await?;
            let on_share = match Tree::connect(&mut c, "share").await {
                Ok(_) => "share ALLOWED (unexpected)".to_string(),
                Err(e) => format!("share refused: {e}"),
            };
            Ok(format!("read hello.txt {} bytes; {on_share}", data.len()))
        }
        .await;
        match r {
            Ok(d) => row(1, &format!("login {what}"), &e, "OK", &d),
            Err(err) => row(1, &format!("login {what}"), &e, "FAIL", &kind_of(err.as_ref())),
        }
    }
    // Unknown user with a password: Samba maps to guest; the crate refuses that on purpose.
    match login(dial(FAST).await?, "nosuchuser", "x", DOM).await {
        Ok(_) => row(1, "login unknown user (map to guest)", &e, "accepted as guest", ""),
        Err(err) => row(1, "login unknown user (map to guest)", &e, "refused", &kind_of(err.as_ref())),
    }
    match login(dial(FAST).await?, USER, "wrong", DOM).await {
        Ok(_) => row(1, "wrong password", &e, "FAIL accepted", ""),
        Err(err) => row(1, "wrong password", &e, "OK refused", &kind_of(err.as_ref())),
    }
    // Encrypted share, counted through the tap: frames must be opaque ENCRYPTED.
    let r: R<String> = async {
        let tap = Tap::start(FAST)?;
        let mut c = login(dial(&tap.addr().to_string()).await?, USER, PASS, DOM).await?;
        let ((sha, n), s, _) = count(&tap, async {
            let t = Tree::connect(&mut c, "encrypted").await?;
            let n = t.list_directory(&mut c.clone(), "").await?.len();
            let (sha, _) = download(&mut c.clone(), &t, "sizes/65537.bin").await?;
            Ok((sha, n))
        })
        .await?;
        let want = server_sha("rsmb-samba", "sizes/65537.bin");
        Ok(format!(
            "dialect {:?} cipher {:?}; root {n} entries; sizes/65537.bin sha {}; ENCRYPTED frames {} SMB2-plain req {}",
            c.params().map(|p| p.dialect),
            c.params().and_then(|p| p.cipher),
            if sha == want { "match" } else { "MISMATCH" },
            s.per_command.get("ENCRYPTED").copied().unwrap_or(0),
            s.requests
        ))
    }
    .await;
    match r {
        Ok(d) => row(1, "encrypted share", &e, if d.contains("MISMATCH") { "FAIL" } else { "OK" }, &d),
        Err(err) => row(1, "encrypted share", &e, "FAIL", &kind_of(err.as_ref())),
    }
    // SOCKS5, proxy-resolved hostname.
    let hosts = Proc::new("getent").args(["hosts", PRIVATE]).output()?;
    for (port, auth) in [(1080u16, None), (1081, Some((USER, PASS)))] {
        let log = || {
            let o = Proc::new("docker").args(["logs", "rsmb-socks"]).output().unwrap();
            let all = [o.stdout, o.stderr].concat();
            String::from_utf8_lossy(&all).matches(&format!("connected to {PRIVATE}:445")).count()
        };
        let before = log();
        let r: R<String> = async {
            let t0 = Instant::now();
            let mut c = login(dial_socks(port, auth).await?, USER, PASS, DOM).await?;
            let t = Tree::connect(&mut c, "share").await?;
            let n = t.list_directory(&mut c.clone(), "").await?.len();
            Ok(format!("login+tcon+ls {} root entries {n}", ms(t0.elapsed())))
        }
        .await;
        let after = log();
        let detail = format!(
            "{}; proxy log '+{}' lines 'connected to {PRIVATE}:445'; name resolves on host: {}",
            r.as_ref().map(|s| s.clone()).unwrap_or_else(|e| kind_of(e.as_ref())),
            after - before,
            hosts.status.success()
        );
        let ok = r.is_ok() && after > before && !hosts.status.success();
        row(1, &format!("SOCKS5 :{port} {}", if auth.is_some() { "user/pass" } else { "no auth" }), "socks 10.213.45.20", if ok { "OK" } else { "FAIL" }, &detail);
    }
    Ok(())
}

async fn step2() -> R<()> {
    for addr in [FAST, EDGE, SLOW] {
        let e = ep(addr).await;
        let (mut c, _) = open(addr, "share").await?;
        let crate_list = smb2::client::list_shares(&mut c).await?;
        let all = all_shares(&mut c).await?;
        let edge = all.iter().filter(|s| s.name.starts_with("edge")).count();
        let hidden = all.iter().filter(|s| s.name.ends_with('$')).count();
        let sample: Vec<String> = all
            .iter()
            .filter(|s| s.name == "edge010$" || s.name == "share" || s.name == "IPC$")
            .map(|s| format!("{}(type {:#x}, '{}')", s.name, s.share_type, s.comment))
            .collect();
        let t = timed!(5, {
            all_shares(&mut c).await?;
        });
        let tap = Tap::start(addr)?;
        let (mut tc, _) = open(&tap.addr().to_string(), "share").await?;
        let (_, _, line) = count(&tap, all_shares(&mut tc)).await?;
        let ok = addr != EDGE || edge == 300;
        row(
            2,
            "share list",
            &e,
            if ok { "OK" } else { "FAIL" },
            &format!(
                "crate list_shares: {} (no $ shares); full srvsvc: {} total, {edge} edge*, {hidden} with $; {}; time {} | {line}",
                crate_list.len(),
                all.len(),
                sample.join(" "),
                ms(t)
            ),
        );
    }
    Ok(())
}

async fn step3() -> R<()> {
    let cases = [
        ("tree", "existing dir"),
        ("nope", "missing"),
        ("sizes/1.bin", "file"),
        ("forbidden", "forbidden 0700"),
        ("traverse-only", "0711 dir"),
    ];
    let masks = [
        ("READ_ATTRIBUTES|SYNC", FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE),
        ("READ_DATA|READ_ATTRIBUTES|SYNC (crate's open_directory)", FileAccessMask::FILE_READ_DATA | FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE),
        ("MAXIMUM_ALLOWED", FileAccessMask::MAXIMUM_ALLOWED),
    ];
    let (c, tree) = open(FAST, "share").await?;
    let e = ep(FAST).await;
    for (mname, mask) in masks {
        let mut out = Vec::new();
        let mut right = true;
        let mut forbidden = String::new();
        for (path, label) in cases {
            let st = dircheck(&c, tree.tree_id, path, mask, true).await?;
            let plain = dircheck(&c, tree.tree_id, path, mask, false).await?;
            let ok = match path {
                "tree" | "traverse-only" => st == NtStatus::SUCCESS,
                "nope" => st == NtStatus::OBJECT_NAME_NOT_FOUND,
                "sizes/1.bin" => st == NtStatus::NOT_A_DIRECTORY,
                _ => st == NtStatus::SUCCESS || st == NtStatus::ACCESS_DENIED,
            };
            right &= ok && st == plain;
            if path == "forbidden" {
                forbidden = (if st == NtStatus::ACCESS_DENIED { "denied" } else { "enterable" }).to_string();
            }
            out.push(format!("{label}={st}{}", if ok { "" } else { "(WRONG)" }));
        }
        row(3, &format!("dir check mask {mname}"), &e, &if right { format!("right (forbidden: {forbidden})") } else { "WRONG".to_string() }, &out.join(" "));
    }
    // Crate's stat as reference for what it answers.
    let mut out = Vec::new();
    for (path, label) in cases {
        let r = tree.stat(&mut c.clone(), path).await;
        out.push(match r {
            Ok(i) => format!("{label}=is_dir {}", i.is_directory),
            Err(err) => format!("{label}={}", err.status().map(|s| s.to_string()).unwrap_or(err.to_string())),
        });
    }
    row(3, "crate Tree::stat (reference)", &e, "-", &out.join(" "));
    // Requests and flights (tap), and time (direct), for the chosen mask on the existing dir.
    let mask = FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE;
    for addr in [FAST, SLOW] {
        let e = ep(addr).await;
        let (dc, dt) = open(addr, "share").await?;
        let tap = Tap::start(addr)?;
        let (tc, tt) = open(&tap.addr().to_string(), "share").await?;
        for (what, kind) in [("compound CREATE+CLOSE", 0), ("plain CREATE then CLOSE", 1), ("crate Tree::stat", 2)] {
            let run = async |c: &Connection, t: &Arc<Tree>| -> R<()> {
                match kind {
                    0 => drop(dircheck(c, t.tree_id, "tree/d0/d1", mask, true).await?),
                    1 => drop(dircheck(c, t.tree_id, "tree/d0/d1", mask, false).await?),
                    _ => drop(t.stat(&mut c.clone(), "tree/d0/d1").await?),
                }
                Ok(())
            };
            let (_, s, line) = count(&tap, run(&tc, &tt)).await?;
            let d = timed!(5, {
                run(&dc, &dt).await?;
            });
            let gate = kind == 0 && s.requests <= 2;
            row(3, &format!("cd check: {what}"), &e, &ms(d), &format!("{line}{}", if gate { " (gate shape)" } else { "" }));
        }
    }
    Ok(())
}

async fn step4() -> R<()> {
    // Largest buffer the server accepts.
    let (c, tree) = open(FAST, "share").await?;
    let p = c.params().unwrap();
    let mut maxbuf = p.max_transact_size;
    while maxbuf > 65536 {
        if tree.list_directory_instrumented(&mut c.clone(), "sizes", Some(maxbuf)).await.is_ok() {
            break;
        }
        maxbuf /= 2;
    }
    row(4, "largest QUERY_DIRECTORY buffer accepted", &ep(FAST).await, &maxbuf.to_string(), &format!("negotiated max_transact {} max_read {} max_write {} dialect {:?}", p.max_transact_size, p.max_read_size, p.max_write_size, p.dialect));
    drop((c, tree));
    for dir in ["sizes", "many", "huge", "longnames"] {
        let want = server_names("rsmb-samba", dir);
        for buf in [None, Some(maxbuf)] {
            let bl = buf.map(|b| b.to_string()).unwrap_or("default".into());
            for addr in [FAST, SLOW] {
                let e = ep(addr).await;
                // Counted: fresh connection, the listing is the first thing after connect.
                let tap = Tap::start(addr)?;
                let (tc, tt) = open(&tap.addr().to_string(), "share").await?;
                let credits = tc.credits();
                let ((es, tr), _, line) = count(&tap, async {
                    Ok(tt.list_directory_instrumented(&mut tc.clone(), dir, buf).await?)
                })
                .await?;
                let cmp = set_cmp(&names(&es), &want);
                drop(tap);
                // Timed, direct. Fresh: the first listing on a newly opened connection (TCP slow
                // start still applies). Warm: median of 3 further listings on the same connection.
                let (dc, dt) = open(addr, "share").await?;
                let t0 = Instant::now();
                dt.list_directory_instrumented(&mut dc.clone(), dir, buf).await?;
                let fresh = t0.elapsed();
                let d = timed!(3, {
                    dt.list_directory_instrumented(&mut dc.clone(), dir, buf).await?;
                });
                // Peak RSS of one listing in a fresh process (the allocator keeps freed memory).
                let out = Proc::new(std::env::current_exe()?)
                    .args(["rss", dir, &buf.unwrap_or(0).to_string()])
                    .output()?;
                let rss: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(-1.0);
                row(
                    4,
                    &format!("ls {dir} buf={bl}"),
                    &e,
                    &format!("warm {} / fresh {}", ms(d), ms(fresh)),
                    &format!("{cmp}; {} queries, buffer {}; credits before first ls {credits}; peak RSS +{rss:.1} MB over post-login baseline, fresh process; warm = median of 3 listings on a connection that has already listed, fresh = first listing on a new connection | {line}", tr.queries.len(), tr.query_buffer_len),
                );
            }
        }
    }
    Ok(())
}

async fn step5() -> R<()> {
    let e = ep(FAST).await;
    let (mut c, tree) = open(FAST, "share").await?;
    // sizes/: every file, checksum against the server.
    let mut bad = Vec::new();
    let all = tree.list_directory(&mut c, "sizes").await?;
    let mut n = 0;
    for f in all.iter().filter(|e| !e.is_directory) {
        let p = format!("sizes/{}", f.name);
        let (sha, len) = download(&mut c, &tree, &p).await?;
        n += 1;
        if sha != server_sha("rsmb-samba", &p) || len != f.size {
            bad.push(p);
        }
    }
    row(5, "read every file in sizes/", &e, if bad.is_empty() { "OK" } else { "FAIL" }, &format!("{n} files, sha256 vs server; bad {bad:?}"));
    // big.bin x5 at the fast endpoint, cache evicted before each run.
    let want = server_sha("rsmb-samba", BIG);
    let mut times = Vec::new();
    for i in 1..=5 {
        ctl(&["evict", "samba", "/srv/share/large/big.bin"]);
        rss_reset();
        let t0 = Instant::now();
        let (sha, len) = to(600, download(&mut c, &tree, BIG)).await?;
        let d = t0.elapsed();
        times.push(d);
        row(5, &format!("big.bin download run {i}"), &e, &mbps(len, d), &format!("{} in {}; sha {}; peak RSS {:.0} MB", len, ms(d), if sha == want && len == BIG_SIZE { "match" } else { "MISMATCH" }, rss_peak_mb()));
    }
    let mut s = times.clone();
    s.sort();
    let med = s[2];
    let worst = *s.last().unwrap();
    row(5, "big.bin x5 spread", &e, if worst <= med * 3 { "OK" } else { "FAIL" }, &format!("median {} worst {} (limit 3x median)", ms(med), ms(worst)));
    // Counted run through the tap (interims, concurrency).
    ctl(&["evict", "samba", "/srv/share/large/big.bin"]);
    let tap = Tap::start(FAST)?;
    let (mut tc, tt) = open(&tap.addr().to_string(), "share").await?;
    let ((sha, _), _, line) = count(&tap, to(600, download(&mut tc, &tt, BIG))).await?;
    row(5, "big.bin download via tap", &e, if sha == want { "sha match" } else { "MISMATCH" }, &line);
    drop(tap);
    // Slow endpoint, one run, against the iperf3 ceiling.
    let es = ep(SLOW).await;
    let ip = Proc::new("iperf3").args(["-c", "10.213.45.30", "-p", "5201", "-R", "-t", "10", "-f", "M"]).output()?;
    let ipl = String::from_utf8_lossy(&ip.stdout).lines().filter(|l| l.contains("receiver")).last().unwrap_or("").to_string();
    let (mut sc, st) = open(SLOW, "share").await?;
    ctl(&["evict", "samba", "/srv/share/large/big.bin"]);
    let t0 = Instant::now();
    let (sha, len) = to(900, download(&mut sc, &st, BIG)).await?;
    let d = t0.elapsed();
    row(5, "big.bin download slow", &es, &mbps(len, d), &format!("{} in {}; sha {}; iperf3 -R 10 s: {}", len, ms(d), if sha == want { "match" } else { "MISMATCH" }, ipl.split_whitespace().collect::<Vec<_>>().join(" ")));
    // A block above 4 GiB.
    let off = 4_294_979_641u64;
    let len = 1_048_579u64;
    let r = tree.open_file_reader(c.clone(), "large/over4g.bin").await?;
    let data = r.read_at(off, len).await?;
    r.close().await?;
    let got = hex(&Sha256::digest(&data));
    let want = String::from_utf8_lossy(&docker_out("rsmb-samba", &format!("dd if=/srv/share/large/over4g.bin iflag=skip_bytes,count_bytes skip={off} count={len} bs=1M status=none | sha256sum"))).split(' ').next().unwrap_or("").to_string();
    row(5, "over4g.bin block at 4 GiB+12345", &e, if got == want && data.len() as u64 == len { "OK" } else { "FAIL" }, &format!("{} bytes at offset {off}, sha256 vs server dd", data.len()));
    Ok(())
}

async fn step6() -> R<()> {
    let e = ep(FAST).await;
    let (mut c, tree) = open(FAST, "share").await?;
    let _ = tree.create_directory(&mut c, UP).await;
    let mut times = Vec::new();
    for i in 1..=3 {
        let p = format!("{UP}/big-{i}.bin");
        rss_reset();
        let t0 = Instant::now();
        let (sha, n) = to(600, upload(&c, &tree, &p, BIG_SIZE)).await?;
        let d = t0.elapsed();
        times.push(d);
        let srv = server_sha("rsmb-samba", &p);
        row(6, &format!("upload 1.1 GiB run {i}"), &e, &mbps(n, d), &format!("{n} bytes in {}; sha {}; peak RSS {:.0} MB", ms(d), if srv == sha && n == BIG_SIZE { "match" } else { "MISMATCH" }, rss_peak_mb()));
        tree.delete_file(&mut c, &p).await?;
    }
    let mut s = times.clone();
    s.sort();
    row(6, "upload x3 spread", &e, if s[2] <= s[1] * 3 { "OK" } else { "FAIL" }, &format!("median {} worst {}", ms(s[1]), ms(s[2])));
    // Counted 50 MiB upload (pipelining shape).
    let tap = Tap::start(FAST)?;
    let (tc, tt) = open(&tap.addr().to_string(), "share").await?;
    let p = format!("{UP}/counted.bin");
    let (_, _, line) = count(&tap, upload(&tc, &tt, &p, 52_432_897)).await?;
    row(6, "upload 50 MiB via tap", &e, "-", &line);
    tree.delete_file(&mut c, &p).await?;
    // Slow endpoint, one 1.1 GiB run, against the iperf3 ceiling (client -> server).
    let es = ep(SLOW).await;
    let ip = Proc::new("iperf3").args(["-c", "10.213.45.30", "-p", "5201", "-t", "10", "-f", "M"]).output()?;
    let ipl = String::from_utf8_lossy(&ip.stdout).lines().filter(|l| l.contains("receiver")).last().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    let (sc, st) = open(SLOW, "share").await?;
    let t0 = Instant::now();
    let (sha, n) = to(900, upload(&sc, &st, &p, BIG_SIZE)).await?;
    let d = t0.elapsed();
    row(6, "upload 1.1 GiB slow", &es, &mbps(n, d), &format!("{n} bytes in {}; sha {}; iperf3 10 s: {ipl}", ms(d), if sha == server_sha("rsmb-samba", &p) { "match" } else { "MISMATCH" }));
    tree.delete_file(&mut c, &p).await?;
    // Rename, delete, mkdir.
    let a = format!("{UP}/a.txt");
    let b = format!("{UP}/b.txt");
    tree.write_file(&mut c, &a, b"aaa").await?;
    tree.write_file(&mut c, &b, b"bbb").await?;
    let r1 = tree.rename_with(&mut c, &a, &b, RenameOptions::new().replace_if_exists(false)).await;
    let r2 = tree.rename_with(&mut c, &a, &b, RenameOptions::new().replace_if_exists(true)).await;
    let content = tree.read_file_compound(&mut c, &b).await?;
    row(6, "rename onto existing, no replace", &e, if r1.is_err() { "OK refused" } else { "FAIL replaced" }, &r1.err().map(|x| format!("{:?} {x}", x.kind())).unwrap_or_default());
    row(6, "rename onto existing, replace", &e, if r2.is_ok() && content == b"aaa" { "OK" } else { "FAIL" }, &format!("{:?}", r2.err()));
    let d = format!("{UP}/newdir");
    let mk = tree.create_directory(&mut c, &d).await;
    let st = tree.stat(&mut c, &d).await.map(|i| i.is_directory);
    let rd = tree.delete_directory(&mut c, &d).await;
    let del = tree.delete_file(&mut c, &b).await;
    let gone = tree.stat(&mut c, &b).await.is_err();
    row(6, "mkdir / rmdir / delete", &e, if mk.is_ok() && st.as_ref().is_ok_and(|x| *x) && rd.is_ok() && del.is_ok() && gone { "OK" } else { "FAIL" }, &format!("mkdir {:?} is_dir {:?} rmdir {:?} delete {:?} gone {gone}", mk.is_ok(), st.ok(), rd.is_ok(), del.is_ok()));
    tree.delete_directory(&mut c, UP).await?;
    Ok(())
}

/// Breadth-first walk with at most `width` listings in flight; returns (ok, err, dirs per depth).
async fn walk(conn: &Connection, tree: &Arc<Tree>, root: &str, width: usize, limit: usize) -> (usize, usize, Vec<usize>) {
    let sem = Arc::new(Semaphore::new(width));
    let mut set = JoinSet::new();
    let spawn = |set: &mut JoinSet<_>, path: String, depth: usize| {
        let (c, t, s) = (conn.clone(), tree.clone(), sem.clone());
        set.spawn(async move {
            let _p = s.acquire_owned().await.unwrap();
            let r = t.list_directory(&mut c.clone(), &path).await;
            (path, depth, r)
        });
    };
    spawn(&mut set, root.to_string(), 0);
    let (mut started, mut ok, mut err) = (1usize, 0usize, 0usize);
    let mut levels = vec![0usize; 1];
    while let Some(j) = set.join_next().await {
        let (path, depth, r) = j.unwrap();
        if levels.len() <= depth {
            levels.resize(depth + 1, 0);
        }
        levels[depth] += 1;
        match r {
            Ok(es) => {
                ok += 1;
                for en in es.iter().filter(|e| e.is_directory && e.name != "." && e.name != "..") {
                    if started < limit {
                        started += 1;
                        spawn(&mut set, format!("{path}/{}", en.name), depth + 1);
                    }
                }
            }
            Err(_) => err += 1,
        }
    }
    (ok, err, levels)
}

async fn step7() -> R<()> {
    let e = ep(SLOW).await;
    let rtt = 0.2;
    for (root, seq_limit) in [("tree", 40usize), ("fanout", 40)] {
        // Counted through the tap at the slow endpoint (16 wide).
        let tap = Tap::start(SLOW)?;
        let (tc, tt) = open(&tap.addr().to_string(), "share").await?;
        // One listing, to know the flights per listing.
        let (_, s1, _) = count(&tap, async { Ok(tt.list_directory(&mut tc.clone(), &format!("{root}/{}", if root == "tree" { "d0" } else { "d0001" })).await?) }).await?;
        let per = s1.flights as f64;
        let warm = {
            let mut set = JoinSet::new();
            for _ in 0..32 {
                let (c, t) = (tc.clone(), tt.clone());
                set.spawn(async move { t.stat(&mut c.clone(), "tree").await.is_ok() });
            }
            set.join_all().await.len()
        };
        let ((ok, err, levels), s, line) = count(&tap, async { Ok(walk(&tc, &tt, root, 16, usize::MAX).await) }).await?;
        let walk_conns = s.request_log.iter().map(|r| r.conn).collect::<BTreeSet<_>>().len();
        let live = tap.live_connections();
        drop(tap);
        let waves: usize = levels.iter().map(|n| n.div_ceil(16)).sum();
        // Timed direct.
        let (dc, dt) = open(SLOW, "share").await?;
        let credits_before = dc.credits();
        let mut set = JoinSet::new();
        for _ in 0..32 {
            let (c, t) = (dc.clone(), dt.clone());
            set.spawn(async move { t.stat(&mut c.clone(), "tree").await.is_ok() });
        }
        set.join_all().await;
        let credits_warm = dc.credits();
        let t0 = Instant::now();
        walk(&dc, &dt, root, 16, usize::MAX).await;
        let d16 = t0.elapsed();
        let t0 = Instant::now();
        let (sok, serr, _) = walk(&dc, &dt, root, 1, seq_limit).await;
        let dseq = t0.elapsed();
        let per_listing = dseq.as_secs_f64() / (sok + serr) as f64;
        let n = ok + err;
        let ideal = levels.len() as f64 * per * rtt;
        let bound16 = waves as f64 * per * rtt;
        let seq_full = per_listing * n as f64;
        row(
            7,
            &format!("walk {root}/ 16 wide"),
            &e,
            &format!("{:.1} s", d16.as_secs_f64()),
            &format!(
                "{n} dirs ({err} denied), levels {levels:?}; {per} flights/listing; ideal levels x listing {ideal:.1} s; 16-wide bound (waves {waves}) {bound16:.1} s; sequential {per_listing:.2} s/listing x {n} = {seq_full:.0} s (extrapolated from {} listings); speedup {:.1}x; credits {credits_before} -> {credits_warm} after warm-up ({warm} stats); TCP connections: {walk_conns} carried the walk's requests (distinct connection ids in the tap's request log), {live} live through the tap; tap: {line}",
                sok + serr,
                seq_full / d16.as_secs_f64()
            ),
        );
    }
    Ok(())
}

async fn step8() -> R<()> {
    let e = ep(EDGE).await;
    let tap = Tap::start(EDGE)?;
    let before = String::from_utf8_lossy(&Proc::new(CTL).args(["conns", "samba-edge"]).output()?.stdout).trim().parse::<i64>().unwrap_or(-1);
    let (c, tree) = open(&tap.addr().to_string(), "share").await?;
    let p = c.params().unwrap();
    let ((ok, t), _, line) = count(&tap, to(120, async {
        let t0 = Instant::now();
        let mut set = JoinSet::new();
        for i in 1..=64 {
            let (c, t) = (c.clone(), tree.clone());
            set.spawn(async move { t.stat(&mut c.clone(), &format!("fanout/d{i:04}/file.txt")).await.is_ok() });
        }
        let ok = set.join_all().await.iter().filter(|x| **x).count();
        Ok((ok, t0.elapsed()))
    }))
    .await?;
    row(8, "64 parallel stats on 16 credits", &e, if ok == 64 { "OK" } else { "FAIL" }, &format!("{ok}/64 in {}; max read/write/transact {}/{}/{}; credits now {} | {line}", ms(t), p.max_read_size, p.max_write_size, p.max_transact_size, c.credits()));
    let ((ok, t), _, line) = count(&tap, to(120, async {
        let t0 = Instant::now();
        let mut set = JoinSet::new();
        for i in 1..=64 {
            let (c, t) = (c.clone(), tree.clone());
            set.spawn(async move { t.list_directory(&mut c.clone(), &format!("fanout/d{i:04}")).await.is_ok() });
        }
        let ok = set.join_all().await.iter().filter(|x| **x).count();
        Ok((ok, t0.elapsed()))
    }))
    .await?;
    row(8, "64 parallel listings", &e, if ok == 64 { "OK" } else { "FAIL" }, &format!("{ok}/64 in {} | {line}", ms(t)));
    let want = server_sha("rsmb-samba-edge", "large/medium.bin");
    let ((sha, n), _, line) = count(&tap, to(180, download(&mut c.clone(), &tree, "large/medium.bin"))).await?;
    row(8, "download medium.bin 50 MiB", &e, if sha == want { "OK sha match" } else { "FAIL" }, &format!("{n} bytes | {line}"));
    let _ = tree.create_directory(&mut c.clone(), UP).await;
    let up = format!("{UP}/edge.bin");
    let ((sha, n), _, line) = count(&tap, to(180, upload(&c, &tree, &up, 52_432_897))).await?;
    let srv = server_sha("rsmb-samba-edge", &up);
    row(8, "upload 50 MiB", &e, if sha == srv { "OK sha match" } else { "FAIL" }, &format!("{n} bytes | {line}"));
    tree.delete_file(&mut c.clone(), &up).await?;
    tree.delete_directory(&mut c.clone(), UP).await?;
    let after = String::from_utf8_lossy(&Proc::new(CTL).args(["conns", "samba-edge"]).output()?.stdout).trim().parse::<i64>().unwrap_or(-1);
    let live = tap.live_connections();
    row(8, "TCP connections", &e, if live == 1 { "1" } else { "NOT 1" }, &format!("live connections through the tap at the end {live} (multichannel advertised; the crate cannot dial on its own with a caller transport); server-side counter delta {} (shared environment: includes other clients)", after - before));
    Ok(())
}

async fn step9() -> R<()> {
    let e = ep(FAST).await;
    let (mut c, tree) = open(FAST, "share").await?;
    for dir in ["cjk", "special"] {
        let es = tree.list_directory(&mut c, dir).await?;
        let cmp = set_cmp(&names(&es), &server_names("rsmb-samba", dir));
        // Open every file (and walk every subdirectory) by the name the listing gave.
        let (mut files, mut bad) = (0, Vec::new());
        let mut stack = vec![dir.to_string()];
        while let Some(d) = stack.pop() {
            for en in tree.list_directory(&mut c, &d).await? {
                if en.name == "." || en.name == ".." {
                    continue;
                }
                let p = format!("{d}/{}", en.name);
                if en.is_directory {
                    stack.push(p);
                } else {
                    files += 1;
                    match tree.read_file_compound(&mut c, &p).await {
                        Ok(b) if b.len() as u64 == en.size => {}
                        Ok(b) => bad.push(format!("{p}: {} bytes, listed {}", b.len(), en.size)),
                        Err(x) => bad.push(format!("{p}: {x}")),
                    }
                }
            }
        }
        let ok = cmp.starts_with("set equal") && bad.is_empty();
        row(9, &format!("{dir}/ list + open (recursive)"), &e, if ok { "OK" } else { "FAIL" }, &format!("listing {cmp}; {files} files opened by listed name; bad {bad:?}"));
    }
    // Case pair.
    let a = tree.read_file_compound(&mut c, "special/Case.txt").await?;
    let b = tree.read_file_compound(&mut c, "special/case.txt").await?;
    row(9, "special/Case.txt vs case.txt", &e, "-", &format!("contents {:?} / {:?}", String::from_utf8_lossy(&a).trim(), String::from_utf8_lossy(&b).trim()));
    // Create: a file and a directory per name, server checks the bytes.
    let dir = format!("{UP}/names");
    let _ = tree.create_directory(&mut c, UP).await;
    tree.create_directory(&mut c, &dir).await?;
    let mut want = BTreeSet::new();
    let mut errs = Vec::new();
    let cjk: Vec<String> = tree.list_directory(&mut c, "cjk").await?.into_iter().filter(|e| e.is_directory && e.name != "." && e.name != ".." && e.name != "nested").map(|e| e.name).collect();
    let special: Vec<String> = tree.list_directory(&mut c, "special").await?.into_iter().filter(|e| !e.is_directory).map(|e| e.name).collect();
    for n in cjk.iter().chain(special.iter()) {
        if n == "case.txt" {
            continue; // Case.txt is created; the pair collapses on a case-insensitive open
        }
        match tree.write_file(&mut c, &format!("{dir}/{n}"), n.as_bytes()).await {
            Ok(_) => {
                want.insert(n.as_bytes().to_vec());
            }
            Err(x) => errs.push(format!("{n}: {x}")),
        }
    }
    for n in &cjk {
        let d = format!("dir_{n}");
        match tree.create_directory(&mut c, &format!("{dir}/{d}")).await {
            Ok(_) => {
                want.insert(d.as_bytes().to_vec());
            }
            Err(x) => errs.push(format!("{d}: {x}")),
        }
    }
    let got = server_names("rsmb-samba", &dir);
    let cmp = set_cmp(&got, &want);
    // Read back each created file and compare content to the name.
    let mut rb = 0;
    for n in cjk.iter().chain(special.iter()).filter(|n| *n != "case.txt") {
        if tree.read_file_compound(&mut c, &format!("{dir}/{n}")).await.is_ok_and(|b| b == n.as_bytes()) {
            rb += 1;
        }
    }
    row(9, "create files + dirs with cjk/special names", &e, if cmp.starts_with("set equal") && errs.is_empty() { "OK" } else { "FAIL" }, &format!("server-side bytes: {cmp}; read back {rb}; errors {errs:?}"));
    for en in tree.list_directory(&mut c, &dir).await? {
        if en.name == "." || en.name == ".." {
            continue;
        }
        let p = format!("{dir}/{}", en.name);
        if en.is_directory {
            tree.delete_directory(&mut c, &p).await?;
        } else {
            tree.delete_file(&mut c, &p).await?;
        }
    }
    tree.delete_directory(&mut c, &dir).await?;
    tree.delete_directory(&mut c, UP).await?;
    // mangled/
    let es: Vec<_> = tree.list_directory(&mut c, "mangled").await?.into_iter().filter(|e| e.name != "." && e.name != "..").collect();
    let (mut ok, mut nf, mut other) = (0, 0, Vec::new());
    for en in &es {
        match tree.read_file_compound(&mut c, &format!("mangled/{}", en.name)).await {
            Ok(_) => ok += 1,
            Err(x) if x.status() == Some(NtStatus::OBJECT_NAME_NOT_FOUND) => nf += 1,
            Err(x) => other.push(format!("{}: {x}", en.name)),
        }
    }
    let listed: Vec<&str> = es.iter().map(|e| e.name.as_str()).collect();
    row(9, "mangled/ list + open", &e, if es.len() == 9 && ok == 5 { "OK" } else { "FAIL" }, &format!("{} listed {listed:?}; opened {ok}, NOT_FOUND {nf}, refused otherwise {other:?} (smbclient: 5 open, 4 NOT_FOUND)", es.len()));
    Ok(())
}

async fn step10() -> R<()> {
    // a) tap cut while idle
    let e = ep(FAST).await;
    let tap = Tap::start(FAST)?;
    let (c, tree) = open(&tap.addr().to_string(), "share").await?;
    tap.drop_connections();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t0 = Instant::now();
    let r = to(120, async { Ok(tree.stat(&mut c.clone(), "tree").await?) }).await;
    row(10, "tap cut while idle, then stat", &e, &ms(t0.elapsed()), &format!("{}", r.err().map(|x| kind_of(x.as_ref())).unwrap_or("succeeded?!".into())));
    // b) tap cut mid-transfer
    let (c, tree) = open(&tap.addr().to_string(), "share").await?;
    let r: R<(Duration, String)> = to(120, async {
        let mut cc = c.clone();
        let mut d = tree.download(&mut cc, BIG).await?;
        let mut n = 0u64;
        let mut cut: Option<Instant> = None;
        while let Some(ch) = d.next_chunk().await {
            match ch {
                Ok(b) => n += b.len() as u64,
                Err(x) => return Ok((cut.map(|t| t.elapsed()).unwrap_or_default(), format!("error after {n} bytes: {:?} {x}", x.kind()))),
            }
            if cut.is_none() && n >= 64 << 20 {
                tap.drop_connections();
                cut = Some(Instant::now());
            }
        }
        Ok((Duration::ZERO, format!("finished {n} bytes without error")))
    })
    .await;
    match r {
        Ok((d, s)) => row(10, "tap cut mid-download (after 64 MiB)", &e, &ms(d), &s),
        Err(x) => row(10, "tap cut mid-download (after 64 MiB)", &e, "TIMEOUT/ERR", &x.to_string()),
    }
    drop(tap);
    // c) blackhole on the slow path, default timeouts and a 5 s response timeout
    let eh = ep(HOLE).await;
    for rt in [None, Some(5u64)] {
        let (c, tree) = open(HOLE, "share").await?;
        if let Some(s) = rt {
            c.set_response_timeout(Some(Duration::from_secs(s)));
        }
        ctl(&["blackhole", "on"]);
        let t0 = Instant::now();
        let r = to(240, async { Ok(tree.stat(&mut c.clone(), "tree").await?) }).await;
        let d = t0.elapsed();
        ctl(&["blackhole", "off"]);
        row(10, &format!("blackhole, stat (response timeout {})", rt.map(|s| format!("{s} s")).unwrap_or("default".into())), &eh, &ms(d), &r.err().map(|x| kind_of(x.as_ref())).unwrap_or("succeeded?!".into()));
    }
    // d) paused server
    for rt in [None, Some(5u64)] {
        let (c, tree) = open(FAST, "share").await?;
        if let Some(s) = rt {
            c.set_response_timeout(Some(Duration::from_secs(s)));
        }
        ctl(&["pause", "samba"]);
        let t0 = Instant::now();
        let r = to(240, async { Ok(tree.stat(&mut c.clone(), "tree").await?) }).await;
        let d = t0.elapsed();
        ctl(&["unpause", "samba"]);
        row(10, &format!("paused server, stat (response timeout {})", rt.map(|s| format!("{s} s")).unwrap_or("default".into())), &e, &ms(d), &r.err().map(|x| kind_of(x.as_ref())).unwrap_or("succeeded?!".into()));
    }
    // e) drop a read mid-flight, then time the next request (slow endpoint)
    let es = ep(SLOW).await;
    let tap = Tap::start(SLOW)?;
    for (label, addr) in [("direct", SLOW.to_string()), ("via tap", tap.addr().to_string())] {
        let (c, tree) = open(&addr, "share").await?;
        let base = timed!(3, {
            tree.stat(&mut c.clone(), "tree").await?;
        });
        let reader = tree.open_file_reader(c.clone(), BIG).await?;
        tap.reset();
        let dropped = tokio::time::timeout(Duration::from_millis(250), reader.read_at(0, 64 << 20)).await.is_err();
        let t0 = Instant::now();
        let r = to(120, async { Ok(tree.stat(&mut c.clone(), "tree").await?) }).await;
        let d = t0.elapsed();
        let after = to(120, async { Ok(tree.stat(&mut c.clone(), "tree").await?) }).await.is_ok();
        let extra = if label == "via tap" {
            settle(&tap);
            let s = tap.stats();
            format!(" | {}", summary(&s, 0))
        } else {
            String::new()
        };
        row(10, &format!("drop 64 MiB read after 250 ms, next stat ({label})"), &es, &ms(d), &format!("dropped {dropped}; next stat ok {}; one more ok {after}; baseline stat {}{extra}", r.is_ok(), ms(base)));
        drop(reader);
    }
    drop(tap);
    // f) idle for 2x the 5 s keepalive: anything sent unasked?
    let tap = Tap::start(FAST)?;
    let (_c, _tree) = open(&tap.addr().to_string(), "share").await?;
    settle(&tap);
    tap.reset();
    tokio::time::sleep(Duration::from_secs(12)).await;
    let s = tap.stats();
    row(10, "idle 12 s (2x keepalive 5 s)", &e, if s.requests == 0 { "OK nothing sent" } else { "SENT" }, &summary(&s, 0));
    Ok(())
}

async fn step11() -> R<()> {
    let e = ep(FAST).await;
    let (mut c, tree) = open(FAST, "share").await?;
    let rep = |what: &str, r: smb2::Result<()>, want: smb2::ErrorKind| {
        let (res, d) = match r {
            Ok(()) => ("FAIL no error".to_string(), String::new()),
            Err(x) => (
                if x.kind() == want { "OK".into() } else { format!("kind {:?}", x.kind()) },
                format!("{:?} status={} '{x}'", x.kind(), x.status().map(|s| s.to_string()).unwrap_or("-".into())),
            ),
        };
        row(11, what, &e, &res, &d);
    };
    rep("read forbidden/secret.txt", tree.read_file_compound(&mut c, "forbidden/secret.txt").await.map(drop), smb2::ErrorKind::AccessDenied);
    rep("list forbidden/", tree.list_directory(&mut c, "forbidden").await.map(drop), smb2::ErrorKind::AccessDenied);
    rep("stat missing", tree.stat(&mut c, "nope/nothing").await.map(drop), smb2::ErrorKind::NotFound);
    rep("read missing", tree.read_file_compound(&mut c, "nope.txt").await.map(drop), smb2::ErrorKind::NotFound);
    let ro = Tree::connect(&mut c, "readonly").await?;
    rep("write on readonly share", ro.write_file(&mut c, "upload/x.txt", b"x").await.map(drop), smb2::ErrorKind::AccessDenied);
    rep("write outside upload/ on share", tree.write_file(&mut c, "x.txt", b"x").await.map(drop), smb2::ErrorKind::AccessDenied);
    let tiny = Arc::new(Tree::connect(&mut c, "tiny").await?);
    let p = "spike-smb2-full.bin";
    let r = to(120, upload(&c, &tiny, p, 12 << 20)).await;
    let r = match r {
        Ok(_) => Ok(()),
        Err(x) => match x.downcast::<smb2::Error>() {
            Ok(se) => Err(*se),
            Err(x) => Err(smb2::Error::invalid_data(x.to_string())),
        },
    };
    rep("12 MiB upload to tiny (10 MiB)", r, smb2::ErrorKind::DiskFull);
    let del = tiny.delete_file(&mut c, p).await;
    row(11, "cleanup on tiny", &e, if del.is_ok() { "deleted" } else { "-" }, &format!("{:?}", del.err().map(|x| x.to_string())));
    Ok(())
}

async fn step13() -> R<()> {
    let e = ep(BAD).await;
    let t0 = Instant::now();
    let (c, tree) = to(120, open(BAD, "share")).await?;
    row(13, "connect+login+tcon", &e, &ms(t0.elapsed()), "");
    let t0 = Instant::now();
    let st = dircheck(&c, tree.tree_id, "tree/d0", FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE, true).await?;
    row(13, "cd check compound", &e, &ms(t0.elapsed()), &st.to_string());
    for dir in ["tree", "many"] {
        let t0 = Instant::now();
        let n = to(300, async { Ok(tree.list_directory(&mut c.clone(), dir).await?.len()) }).await?;
        row(13, &format!("ls {dir}"), &e, &ms(t0.elapsed()), &format!("{n} entries"));
    }
    let t0 = Instant::now();
    let (sha, n) = to(300, download(&mut c.clone(), &tree, "sizes/8388609.bin")).await?;
    let d = t0.elapsed();
    let ok = sha == server_sha("rsmb-samba", "sizes/8388609.bin");
    row(13, "read sizes/8388609.bin", &e, &mbps(n, d), &format!("{} in {}; sha {}", n, ms(d), if ok { "match" } else { "MISMATCH" }));
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "rss") {
        let buf: u32 = args[2].parse().unwrap();
        let (c, t) = open(FAST, "share").await.unwrap();
        let base = rss_peak_mb();
        let es = t
            .list_directory_instrumented(&mut c.clone(), &args[1], (buf > 0).then_some(buf))
            .await
            .unwrap();
        println!("{:.1}", rss_peak_mb() - base);
        drop(es);
        return;
    }
    let steps: Vec<u32> = args.iter().filter_map(|a| a.parse().ok()).collect();
    for s in steps {
        // Per-step budget; a hang is recorded as TIMEOUT, never left hanging.
        let budget = match s {
            5 => 1800,
            6 => 1200,
            7 => 900,
            10 => 900,
            4 => 900,
            _ => 300,
        };
        let fut: std::pin::Pin<Box<dyn Future<Output = R<()>>>> = match s {
            1 => Box::pin(step1()),
            2 => Box::pin(step2()),
            3 => Box::pin(step3()),
            4 => Box::pin(step4()),
            5 => Box::pin(step5()),
            6 => Box::pin(step6()),
            7 => Box::pin(step7()),
            8 => Box::pin(step8()),
            9 => Box::pin(step9()),
            10 => Box::pin(step10()),
            11 => Box::pin(step11()),
            13 => Box::pin(step13()),
            _ => continue,
        };
        let t0 = Instant::now();
        match tokio::time::timeout(Duration::from_secs(budget), fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => row(s, "step aborted", "-", "ERROR", &kind_of(e.as_ref())),
            Err(_) => row(s, "step", "-", "TIMEOUT", &format!("after {budget} s")),
        }
        eprintln!("step {s} took {:.1} s", t0.elapsed().as_secs_f64());
    }
}
