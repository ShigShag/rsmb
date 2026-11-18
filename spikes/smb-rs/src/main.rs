    //! Stage 0 spike for the `smb` crate (smb-rs) 0.12.1. Throwaway measurement code.
//!
//! Usage: spike-smbrs <step>...   (steps: 1 2 3 4 5 6 7 7c 8 9 10 10i 11 13)
//! Each result is one line on stdout: `ROW\tstep\titem\tendpoint\tresult\tdetail`.
//! Locking against other users of the test environment is done by run.sh, not here.

use futures::stream::{FuturesUnordered, StreamExt};
use futures::{FutureExt, future::BoxFuture, stream};
use sha2::{Digest, Sha256};
use smb::transport::{IoVec, SmbTransport, SmbTransportRead, SmbTransportWrite, TransportError};
use smb::{
    Connection, ConnectionConfig, CreateDisposition, CreateOptions, Directory, File,
    FileAccessMask, FileAttributes, FileCreateArgs, FileDirectoryInformation,
    FileDispositionInformation, FileRenameInformation, GetLen, Guid, Resource, Session, Tree,
    UncPath,
};
use smbtap::Tap;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

const FAST: &str = "10.213.45.10:445";
const SLOW: &str = "10.213.45.30:445";
const BAD: &str = "10.213.45.31:445";
const EDGE: &str = "10.213.45.11:445";
const BLACKHOLE: &str = "10.213.45.30:4450";
const SOCKS: &str = "10.213.45.20:1080";
const SOCKS_AUTH: &str = "10.213.45.20:1081";
const PRIVATE: &str = "fileserver.rsmb.test";
const UP: &str = "upload\\spike-smbrs";
const BIG_LEN: u64 = 1_181_116_007;

type R<T> = Result<T, String>;

// ---------------------------------------------------------------- transport

/// Our own transport: tokio TcpStream, TCP_NODELAY, one write per SMB frame.
struct Tcp {
    r: Option<OwnedReadHalf>,
    w: Option<OwnedWriteHalf>,
    peer: SocketAddr,
}

type TR<T> = Result<T, TransportError>;

fn io_err(e: std::io::Error) -> TransportError {
    use std::io::ErrorKind::*;
    match e.kind() {
        UnexpectedEof | ConnectionAborted | ConnectionReset | BrokenPipe => {
            TransportError::NotConnected
        }
        _ => e.into(),
    }
}

impl Tcp {
    fn new(s: TcpStream, peer: SocketAddr) -> Self {
        s.set_nodelay(true).unwrap();
        let (r, w) = s.into_split();
        Tcp { r: Some(r), w: Some(w), peer }
    }
}

impl SmbTransportRead for Tcp {
    fn receive_exact<'a>(&'a mut self, out: &'a mut [u8]) -> BoxFuture<'a, TR<()>> {
        async move {
            let r = self.r.as_mut().ok_or(TransportError::NotConnected)?;
            r.read_exact(out).await.map_err(io_err)?;
            Ok(())
        }
        .boxed()
    }
}

impl SmbTransportWrite for Tcp {
    fn send_raw<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, TR<()>> {
        async move {
            let w = self.w.as_mut().ok_or(TransportError::NotConnected)?;
            w.write_all(buf).await.map_err(io_err)
        }
        .boxed()
    }
    /// The crate's default writes the 4-byte header and each body part separately (issue #184).
    fn send<'a>(&'a mut self, data: &'a IoVec) -> BoxFuture<'a, TR<()>> {
        async move {
            let len = data.total_size();
            let mut frame = Vec::with_capacity(4 + len);
            frame.extend_from_slice(&(len as u32).to_be_bytes());
            for b in data.iter() {
                frame.extend_from_slice(b);
            }
            self.send_raw(&frame).await
        }
        .boxed()
    }
}

impl SmbTransport for Tcp {
    fn connect<'a>(&'a mut self, _: &'a str, _: SocketAddr) -> BoxFuture<'a, TR<()>> {
        async { Ok(()) }.boxed() // we dial ourselves
    }
    fn default_port(&self) -> u16 {
        445
    }
    fn split(self: Box<Self>) -> TR<(Box<dyn SmbTransportRead>, Box<dyn SmbTransportWrite>)> {
        let peer = self.peer;
        Ok((
            Box::new(Tcp { r: self.r, w: None, peer }),
            Box::new(Tcp { r: None, w: self.w, peer }),
        ))
    }
    fn remote_address(&self) -> TR<SocketAddr> {
        Ok(self.peer)
    }
}

// ---------------------------------------------------------------- session helpers

#[derive(Clone)]
enum Via {
    Tcp(String),
    Socks { proxy: &'static str, auth: bool },
}

fn cfg() -> ConnectionConfig {
    ConnectionConfig {
        smb2_only_negotiate: true,
        allow_unsigned_guest_access: true,
        ..Default::default()
    }
}

struct Smb {
    conn: Connection,
    sess: Session,
    server: String,
}

async fn dial(via: &Via) -> R<(TcpStream, SocketAddr, String)> {
    match via {
        Via::Tcp(a) => {
            let s = TcpStream::connect(a).await.map_err(|e| format!("tcp: {e}"))?;
            let peer = s.peer_addr().unwrap();
            Ok((s, peer, peer.ip().to_string()))
        }
        Via::Socks { proxy, auth } => {
            let target = (PRIVATE, 445u16);
            let s = if *auth {
                tokio_socks::tcp::Socks5Stream::connect_with_password(
                    *proxy, target, "tester", "testpass",
                )
                .await
            } else {
                tokio_socks::tcp::Socks5Stream::connect(*proxy, target).await
            }
            .map_err(|e| format!("socks: {e}"))?;
            let s = s.into_inner();
            let peer = s.peer_addr().unwrap();
            Ok((s, peer, PRIVATE.to_string()))
        }
    }
}

async fn connect_as(via: &Via, user: &str, pass: &str, c: ConnectionConfig) -> R<Smb> {
    let (s, peer, server) = dial(via).await?;
    let conn = Connection::from_transport(Box::new(Tcp::new(s, peer)), &server, Guid::generate(), c)
        .await
        .map_err(|e| format!("negotiate: {e}"))?;
    let id = sspi::AuthIdentity {
        username: sspi::Username::parse(user).map_err(|e| format!("username: {e:?}"))?,
        password: sspi::Secret::from(pass.to_string()),
    };
    let sess = conn.authenticate(id).await.map_err(|e| format!("auth: {e}"))?;
    Ok(Smb { conn, sess, server })
}

async fn connect(via: &Via) -> R<Smb> {
    connect_as(via, "TESTDOM\\tester", "testpass", cfg()).await
}

fn tcp(a: impl ToString) -> Via {
    Via::Tcp(a.to_string())
}

impl Smb {
    async fn tree(&self, share: &str) -> R<Tree> {
        let unc: UncPath = format!(r"\\{}\{}", self.server, share).parse().map_err(|e| format!("{e}"))?;
        self.sess.tree_connect(&unc).await.map_err(|e| format!("tree_connect {share}: {e}"))
    }
    fn max_read(&self) -> u32 {
        self.conn.conn_info().unwrap().negotiation.max_read_size
    }
    fn max_write(&self) -> u32 {
        self.conn.conn_info().unwrap().negotiation.max_write_size
    }
    fn max_transact(&self) -> u32 {
        self.conn.conn_info().unwrap().negotiation.max_transact_size
    }
}

fn p(path: &str) -> String {
    path.replace('/', "\\")
}

fn dir_args() -> FileCreateArgs {
    FileCreateArgs {
        disposition: CreateDisposition::Open,
        attributes: FileAttributes::new(),
        options: CreateOptions::new().with_directory_file(true),
        desired_access: FileAccessMask::new()
            .with_file_read_data(true)
            .with_file_read_attributes(true)
            .with_synchronize(true),
    }
}

fn read_args() -> FileCreateArgs {
    FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true))
}

#[derive(Clone)]
struct Entry {
    name: Vec<u8>,
    is_dir: bool,

}

/// One listing: CREATE, QUERY_DIRECTORY until empty, CLOSE (explicit when `close`, else on drop).
async fn list(tree: &Tree, path: &str, buf: u32, close: bool) -> R<Vec<Entry>> {
    let res = tree.create(&p(path), &dir_args()).await.map_err(|e| format!("open {path}: {e}"))?;
    let dir = match res {
        Resource::Directory(d) => Arc::new(d),
        _ => return Err(format!("{path}: not a directory")),
    };
    let mut out = Vec::new();
    {
        let mut s = Directory::query_with_options::<FileDirectoryInformation>(&dir, "*", buf)
            .await
            .map_err(|e| format!("query {path}: {e}"))?;
        while let Some(e) = s.next().await {
            let e = e.map_err(|e| format!("query {path}: {e}"))?;
            let name = match String::try_from(e.file_name) {
                Ok(s) => s.into_bytes(),
                Err(_) => b"<invalid utf-16>".to_vec(),
            };
            if name == b"." || name == b".." {
                continue;
            }
            out.push(Entry { name, is_dir: e.file_attributes.directory() });
        }
    }
    if close {
        dir.close().await.map_err(|e| format!("close {path}: {e}"))?;
    }
    Ok(out)
}

async fn open_file(tree: &Tree, path: &str) -> R<Arc<File>> {
    match tree.create(&p(path), &read_args()).await.map_err(|e| format!("open {path}: {e}"))? {
        Resource::File(f) => Ok(Arc::new(f)),
        _ => Err(format!("{path}: not a file")),
    }
}

/// Read a whole file with `width` chunk requests in flight, hashing in order.
async fn read_all(tree: &Tree, path: &str, chunk: usize, width: usize) -> R<(u64, String)> {
    let f = open_file(tree, path).await?;
    let len = f.get_len().await.map_err(|e| e.to_string())?;
    let offs: Vec<u64> = (0..len).step_by(chunk).collect();
    let mut h = Sha256::new();
    let mut total = 0u64;
    let mut s = stream::iter(offs)
        .map(|off| {
            let f = f.clone();
            async move {
                let n = chunk.min((len - off) as usize);
                let mut buf = vec![0u8; n];
                let got = f.read_block(&mut buf, off, None, false).await.map_err(|e| format!("read @{off}: {e}"))?;
                if got != n {
                    return Err(format!("short read @{off}: {got} of {n}"));
                }
                Ok::<_, String>(buf)
            }
        })
        .buffered(width);
    while let Some(b) = s.next().await {
        let b = b?;
        total += b.len() as u64;
        h.update(&b);
    }
    drop(s);
    f.close().await.map_err(|e| e.to_string())?;
    Ok((total, hex(&h.finalize())))
}

fn gen_chunk(i: u64, n: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (i.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
    let mut v = Vec::with_capacity(n + 8);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(n);
    v
}

/// Create (overwrite) and write `size` generated bytes with `width` writes in flight. Returns local sha256.
async fn write_all(tree: &Tree, path: &str, size: u64, chunk: usize, width: usize) -> R<String> {
    let res = tree
        .create(&p(path), &FileCreateArgs::make_overwrite(FileAttributes::new(), CreateOptions::new()))
        .await
        .map_err(|e| format!("create {path}: {e}"))?;
    let f = match res {
        Resource::File(f) => Arc::new(f),
        _ => return Err("not a file".into()),
    };
    let n = size.div_ceil(chunk as u64);
    let mut h = Sha256::new();
    let mut s = stream::iter(0..n)
        .map(|i| {
            let off = i * chunk as u64;
            let data = gen_chunk(i, chunk.min((size - off) as usize));
            h.update(&data);
            let f = f.clone();
            async move {
                let len = data.len();
                let w = f.write_block(&data, off, None).await.map_err(|e| format!("write @{off}: {e}"))?;
                if w != len {
                    return Err(format!("short write @{off}: {w} of {len}"));
                }
                Ok::<_, String>(())
            }
        })
        .buffer_unordered(width);
    let mut err = None;
    while let Some(r) = s.next().await {
        if let Err(e) = r {
            err = Some(e);
            break;
        }
    }
    drop(s);
    let _ = f.close().await;
    match err {
        Some(e) => Err(e),
        None => Ok(hex(&h.finalize())),
    }
}

async fn open_any(tree: &Tree, path: &str, access: FileAccessMask) -> R<Resource> {
    tree.create(&p(path), &FileCreateArgs::make_open_existing(access)).await.map_err(|e| format!("{e}"))
}

fn handle(r: &Resource) -> &smb::ResourceHandle {
    match r {
        Resource::File(f) => f,
        Resource::Directory(d) => d,
        Resource::Pipe(x) => x,
    }
}

async fn delete(tree: &Tree, path: &str) -> R<()> {
    let r = open_any(tree, path, FileAccessMask::new().with_delete(true).with_file_read_attributes(true)).await?;
    let h = handle(&r);
    h.set_info(FileDispositionInformation::default()).await.map_err(|e| format!("delete {path}: {e}"))?;
    h.close().await.map_err(|e| e.to_string())
}

async fn rename(tree: &Tree, from: &str, to: &str, replace: bool) -> R<()> {
    let r = open_any(tree, from, FileAccessMask::new().with_delete(true).with_file_read_attributes(true)).await?;
    let h = handle(&r);
    let res = h
        .set_info(FileRenameInformation {
            replace_if_exists: replace.into(),
            root_directory: 0,
            file_name: p(to).as_str().into(),
        })
        .await
        .map_err(|e| format!("{e}"));
    let _ = h.close().await;
    res
}

async fn mkdir(tree: &Tree, path: &str) -> R<()> {
    let r = tree
        .create_directory(&p(path), CreateDisposition::OpenIf, FileAccessMask::new().with_generic_all(true))
        .await
        .map_err(|e| format!("mkdir {path}: {e}"))?;
    handle(&r).close().await.map_err(|e| e.to_string())
}

/// Breadth-first walk with at most `width` listings in flight. Returns (dirs listed, files, denied).
async fn walk(tree: &Tree, root: &str, width: usize) -> (usize, usize, usize) {
    let mut queue = vec![root.to_string()];
    let mut running = FuturesUnordered::new();
    let (mut dirs, mut files, mut denied) = (0, 0, 0);
    loop {
        while running.len() < width
            && let Some(d) = queue.pop()
        {
            running.push(async move {
                let r = list(tree, &d, 65536, false).await;
                (d, r)
            });
        }
        let Some((d, r)) = running.next().await else { break };
        match r {
            Ok(es) => {
                dirs += 1;
                for e in es {
                    if e.is_dir {
                        queue.insert(0, format!("{d}/{}", String::from_utf8_lossy(&e.name)));
                    } else {
                        files += 1;
                    }
                }
            }
            Err(_) => denied += 1,
        }
    }
    (dirs, files, denied)
}

// ---------------------------------------------------------------- measurement helpers

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn row(step: &str, item: &str, ep: &str, result: &str, detail: impl AsRef<str>) {
    println!("ROW\t{step}\t{item}\t{ep}\t{result}\t{}", detail.as_ref().replace(['\t', '\n'], " "));
}

async fn within<T>(secs: u64, f: impl std::future::Future<Output = R<T>>) -> R<T> {
    match tokio::time::timeout(Duration::from_secs(secs), f).await {
        Ok(r) => r,
        Err(_) => Err(format!("TIMEOUT after {secs}s")),
    }
}

fn verdict<T>(r: &R<T>) -> &'static str {
    match r {
        Ok(_) => "OK",
        Err(e) if e.starts_with("TIMEOUT") => "TIMEOUT",
        Err(_) => "ERROR",
    }
}

/// Median TCP connect time over 3 connects, as check.sh measures it.
async fn rtt(addr: &str) -> f64 {
    let mut v = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        if let Ok(Ok(_s)) = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr)).await {
            v.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

async fn ep(addr: &str) -> String {
    let name = match addr {
        FAST => "fast",
        SLOW => "slow",
        BAD => "wan-bad",
        EDGE => "edge",
        BLACKHOLE => "slow:4450",
        _ => addr,
    };
    format!("{name} rtt={:.2}ms", rtt(addr).await)
}

fn ms(d: Duration) -> String {
    format!("{:.1}ms", d.as_secs_f64() * 1000.0)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn start_tap(upstream: &str) -> Tap {
    Tap::start(upstream).expect("tap")
}

fn settle(tap: &Tap) -> bool {
    tokio::task::block_in_place(|| tap.settle(Duration::from_millis(300), Duration::from_secs(60)))
}

fn counts(s: &smbtap::Stats) -> String {
    let cmds: Vec<String> = s.per_command.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    format!(
        "req={} flights={} interim={} peak_out={} conns={} [{}]",
        s.requests,
        s.flights,
        s.interim,
        s.peak_outstanding,
        s.connections,
        cmds.join(" ")
    )
}

fn hwm_reset() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn hwm_mib() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

fn sh(cmd: &str) -> String {
    let o = Command::new("bash").arg("-c").arg(cmd).output().expect("bash");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn ctl(args: &str) -> String {
    let repo = std::env::var("REPO").unwrap_or_else(|_| "../..".into());
    sh(&format!("{repo}/testenv/ctl.sh {args} 2>&1"))
}

fn server_sha(container: &str, path: &str) -> String {
    sh(&format!("docker exec {container} sha256sum '{path}' | cut -d' ' -f1"))
}

/// Names in a server directory as raw bytes, NUL-separated (no `ls` quoting).
fn server_names(dir: &str) -> BTreeSet<Vec<u8>> {
    let o = Command::new("docker")
        .args(["exec", "rsmb-samba", "sh", "-c"])
        .arg(format!(
            "cd '/srv/share/{dir}' && for f in * .[!.]* ..?*; do if [ -e \"$f\" ] || [ -L \"$f\" ]; then printf '%s\\0' \"$f\"; fi; done"
        ))
        .output()
        .expect("docker");
    o.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| s.to_vec()).collect()
}

// ---------------------------------------------------------------- steps

async fn step1() {
    // user/password/domain, fast and slow, counted and timed
    for addr in [FAST, SLOW] {
        let tap = start_tap(addr);
        let r = within(30, async {
            let s = connect(&tcp(tap.addr())).await?;
            let t = s.tree("share").await?;
            list(&t, "tree", 65536, true).await?;
            Ok(())
        })
        .await;
        settle(&tap);
        let e = ep(addr).await;
        row("1", "login user/pass/domain + tree_connect + ls", &e, verdict(&r), format!("{} {}", counts(&tap.stats()), r.err().unwrap_or_default()));
        let mut ts = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            if within(30, connect(&tcp(addr))).await.is_ok() {
                ts.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        row("1", "login time (connect+negotiate+session setup), median of 5", &e, if ts.len() == 5 { "OK" } else { "ERROR" }, format!("{:.1}ms", median(ts)));
    }
    let wrong = within(30, connect_as(&tcp(FAST), "TESTDOM\\tester", "wrongpass", cfg())).await;
    row("1", "wrong password refused", "fast", if wrong.is_err() { "OK" } else { "FAIL" }, wrong.err().unwrap_or_default());

    // guest: unknown user -> Samba maps to guest
    for (label, allow) in [("guest (allow_unsigned_guest_access=true)", true), ("guest (crate default config)", false)] {
        let c = ConnectionConfig { allow_unsigned_guest_access: allow, ..cfg() };
        let r = within(30, async {
            let s = connect_as(&tcp(FAST), "nobody", "x", c).await?;
            let t = s.tree("guest").await?;
            let (n, _) = read_all(&t, "hello.txt", 65536, 1).await?;
            let denied = s.tree("share").await.is_err();
            Ok(format!("read guest/hello.txt {n} bytes; tree_connect share refused={denied}"))
        })
        .await;
        row("1", label, "fast", verdict(&r), r.unwrap_or_else(|e| e));
    }
    // anonymous: empty user name and password
    let r = within(30, async {
        let s = connect_as(&tcp(FAST), "", "", cfg()).await?;
        let t = s.tree("guest").await?;
        let (n, _) = read_all(&t, "hello.txt", 65536, 1).await?;
        Ok(format!("read guest/hello.txt {n} bytes"))
    })
    .await;
    row("1", "anonymous (empty user/password)", "fast", verdict(&r), r.unwrap_or_else(|e| e));

    // encrypted share
    let tap = start_tap(FAST);
    let r = within(30, async {
        let s = connect(&tcp(tap.addr())).await?;
        let t = s.tree("encrypted").await?;
        let es = list(&t, "tree", 65536, true).await?;
        let (n, h) = read_all(&t, "large/medium.bin", 1 << 20, 8).await?;
        Ok(format!("ls tree={} entries; medium.bin {n} bytes sha_ok={}", es.len(), h == server_sha("rsmb-samba", "/srv/share/large/medium.bin")))
    })
    .await;
    settle(&tap);
    let st = tap.stats();
    let enc = st.per_command.get("ENCRYPTED").copied().unwrap_or(0);
    let ok = r.is_ok() && enc > 0;
    row("1", "encrypted share (smb encrypt = required)", "fast", if ok { "OK" } else { verdict(&r) }, format!("{} encrypted_frames={enc} {}", r.unwrap_or_else(|e| e), counts(&st)));

    // SOCKS5, hostname resolved by the proxy
    for (label, proxy, auth) in [("SOCKS5 no-auth :1080", SOCKS, false), ("SOCKS5 user/pass :1081", SOCKS_AUTH, true)] {
        let log_before = sh("docker logs rsmb-socks 2>&1 | grep -c 'connected to fileserver.rsmb.test:445'");
        let r = within(30, async {
            let s = connect(&Via::Socks { proxy, auth }).await?;
            let t = s.tree("share").await?;
            let es = list(&t, "tree", 65536, true).await?;
            let (n, h) = read_all(&t, "large/medium.bin", 1 << 20, 8).await?;
            let ok = h == server_sha("rsmb-samba-private", "/srv/share/large/medium.bin");
            Ok(format!("ls tree={} entries; medium.bin {n} bytes sha_ok={ok}", es.len()))
        })
        .await;
        let log_after = sh("docker logs rsmb-socks 2>&1 | grep -c 'connected to fileserver.rsmb.test:445'");
        let host_resolves = sh("getent hosts fileserver.rsmb.test || echo none");
        row(
            "1",
            label,
            "socks->fileserver.rsmb.test",
            verdict(&r),
            format!("{}; proxy log lines {log_before}->{log_after}; host resolves name: {host_resolves}", r.clone().unwrap_or_else(|e| e)),
        );
    }
}

async fn shares(addr: &str) -> R<Vec<(String, u32, String)>> {
    let s = connect(&tcp(addr)).await?;
    let ipc = s.tree("IPC$").await?;
    let pipe = match ipc.create("srvsvc", &FileCreateArgs::make_pipe()).await.map_err(|e| e.to_string())? {
        Resource::Pipe(p) => p,
        _ => return Err("srvsvc: not a pipe".into()),
    };
    let mut srv: smb_rpc::interface::SrvSvc<_> = pipe.bind().await.map_err(|e| format!("bind: {e}"))?;
    let list = srv.netr_share_enum(&s.server).await.map_err(|e| format!("NetrShareEnum: {e}"))?;
    Ok(list
        .iter()
        .map(|i| {
            let name = i.netname.as_ref().map(|n| n.to_string()).unwrap_or_default();
            let kind = format!("{:?}", i.share_type.kind());
            let remark = i.remark.as_ref().map(|n| n.to_string()).unwrap_or_default();
            (name, 0, format!("{kind}|{remark}"))
        })
        .collect())
}

// ---- workaround: our own NDR32 srvsvc NetrShareEnum over FSCTL_PIPE_TRANSCEIVE (crate's RPC is NDR64-only)

fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Vec<u8> {
    let mut v = d1.to_le_bytes().to_vec();
    v.extend(d2.to_le_bytes());
    v.extend(d3.to_le_bytes());
    v.extend(d4);
    v
}

fn pdu(ptype: u8, call_id: u32, body: &[u8]) -> Vec<u8> {
    let mut v = vec![5, 0, ptype, 3, 0x10, 0, 0, 0];
    v.extend(((16 + body.len()) as u16).to_le_bytes());
    v.extend([0, 0]);
    v.extend(call_id.to_le_bytes());
    v.extend(body);
    v
}

fn ndr_wstr(v: &mut Vec<u8>, s: &str) {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    for x in [w.len() as u32, 0, w.len() as u32] {
        v.extend(x.to_le_bytes());
    }
    w.iter().for_each(|c| v.extend(c.to_le_bytes()));
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// Response stubs of all fragments, concatenated.
fn rpc_stub(out: &[u8]) -> R<Vec<u8>> {
    let (mut i, mut stub) = (0, vec![]);
    while i + 24 <= out.len() {
        let flen = u16::from_le_bytes([out[i + 8], out[i + 9]]) as usize;
        if out[i + 2] != 2 {
            return Err(format!("rpc ptype {} (fault?)", out[i + 2]));
        }
        stub.extend_from_slice(&out[i + 24..i + flen]);
        let last = out[i + 3] & 2 != 0;
        i += flen;
        if last {
            return Ok(stub);
        }
    }
    Err(format!("rpc response incomplete: {} bytes, no last fragment", out.len()))
}

/// The rest of an RPC reply longer than one fragment can only be read with READ on the pipe,
/// which the crate does not expose (patches/2-pipe-read.patch adds `Pipe::read`).
#[cfg(feature = "patched")]
async fn rpc_reply(pipe: &smb::Pipe, mut out: Vec<u8>) -> R<Vec<u8>> {
    loop {
        match rpc_stub(&out) {
            Err(e) if e.starts_with("rpc response incomplete") => {
                let more = pipe.read(65536).await.map_err(|e| format!("pipe read: {e}"))?;
                if more.is_empty() {
                    return Err(e);
                }
                out.extend(more);
            }
            r => return r,
        }
    }
}

#[cfg(not(feature = "patched"))]
async fn rpc_reply(_: &smb::Pipe, out: Vec<u8>) -> R<Vec<u8>> {
    rpc_stub(&out)
}

async fn transceive(pipe: &smb::Pipe, data: Vec<u8>, max: u32) -> smb::Result<Vec<u8>> {
    let r = pipe.fsctl_with_options(smb::PipeTransceiveRequest::from(smb::IoctlBuffer::from(data)), max).await?;
    Ok(r.to_vec())
}

async fn shares_ndr32(addr: &str, uncapped: bool) -> R<Vec<(String, u32, String)>> {
    let s = connect(&tcp(addr)).await?;
    let ipc = s.tree("IPC$").await?;
    let pipe = match ipc.create("srvsvc", &FileCreateArgs::make_pipe()).await.map_err(|e| e.to_string())? {
        Resource::Pipe(p) => p,
        _ => return Err("srvsvc: not a pipe".into()),
    };
    let mut bind = vec![0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0]; // frags 65535, ctx 0
    bind[0..2].copy_from_slice(&0xfff0u16.to_le_bytes());
    bind[2..4].copy_from_slice(&0xfff0u16.to_le_bytes());
    bind.extend(guid(0x4b32_4fc8, 0x1670, 0x01d3, [0x12, 0x78, 0x5a, 0x47, 0xbf, 0x6e, 0xe1, 0x88]));
    bind.extend([3, 0, 0, 0]);
    bind.extend(guid(0x8a88_5d04, 0x1ceb, 0x11c9, [0x9f, 0xe8, 0x08, 0x00, 0x2b, 0x10, 0x48, 0x60]));
    bind.extend(2u32.to_le_bytes());
    let ack = transceive(&pipe, pdu(11, 1, &bind), 65536).await.map_err(|e| format!("bind: {e}"))?;
    if ack.get(2) != Some(&12) {
        return Err(format!("bind not acked: ptype {:?}", ack.get(2)));
    }
    // Samba sends RPC fragments of at most 4280 bytes, and the rest of a longer answer can only be
    // fetched with READ on the pipe, which the crate does not expose for pipes. So page the
    // enumeration instead: small PreferedMaximumLength + ResumeHandle until ERROR_MORE_DATA stops.
    let (mut res, mut resume, mut call) = (vec![], 0u32, 2u32);
    loop {
        let mut stub = vec![0, 0, 2, 0]; // ServerName: unique pointer
        ndr_wstr(&mut stub, &format!("\\\\{}", s.server));
        let prefmax = if uncapped { 0xffff_ffff } else { 2048 };
        for x in [1u32, 1, 0x0002_0004, 0, 0, prefmax, 0x0002_0008, resume] {
            stub.extend(x.to_le_bytes()); // level, switch, ptr container, entries, null buffer, max len, resume ptr, resume
        }
        let mut req = (stub.len() as u32).to_le_bytes().to_vec();
        req.extend([0, 0, 15, 0]); // context 0, opnum 15 NetrShareEnum
        req.extend(&stub);
        let max = if uncapped { 1 << 20 } else { s.max_transact() };
        let out = transceive(&pipe, pdu(0, call, &req), max).await.map_err(|e| format!("NetrShareEnum: {e}"))?;
        call += 1;
        let b = rpc_reply(&pipe, out).await?;
        let u32at = |i: usize| -> R<u32> { b.get(i..i + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).ok_or("short stub".to_string()) };
        let n = u32at(12)? as usize; // level, switch, ptr, entries read
        let mut i = if n == 0 { 20 } else { 24 }; // ..., buffer ptr (null when empty), max count
        let mut fixed = vec![];
        for _ in 0..n {
            fixed.push((u32at(i)?, u32at(i + 4)?, u32at(i + 8)?));
            i += 12;
        }
        let wstr = |i: &mut usize, present: bool| -> R<String> {
            if !present {
                return Ok(String::new());
            }
            let actual = u32at(*i + 8)? as usize;
            let w: Vec<u16> = b.get(*i + 12..*i + 12 + 2 * actual).ok_or("short string")?.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            *i += 12 + 2 * actual;
            *i = i.div_ceil(4) * 4;
            Ok(String::from_utf16_lossy(&w).trim_end_matches('\0').to_string())
        };
        for (pn, ty, pr) in fixed {
            let name = wstr(&mut i, pn != 0)?;
            let remark = wstr(&mut i, pr != 0)?;
            res.push((name, ty, format!("type=0x{ty:x}|{remark}")));
        }
        // TotalEntries, ResumeHandle (ptr, value), WERROR
        let (rptr, rval, werr) = (u32at(i + 4)?, u32at(i + 8)?, u32at(i + 12)?);
        match werr {
            0 => break,
            234 if rptr != 0 && n > 0 => resume = rval,
            x => return Err(format!("NetrShareEnum WERROR {x} after {} shares", res.len())),
        }
    }
    let _ = pipe.close().await;
    Ok(res)
}

async fn step2() {
    for (addr, expect_edge, own, uncapped) in [(FAST, false, false, false), (EDGE, true, false, false), (FAST, false, true, false), (EDGE, true, true, false), (EDGE, true, true, true)] {
        let tap = start_tap(addr);
        let a = tap.addr().to_string();
        let r = if own { within(30, shares_ndr32(&a, uncapped)).await } else { within(30, shares(&a)).await };
        let how = match (own, uncapped) {
            (false, _) => "crate SrvSvc (NDR64)",
            (true, false) => "workaround: own NDR32 NetrShareEnum via FSCTL_PIPE_TRANSCEIVE, max output = max_transact",
            (true, true) => "same, max output 1 MiB (credit charge 17 > 16 credits the server grants)",
        };
        settle(&tap);
        let c = counts(&tap.stats());
        if std::env::var("DEBUG").is_ok() { let st = tap.stats(); for q in &st.request_log { eprintln!("REQ {:?}", q); } for q in &st.response_log { eprintln!("RSP {:?}", q); } }
        let e = ep(addr).await;
        match r {
            Ok(list) => {
                let names: BTreeSet<&str> = list.iter().map(|x| x.0.as_str()).collect();
                let mut missing = vec![];
                for n in ["share", "readonly", "guest", "encrypted", "tiny", "IPC$"] {
                    if !names.contains(n) {
                        missing.push(n.to_string());
                    }
                }
                if expect_edge {
                    for i in 1..=300 {
                        let n = if i % 10 == 0 { format!("edge{i:03}$") } else { format!("edge{i:03}") };
                        if !names.contains(n.as_str()) {
                            missing.push(n);
                        }
                    }
                }
                let hidden = list.iter().filter(|x| x.0.ends_with('$')).count();
                let sample: Vec<String> = list.iter().take(3).map(|x| format!("{}={}", x.0, x.2)).collect();
                let ok = missing.is_empty();
                row("2", &format!("share list, {how}"), &e, if ok { "OK" } else { "FAIL" }, format!("{} shares, {hidden} hidden, missing={missing:?}; sample {sample:?}; {c}", list.len()));
            }
            Err(err) => row("2", &format!("share list, {how}"), &e, if err.starts_with("TIMEOUT") { "TIMEOUT" } else { "ERROR" }, format!("{err} {c}")),
        }
    }
}

fn status_of(r: &R<Resource>) -> String {
    match r {
        Ok(Resource::Directory(_)) => "dir".into(),
        Ok(Resource::File(_)) => "file".into(),
        Ok(Resource::Pipe(_)) => "pipe".into(),
        Err(e) => e.clone(),
    }
}

/// Error as a status name when the crate gives a typed status, plus the variant used.
fn es(e: &smb::Error) -> String {
    let (code, variant) = match e {
        smb::Error::UnexpectedMessageStatus(c) => (*c, "UnexpectedMessageStatus"),
        smb::Error::ReceivedErrorMessage(c, _) => (*c, "ReceivedErrorMessage"),
        other => return format!("untyped: {other}"),
    };
    let name = match code {
        0xC000_0034 => "NOT_FOUND",
        0xC000_003A => "PATH_NOT_FOUND",
        0xC000_0022 => "ACCESS_DENIED",
        0xC000_0103 => "NOT_A_DIRECTORY",
        0xC000_00A2 => "MEDIA_WRITE_PROTECTED",
        0xC000_007F => "DISK_FULL",
        0xC000_0035 => "OBJECT_NAME_COLLISION",
        _ => "",
    };
    format!("{name}(0x{code:08x} via Error::{variant})")
}

async fn step3() {
    let cases = [
        ("tree", "existing dir", &["dir"][..]),
        ("no-such-dir", "missing", &["NOT_FOUND"][..]),
        ("large/medium.bin", "file", &["NOT_A_DIRECTORY", "file"][..]),
        ("forbidden", "forbidden/ (0700 root)", &["dir", "ACCESS_DENIED"][..]), // denied or enterable: both right, "missing" is wrong
        ("traverse-only", "traverse-only/ (0711)", &["dir"][..]),
    ];
    let masks: [(&str, FileAccessMask, bool); 4] = [
        ("READ_ATTRIBUTES|SYNCHRONIZE +DIRECTORY_FILE", FileAccessMask::new().with_file_read_attributes(true).with_synchronize(true), true),
        ("READ_ATTRIBUTES only +DIRECTORY_FILE", FileAccessMask::new().with_file_read_attributes(true), true),
        ("LIST_DIRECTORY|READ_ATTRIBUTES|SYNCHRONIZE +DIRECTORY_FILE", FileAccessMask::new().with_file_read_data(true).with_file_read_attributes(true).with_synchronize(true), true),
        ("READ_ATTRIBUTES|SYNCHRONIZE, no DIRECTORY_FILE", FileAccessMask::new().with_file_read_attributes(true).with_synchronize(true), false),
    ];
    for addr in [FAST, SLOW] {
        let e = ep(addr).await;
        let tap = start_tap(addr);
        let s = match within(30, connect(&tcp(tap.addr()))).await {
            Ok(s) => s,
            Err(x) => return row("3", "connect", &e, "ERROR", x),
        };
        let t = s.tree("share").await.unwrap();
        for (mname, mask, dirflag) in &masks {
            let args = FileCreateArgs {
                disposition: CreateDisposition::Open,
                attributes: FileAttributes::new(),
                options: CreateOptions::new().with_directory_file(*dirflag),
                desired_access: *mask,
            };
            let mut all_right = true;
            let mut detail = vec![];
            let mut max_fl_ret = 0;
            let mut max_req = 0;
            let mut forbidden = String::new();
            for (path, label, right) in &cases {
                settle(&tap);
                tap.reset();
                let r = within(30, async { t.create(&p(path), &args).await.map_err(|e| es(&e)) }).await;
                let at_return = tap.stats().flights;
                let st = status_of(&r);
                let st = st.split('(').next().unwrap_or("").to_string();
                // CLOSE on drop, in the background, as rsmb's cd would
                drop(r);
                settle(&tap);
                let s2 = tap.stats();
                max_fl_ret = max_fl_ret.max(at_return);
                max_req = max_req.max(s2.requests);
                let ok = right.contains(&st.as_str());
                all_right &= ok;
                if *path == "forbidden" {
                    forbidden = (if st == "dir" { "enterable" } else { "denied" }).to_string();
                }
                detail.push(format!("{label}={st}{}", if ok { "" } else { "(WRONG)" }));
            }
            row(
                "3",
                &format!("dir check: {mname}"),
                &e,
                &if all_right && max_fl_ret <= 1 && max_req <= 2 { format!("PASS (forbidden: {forbidden})") } else { "FAIL".to_string() },
                format!("{}; worst flights-at-return={max_fl_ret} worst requests-settled={max_req}", detail.join(", ")),
            );
        }
        row("3", "compound CREATE+CLOSE", &e, "N/A", "crate has no compound-request API (OutgoingMessage carries one command); plain CREATE then CLOSE on drop instead");
        drop(t);
        drop(s);
        // timing of the check (time until create returns), direct, median of 5
        let s = connect(&tcp(addr)).await.unwrap();
        let t = s.tree("share").await.unwrap();
        let args = FileCreateArgs {
            disposition: CreateDisposition::Open,
            attributes: FileAttributes::new(),
            options: CreateOptions::new().with_directory_file(true),
            desired_access: FileAccessMask::new().with_file_read_attributes(true).with_synchronize(true),
        };
        let mut v = vec![];
        for _ in 0..5 {
            let t0 = Instant::now();
            let r = t.create("tree\\d1\\d2", &args).await;
            v.push(t0.elapsed().as_secs_f64() * 1000.0);
            drop(r);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        row("3", "dir check wall time (CREATE returns), median of 5", &ep(addr).await, "OK", format!("{:.1}ms", median(v)));
    }
}

async fn step4() {
    let dirs = ["tree", "many", "huge", "longnames"];
    // set comparison with the server's own listing
    let s = connect(&tcp(FAST)).await.unwrap();
    let max = s.max_transact();
    let t = s.tree("share").await.unwrap();
    for d in dirs {
        let r = within(120, list(&t, d, max, true)).await;
        let want = server_names(d);
        match r {
            Ok(es) => {
                let got: BTreeSet<Vec<u8>> = es.iter().map(|e| e.name.clone()).collect();
                let ok = got == want;
                row("4", &format!("ls {d}/ = server listing (set of byte strings)"), "fast", if ok { "OK" } else { "FAIL" }, format!("client {} server {} only-client {} only-server {}", got.len(), want.len(), got.difference(&want).count(), want.difference(&got).count()));
            }
            Err(e) => row("4", &format!("ls {d}/ = server listing"), "fast", verdict(&Err::<(), _>(e.clone())), e),
        }
    }
    drop(t);
    drop(s);
    // first ls after connect with the largest buffer (multi-credit query on fresh credits)
    let tap = start_tap(FAST);
    let r = within(60, async {
        let s = connect(&tcp(tap.addr())).await?;
        let t = s.tree("share").await?;
        let es = list(&t, "huge", s.max_transact(), true).await?;
        Ok(es.len())
    })
    .await;
    settle(&tap);
    let st = tap.stats();
    let qd: Vec<u16> = st.request_log.iter().filter(|q| q.command == "QUERY_DIRECTORY").map(|q| q.credit_charge).take(3).collect();
    row("4", &format!("first ls after connect: huge/ with {max}-byte buffer"), &ep(FAST).await, verdict(&r), format!("{} entries; QUERY_DIRECTORY credit charges {qd:?}; {}", r.as_ref().map(|n| n.to_string()).unwrap_or_else(|e| e.clone()), counts(&st)));

    for addr in [FAST, SLOW] {
        let e = ep(addr).await;
        let tap = start_tap(addr);
        let sc = connect(&tcp(tap.addr())).await.unwrap();
        let tc = sc.tree("share").await.unwrap();
        let sd = connect(&tcp(addr)).await.unwrap();
        let td = sd.tree("share").await.unwrap();
        for d in dirs {
            for buf in [65536u32, max] {
                settle(&tap);
                tap.reset();
                let r = within(180, list(&tc, d, buf, true)).await;
                settle(&tap);
                let c = counts(&tap.stats());
                if let Err(x) = r {
                    row("4", &format!("ls {d}/ buf={buf}"), &e, verdict(&Err::<(), _>(x.clone())), format!("{x} {c}"));
                    continue;
                }
                let runs = if addr == SLOW && d == "huge" && buf == 65536 { 1 } else { 3 };
                let mut ts = vec![];
                let mut rss = 0f64;
                let mut n = 0;
                for _ in 0..runs {
                    hwm_reset();
                    let base = hwm_mib();
                    let t0 = Instant::now();
                    match within(180, list(&td, d, buf, true)).await {
                        Ok(es) => {
                            ts.push(t0.elapsed().as_secs_f64() * 1000.0);
                            n = es.len();
                        }
                        Err(x) => {
                            row("4", &format!("ls {d}/ buf={buf} (timed)"), &e, "ERROR", x);
                            break;
                        }
                    }
                    rss = rss.max(hwm_mib() - base);
                }
                if ts.is_empty() {
                    continue;
                }
                row("4", &format!("ls {d}/ buf={buf}"), &e, "OK", format!("{n} entries, {:.1}ms (median of {runs}), peak RSS +{rss:.1}MiB; {c}", median(ts)));
            }
        }
    }
}

async fn step5() {
    let s = connect(&tcp(FAST)).await.unwrap();
    let chunk = (s.max_read() as usize).min(1 << 20);
    let width = 16;
    let t = s.tree("share").await.unwrap();
    // sizes/
    let sums = sh("docker exec rsmb-samba sh -c 'cd /srv/share/sizes && sha256sum *.bin'");
    let mut bad = vec![];
    let mut n = 0;
    for line in sums.lines() {
        let mut it = line.split_whitespace();
        let (want, name) = (it.next().unwrap(), it.next().unwrap());
        n += 1;
        match within(60, read_all(&t, &format!("sizes/{name}"), chunk, width)).await {
            Ok((_, h)) if h == want => {}
            Ok((len, _)) => bad.push(format!("{name}: sha mismatch ({len} bytes)")),
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }
    row("5", "read every file in sizes/, sha256 vs server", &ep(FAST).await, if bad.is_empty() && n == 11 { "OK" } else { "FAIL" }, format!("{n} files, chunk={chunk} width={width}; failures {bad:?}"));

    // big.bin, five runs, evicted first, direct (no tap)
    let want = server_sha("rsmb-samba", "/srv/share/large/big.bin");
    let e = ep(FAST).await;
    let mut ts = vec![];
    for i in 1..=5 {
        ctl("evict samba /srv/share/large/big.bin");
        hwm_reset();
        let base = hwm_mib();
        let t0 = Instant::now();
        let r = within(120, read_all(&t, "large/big.bin", chunk, width)).await;
        let dt = t0.elapsed().as_secs_f64();
        match r {
            Ok((len, h)) => {
                ts.push(dt);
                row("5", &format!("download big.bin run {i}"), &e, if h == want && len == BIG_LEN { "OK" } else { "FAIL" }, format!("{:.2}s {:.0}MB/s sha_ok={} peak RSS +{:.1}MiB chunk={chunk} width={width}", dt, len as f64 / dt / 1e6, h == want, hwm_mib() - base));
            }
            Err(x) => {
                row("5", &format!("download big.bin run {i}"), &e, verdict(&Err::<(), _>(x.clone())), x);
                return row("5", "rest of step 5", "-", "SKIPPED", "after a failed or hung 1.1 GiB download (see issue #185 / patches/3-interim-credits.patch)");
            }
        }
    }
    if ts.len() == 5 {
        let m = median(ts.clone());
        let worst = ts.iter().cloned().fold(0f64, f64::max);
        row("5", "download big.bin x5: worst vs median", &e, if worst <= 3.0 * m { "OK" } else { "FAIL" }, format!("median {m:.2}s worst {worst:.2}s ratio {:.2}", worst / m));
    }
    drop(t);
    drop(s);

    // one counted run through the tap, evicted: interim responses (issue #185)
    let tap = start_tap(FAST);
    ctl("evict samba /srv/share/large/big.bin");
    let r = within(300, async {
        let s = connect(&tcp(tap.addr())).await?;
        let t = s.tree("share").await?;
        read_all(&t, "large/big.bin", chunk, width).await
    })
    .await;
    settle(&tap);
    let st = tap.stats();
    row("5", "download big.bin through tap (evicted): interims", &e, verdict(&r), format!("sha_ok={} {}{}", r.as_ref().map(|x| x.1 == want).unwrap_or(false), counts(&st), if st.interim == 0 { " (0 interims: #185 stall test inconclusive)" } else { "" }));

    // slow endpoint, one run, against the raw TCP ceiling
    let ceiling = sh("iperf3 -c 10.213.45.30 -p 5201 -t 12 -R -J 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(round(d[\"end\"][\"sum_received\"][\"bits_per_second\"]/8e6,1))' 2>/dev/null || echo n/a");
    let e = ep(SLOW).await;
    ctl("evict samba /srv/share/large/big.bin");
    let r = within(600, async {
        let s = connect(&tcp(SLOW)).await?;
        let t = s.tree("share").await?;
        let t0 = Instant::now();
        let x = read_all(&t, "large/big.bin", chunk, width).await?;
        Ok((x, t0.elapsed().as_secs_f64()))
    })
    .await;
    match r {
        Ok(((len, h), dt)) => row("5", "download big.bin at slow endpoint", &e, if h == want { "OK" } else { "FAIL" }, format!("{dt:.1}s {:.1}MB/s; iperf3 -R ceiling {ceiling}MB/s (12 s incl. slow start); chunk={chunk} width={width}", len as f64 / dt / 1e6)),
        Err(x) => row("5", "download big.bin at slow endpoint", &e, verdict(&Err::<(), _>(x.clone())), x),
    }

    // a block above 4 GiB
    let off: u64 = (4u64 << 30) + 12345;
    let want = sh(&format!("docker exec rsmb-samba sh -c 'dd if=/srv/share/large/over4g.bin bs=1048576 count=1 skip={off} iflag=skip_bytes 2>/dev/null | sha256sum' | cut -d' ' -f1"));
    let r = within(60, async {
        let s = connect(&tcp(FAST)).await?;
        let t = s.tree("share").await?;
        let f = open_file(&t, "large/over4g.bin").await?;
        let mut buf = vec![0u8; 1 << 20];
        let n = f.read_block(&mut buf, off, None, false).await.map_err(|e| e.to_string())?;
        let _ = f.close().await;
        Ok(hex(&Sha256::digest(&buf[..n])))
    })
    .await;
    row("5", "1 MiB block of over4g.bin at offset 4 GiB+12345", &ep(FAST).await, if r.as_ref().map(|h| *h == want).unwrap_or(false) { "OK" } else { "FAIL" }, format!("{:?} server {want}", r));
}

async fn step6() {
    let s = connect(&tcp(FAST)).await.unwrap();
    let chunk = (s.max_write() as usize).min(1 << 20);
    let width = 16;
    let t = s.tree("share").await.unwrap();
    let r = mkdir(&t, UP).await;
    row("6", "mkdir upload/spike-smbrs", "fast", verdict(&r), r.err().unwrap_or_default());
    let e = ep(FAST).await;
    let path = format!("{UP}\\big-upload.bin");
    let mut ts = vec![];
    for i in 1..=3 {
        hwm_reset();
        let base = hwm_mib();
        let t0 = Instant::now();
        let r = within(300, write_all(&t, &path, BIG_LEN, chunk, width)).await;
        let dt = t0.elapsed().as_secs_f64();
        match r {
            Ok(local) => {
                let remote = server_sha("rsmb-samba", "/srv/share/upload/spike-smbrs/big-upload.bin");
                ts.push(dt);
                row("6", &format!("upload 1.1 GiB run {i}"), &e, if local == remote { "OK" } else { "FAIL" }, format!("{dt:.2}s {:.0}MB/s sha_match={} peak RSS +{:.1}MiB chunk={chunk} width={width}", BIG_LEN as f64 / dt / 1e6, local == remote, hwm_mib() - base));
            }
            Err(x) => row("6", &format!("upload 1.1 GiB run {i}"), &e, verdict(&Err::<(), _>(x.clone())), x),
        }
        let _ = delete(&t, &path).await;
    }
    if ts.len() == 3 {
        let m = median(ts.clone());
        let worst = ts.iter().cloned().fold(0f64, f64::max);
        row("6", "upload x3: worst vs median", &e, if worst <= 3.0 * m { "OK" } else { "FAIL" }, format!("median {m:.2}s worst {worst:.2}s"));
    }
    // rename, delete, mkdir
    let a = format!("{UP}\\a.txt");
    let b = format!("{UP}\\b.txt");
    let c = format!("{UP}\\c.txt");
    let _ = write_all(&t, &a, 10, 10, 1).await;
    let _ = write_all(&t, &b, 20, 20, 1).await;
    let r1 = rename(&t, &a, &c, false).await;
    row("6", "rename a->c (target absent, no replace)", "fast", verdict(&r1), r1.err().unwrap_or_default());
    let r2 = rename(&t, &c, &b, false).await;
    row("6", "rename c->b without replace (target exists): must fail", "fast", if r2.is_err() { "OK" } else { "FAIL" }, format!("{:?}", r2));
    let r3 = rename(&t, &c, &b, true).await;
    let size = sh("docker exec rsmb-samba stat -c %s /srv/share/upload/spike-smbrs/b.txt");
    row("6", "rename c->b with replace", "fast", if r3.is_ok() && size == "10" { "OK" } else { "FAIL" }, format!("{:?}; b.txt now {size} bytes (expect 10)", r3));
    let r4 = delete(&t, &b).await;
    let gone = sh("docker exec rsmb-samba sh -c 'test -e /srv/share/upload/spike-smbrs/b.txt && echo present || echo gone'");
    row("6", "delete file", "fast", if r4.is_ok() && gone == "gone" { "OK" } else { "FAIL" }, format!("{:?} {gone}", r4));
    let d = format!("{UP}\\newdir");
    let r5 = mkdir(&t, &d).await;
    let r6 = delete(&t, &d).await;
    row("6", "mkdir + rmdir (delete-on-close on a directory)", "fast", if r5.is_ok() && r6.is_ok() { "OK" } else { "FAIL" }, format!("{:?} {:?}", r5, r6));
    let r7 = delete(&t, UP).await;
    row("6", "cleanup upload/spike-smbrs", "fast", verdict(&r7), r7.err().unwrap_or_default());
}

async fn step7() {
    let e = ep(SLOW).await;
    // counted, through the tap: 16-wide walk of tree/ after warm-up
    let tap = start_tap(SLOW);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let _ = within(60, walk(&t, "tree/d0/d0", 16).map(Ok)).await; // warm-up: credits
    for (root, width) in [("tree", 16), ("fanout", 16), ("tree", 1)] {
        settle(&tap);
        tap.reset();
        let t0 = Instant::now();
        let r = within(900, walk(&t, root, width).map(Ok)).await;
        let dt = t0.elapsed();
        settle(&tap);
        let st = tap.stats();
        row("7", &format!("walk {root}/ width={width} (through tap)"), &e, verdict(&r), format!("{:?} (dirs, files, denied) {} {}", r.unwrap_or((0, 0, 0)), ms(dt), counts(&st)));
    }
    drop(t);
    drop(s);
    // timed, direct
    let s = connect(&tcp(SLOW)).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let _ = within(60, walk(&t, "tree/d0/d0", 16).map(Ok)).await;
    for (root, width) in [("tree", 16), ("fanout", 16), ("tree", 1)] {
        let t0 = Instant::now();
        let r = within(900, walk(&t, root, width).map(Ok)).await;
        row("7", &format!("walk {root}/ width={width} (timed, direct)"), &e, verdict(&r), format!("{:?} {}", r.unwrap_or((0, 0, 0)), ms(t0.elapsed())));
    }
}

/// Needs the exclusive lock: resets the server-side connection counter.
async fn step7c() {
    let before = ctl("conns-reset samba; true");
    let r = within(120, async {
        let s = connect(&tcp(FAST)).await?;
        let t = s.tree("share").await?;
        let x = walk(&t, "fanout", 16).await;
        let y = walk(&t, "tree", 16).await;
        let (n, _) = read_all(&t, "large/medium.bin", 1 << 20, 16).await?;
        Ok(format!("fanout {x:?} tree {y:?} medium.bin {n} bytes"))
    })
    .await;
    let conns = ctl("conns samba");
    row("7", "one TCP connection (server-side count, fanout+tree walk+read)", &ep(FAST).await, if r.is_ok() && conns.trim() == "1" { "OK" } else { "FAIL" }, format!("server conns={conns} {}{before}", r.unwrap_or_else(|e| e)));
}

async fn step8() {
    let e = ep(EDGE).await;
    ctl("conns-reset samba-edge; true");
    let tap = start_tap(EDGE);
    let r = within(300, async {
        let s = connect(&tcp(tap.addr())).await?;
        let t = s.tree("share").await?;
        let info = format!("max_read={} max_write={} max_transact={}", s.max_read(), s.max_write(), s.max_transact());
        let w = walk(&t, "tree", 64).await;
        let w2 = walk(&t, "fanout", 64).await;
        let chunk = s.max_read() as usize;
        let (n, h) = read_all(&t, "large/medium.bin", chunk, 64).await?;
        let ok = h == server_sha("rsmb-samba-edge", "/srv/share/large/medium.bin");
        Ok(format!("{info}; walk tree width 64 {w:?}; walk fanout width 64 {w2:?}; medium.bin {n} bytes chunk={chunk} width=64 sha_ok={ok}"))
    })
    .await;
    settle(&tap);
    let st = tap.stats();
    let conns = ctl("conns samba-edge");
    let granted_max = st.response_log.iter().map(|r| r.credits_granted).max().unwrap_or(0);
    row("8", "samba-edge: 64 parallel listings + 50 MiB read at 64 KiB", &e, verdict(&r), format!("{} server conns={conns} max credits granted in one response={granted_max} {}", r.clone().unwrap_or_else(|e| e), counts(&st)));
}

async fn step9() {
    let names = sh(". ${REPO:-../..}/testenv/samba/names.sh; for v in ZH_HANS ZH_HANT KO_NFC KO_NFD JA_HIRA_NFD JA_KATA_NFD HAN_SMP FULLWIDTH IDEO_SPACE UMLAUT CYRILLIC EMOJI CAFE_NFC CAFE_NFD; do printf '%s\\n' \"${!v}\"; done");
    let names: Vec<String> = names.lines().map(|s| s.to_string()).collect();
    let s = connect(&tcp(FAST)).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let max = s.max_transact();
    for d in ["cjk", "special", "cjk/nested"] {
        let want = server_names(d);
        let r = within(60, list(&t, d, max, true)).await;
        match r {
            Ok(es) => {
                let got: BTreeSet<Vec<u8>> = es.iter().map(|e| e.name.clone()).collect();
                // open every file listed, by the name as listed
                let mut fails = vec![];
                for e in es.iter().filter(|e| !e.is_dir) {
                    let n = String::from_utf8_lossy(&e.name).to_string();
                    if let Err(x) = read_all(&t, &format!("{d}/{n}"), 65536, 1).await {
                        fails.push(format!("{n}: {x}"));
                    }
                }
                row("9", &format!("{d}/: list (bytes vs server) + open every file"), "fast", if got == want && fails.is_empty() { "OK" } else { "FAIL" }, format!("listed {} server {} equal={} open failures {}: {:?}", got.len(), want.len(), got == want, fails.len(), fails.iter().take(3).collect::<Vec<_>>()));
            }
            Err(x) => row("9", &format!("{d}/: list"), "fast", "ERROR", x),
        }
    }
    let nested = names[..9].join("/");
    let r = read_all(&t, &format!("cjk/nested/{nested}/leaf.txt"), 65536, 1).await;
    row("9", "open cjk/nested/<9 names>/leaf.txt by full path", "fast", verdict(&r), r.err().unwrap_or_default());
    // create in upload/
    let _ = mkdir(&t, UP).await;
    let mut fails = vec![];
    for n in &names {
        let dpath = format!("{UP}\\{n}");
        if let Err(x) = mkdir(&t, &dpath).await {
            fails.push(format!("mkdir {n}: {x}"));
            continue;
        }
        if let Err(x) = write_all(&t, &format!("{dpath}\\{n}.txt"), 5, 5, 1).await {
            fails.push(format!("create {n}.txt: {x}"));
        }
    }
    let want: BTreeSet<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
    let got = server_names("upload/spike-smbrs");
    let mut inner_ok = 0;
    for n in &names {
        if server_names(&format!("upload/spike-smbrs/{n}")).contains(format!("{n}.txt").as_bytes()) {
            inner_ok += 1;
        }
    }
    row("9", "create dir + file for 14 non-ASCII names, bytes checked on server", "fast", if fails.is_empty() && got == want && inner_ok == names.len() { "OK" } else { "FAIL" }, format!("server dirs equal={} files ok {inner_ok}/{}; failures {fails:?}", got == want, names.len()));
    for n in &names {
        let _ = delete(&t, &format!("{UP}\\{n}\\{n}.txt")).await;
        let _ = delete(&t, &format!("{UP}\\{n}")).await;
    }
    let _ = delete(&t, UP).await;
    // mangled/
    match within(60, list(&t, "mangled", max, true)).await {
        Ok(es) => {
            let mut opened = 0;
            let mut nf = vec![];
            for e in &es {
                let n = String::from_utf8_lossy(&e.name).to_string();
                match read_all(&t, &format!("mangled/{n}"), 65536, 1).await {
                    Ok(_) => opened += 1,
                    Err(x) => nf.push(format!("{n}:{}", if x.contains("NotFound") { "NOT_FOUND" } else { x.as_str() })),
                }
            }
            row("9", "mangled/: list + open every listed name", "fast", if es.len() == 9 && opened == 5 { "OK" } else { "INFO" }, format!("listed {} (smbclient: 9), opened {opened} (smbclient: 5), not opened {nf:?}", es.len()));
        }
        Err(x) => row("9", "mangled/: list", "fast", "ERROR", x),
    }
}

/// Needs the exclusive lock: pause, blackhole.
async fn step10() {
    // cut while idle
    let tap = start_tap(FAST);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    list(&t, "tree", 65536, true).await.unwrap();
    tokio::task::block_in_place(|| tap.drop_connections());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t0 = Instant::now();
    let r = within(60, list(&t, "tree", 65536, true)).await;
    row("10", "tap cuts connection while idle; next ls", &ep(FAST).await, if r.is_err() && !r.as_ref().err().unwrap().starts_with("TIMEOUT") { "OK" } else { "FAIL" }, format!("error after {}: {:?}", ms(t0.elapsed()), r.err()));
    drop(t);
    drop(s);

    // cut mid-transfer (slow endpoint so the transfer is long)
    let tap = start_tap(SLOW);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let cut = async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        tokio::task::block_in_place(|| tap.drop_connections());
        Instant::now()
    };
    let (r, at) = tokio::join!(within(120, read_all(&t, "large/big.bin", 1 << 20, 16)), cut);
    row("10", "tap cuts connection mid-download (big.bin, slow)", &ep(SLOW).await, if r.is_err() && !r.as_ref().err().unwrap().starts_with("TIMEOUT") { "OK" } else { "FAIL" }, format!("error {} after the cut: {:?}", ms(at.elapsed()), r.err()));
    drop(t);
    drop(s);

    // blackhole
    let r = within(30, async {
        let s = connect(&tcp(BLACKHOLE)).await?;
        let t = s.tree("share").await?;
        list(&t, "tree", 65536, true).await?;
        Ok((s, t))
    })
    .await;
    match r {
        Ok((s, t)) => {
            ctl("blackhole on");
            let t0 = Instant::now();
            let r = within(120, list(&t, "tree", 65536, true)).await;
            let dt = t0.elapsed();
            ctl("blackhole off");
            row("10", "blackhole on an open connection; next ls", &ep(BLACKHOLE).await, if r.is_err() && !r.as_ref().err().unwrap().starts_with("TIMEOUT") { "OK" } else { "FAIL" }, format!("result after {}: {:?}", ms(dt), r.err()));
            drop(t);
            drop(s);
        }
        Err(x) => row("10", "blackhole", "slow:4450", "ERROR", x),
    }

    // paused server
    let s = connect(&tcp(FAST)).await.unwrap();
    let t = s.tree("share").await.unwrap();
    list(&t, "tree", 65536, true).await.unwrap();
    ctl("pause samba");
    let t0 = Instant::now();
    let r = within(120, list(&t, "tree", 65536, true)).await;
    let dt = t0.elapsed();
    ctl("unpause samba");
    row("10", "server paused (docker pause); next ls", "fast", if r.is_err() && !r.as_ref().err().unwrap().starts_with("TIMEOUT") { "OK" } else { "FAIL" }, format!("result after {}: {:?}", ms(dt), r.err()));
    drop(t);
    drop(s);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let r = within(30, async { connect(&tcp(FAST)).await.map(|_| ()) }).await;
    row("10", "new connection after unpause", "fast", verdict(&r), r.err().unwrap_or_default());

    // a request dropped mid-read, then the next request on the same connection
    let e = ep(SLOW).await;
    let s = connect(&tcp(SLOW)).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let f = open_file(&t, "large/big.bin").await.unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let t0 = Instant::now();
    f.read_block(&mut buf, 0, None, false).await.unwrap();
    let base = t0.elapsed();
    let dropped = tokio::time::timeout(Duration::from_millis(50), f.read_block(&mut buf, 1 << 20, None, false)).await;
    let t0 = Instant::now();
    let r = within(60, async { f.read_block(&mut buf, 2 << 20, None, false).await.map_err(|e| e.to_string()) }).await;
    let next = t0.elapsed();
    let t1 = Instant::now();
    let r2 = within(60, list(&t, "tree", 65536, true)).await;
    row("10", "read dropped after 50 ms, then next read + ls on same connection", &e, if r.is_ok() && r2.is_ok() { "OK" } else { "FAIL" }, format!("dropped future completed={}; baseline 1 MiB read {}; next read {} ({:?}); ls {} ({})", dropped.is_ok(), ms(base), ms(next), r.as_ref().map(|_| "ok").map_err(|e| e.clone()), ms(t1.elapsed()), verdict(&r2)));
}

async fn step10i() {
    let secs: u64 = std::env::var("IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let tap = start_tap(FAST);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    list(&t, "tree", 65536, true).await.unwrap();
    settle(&tap);
    tap.reset();
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let st = tap.stats();
    let r = within(30, list(&t, "tree", 65536, true)).await;
    row("10", &format!("idle {secs}s: requests sent unasked"), "fast", if st.requests == 0 && st.client_frames == 0 { "OK" } else { "FAIL" }, format!("requests={} client_frames={}; ls after idle {} (crate has no echo/keepalive code: grep finds none)", st.requests, st.client_frames, verdict(&r)));
}

/// Needs the exclusive lock: fills `tiny`.
async fn step11() {
    let s = connect(&tcp(FAST)).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let show = |r: R<Resource>| match r {
        Ok(_) => "unexpected success".to_string(),
        Err(e) => e,
    };
    let r = t.create("forbidden\\secret.txt", &read_args()).await.map_err(|e| es(&e));
    row("11", "access denied: open forbidden/secret.txt", "fast", "INFO", show(r));
    let r = t.create("tree\\unreadable.txt", &read_args()).await.map_err(|e| es(&e));
    row("11", "access denied: open tree/unreadable.txt", "fast", "INFO", show(r));
    let r = t.create("nope.txt", &read_args()).await.map_err(|e| es(&e));
    row("11", "not found: open nope.txt", "fast", "INFO", show(r));
    let r = t.create("nodir\\nope.txt", &read_args()).await.map_err(|e| es(&e));
    row("11", "path not found: open nodir/nope.txt", "fast", "INFO", show(r));
    let ro = s.tree("readonly").await.unwrap();
    let r = ro
        .create("upload\\spike-smbrs-ro.txt", &FileCreateArgs::make_overwrite(FileAttributes::new(), CreateOptions::new()))
        .await
        .map_err(|e| es(&e));
    row("11", "write to read-only share: create file", "fast", "INFO", show(r));
    let tiny = s.tree("tiny").await.unwrap();
    let r = write_all(&tiny, "spike-smbrs-fill.bin", 12 << 20, 1 << 20, 4).await;
    row("11", "disk full on tiny: write 12 MiB to a 10 MiB share", "fast", if r.is_err() { "INFO" } else { "FAIL" }, format!("{:?} (write errors are std::io::Error::other(string): status not matchable)", r));
    let d = delete(&tiny, "spike-smbrs-fill.bin").await;
    row("11", "cleanup tiny", "fast", verdict(&d), d.err().unwrap_or_default());
}

async fn step13() {
    let e = ep(BAD).await;
    // wan-bad: 1 % loss at 300 ms RTT holds TCP to tens of KB/s, so one 1 MiB READ can take longer
    // than the crate's fixed per-request timeout (default 10 s, counted from the send).
    for secs in [10u64, 60] {
        let t0 = Instant::now();
        let r = within(300, async {
            let c = ConnectionConfig { timeout: Some(Duration::from_secs(secs)), ..cfg() };
            let s = connect_as(&tcp(BAD), "TESTDOM\\tester", "testpass", c).await?;
            let a = t0.elapsed();
            let t = s.tree("share").await?;
            let es = list(&t, "tree", 65536, true).await?;
            let b = t0.elapsed();
            let m = list(&t, "many", s.max_transact(), true).await?;
            let c = t0.elapsed();
            let (n, h) = read_all(&t, "sizes/1048577.bin", 1 << 20, 2).await?;
            let d = t0.elapsed();
            let ok = h == server_sha("rsmb-samba", "/srv/share/sizes/1048577.bin");
            Ok(format!("login {} ; ls tree ({} entries) at {}; ls many ({}) at {}; sizes/1048577.bin sha_ok={ok} at {} ({:.0} KB/s)", ms(a), es.len(), ms(b), m.len(), ms(c), ms(d), n as f64 / (d - c).as_secs_f64() / 1e3))
        })
        .await;
        row("13", &format!("smoke over wan-bad: login, ls tree, ls many, read 1 MiB+1 (request timeout {secs} s)"), &e, verdict(&r), r.unwrap_or_else(|e| e));
    }
}

/// Debug: read $FILE with $CHUNK x $WIDTH through the tap, print the end of the tap log.
async fn stepxr() {
    let file = std::env::var("FILE").unwrap_or("large/big.bin".into());
    let chunk: usize = std::env::var("CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(1 << 20);
    let width: usize = std::env::var("WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
    let tap = start_tap(FAST);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let t0 = Instant::now();
    let r = within(20, read_all(&t, &file, chunk, width)).await;
    eprintln!("result after {}: {:?}", ms(t0.elapsed()), r);
    let st = tap.stats();
    eprintln!("{}", counts(&st));
    let n = st.request_log.len();
    for q in &st.request_log[n.saturating_sub(6)..] { eprintln!("REQ {} id={} charge={} req={} at={:?}", q.command, q.message_id, q.credit_charge, q.credits_requested, q.at); }
    let n = st.response_log.len();
    for q in &st.response_log[n.saturating_sub(6)..] { eprintln!("RSP {} id={} status={:x} granted={} interim={} at={:?}", q.command, q.message_id, q.status, q.credits_granted, q.interim, q.at); }
    let total: u64 = st.response_log.iter().map(|r| r.credits_granted as u64).sum();
    let charged: u64 = st.request_log.iter().map(|r| r.credit_charge.max(1) as u64).sum();
    eprintln!("sum granted {total} sum charged {charged}");
}

/// Debug: list $DIR with $BUF through the tap, print the tap log.
async fn stepx() {
    let dir = std::env::var("DIR").unwrap_or("many".into());
    let buf: u32 = std::env::var("BUF").ok().and_then(|v| v.parse().ok()).unwrap_or(65536);
    let tap = start_tap(FAST);
    let s = connect(&tcp(tap.addr())).await.unwrap();
    let t = s.tree("share").await.unwrap();
    let r = within(15, list(&t, &dir, buf, true)).await;
    eprintln!("result: {:?}", r.map(|v| v.len()));
    let st = tap.stats();
    for q in &st.request_log { eprintln!("REQ {} id={} charge={} req={}", q.command, q.message_id, q.credit_charge, q.credits_requested); }
    for q in &st.response_log { eprintln!("RSP {} id={} status={:x} granted={}", q.command, q.message_id, q.status, q.credits_granted); }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let steps: Vec<String> = std::env::args().skip(1).collect();
    for s in &steps {
        match s.as_str() {
            "1" => step1().await,
            "2" => step2().await,
            "3" => step3().await,
            "4" => step4().await,
            "5" => step5().await,
            "6" => step6().await,
            "7" => step7().await,
            "7c" => step7c().await,
            "8" => step8().await,
            "9" => step9().await,
            "10" => step10().await,
            "10i" => step10i().await,
            "11" => step11().await,
            "13" => step13().await,
            "x" => stepx().await,
            "xr" => stepxr().await,
            x => eprintln!("unknown step {x}"),
        }
    }
}
