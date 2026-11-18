//! smbtap: a TCP forwarder between an SMB client and server that counts what crosses it.
//!
//! The client connects to the tap instead of the server. Every byte is forwarded unchanged in both
//! directions, with `TCP_NODELAY` on both sockets; a copy is cut into frames and counted. Nothing
//! the client reports about itself is trusted. docs/measuring.md explains how tests use it.
//!
//! Terms:
//! - **request**: one SMB2 header sent by the client. Each element of a compound chain counts.
//! - **response**: one SMB2 header sent by the server, interim responses included.
//! - **interim**: a response with `STATUS_PENDING` and the async flag; the final one follows later.
//! - **notification**: a server message nobody asked for (oplock or lease break, message id all
//!   ones). Not a response.
//! - **flight**: a new flight starts when the client sends a frame after at least one server frame
//!   has arrived on that connection since the client's previous send; the first send on a
//!   connection starts flight 1. At high latency wall time follows the number of flights.
//! - **outstanding**: requests without a final response yet.
//! - **opaque frame**: SMB1, encrypted (transform header) or compressed. Counted per kind, never
//!   parsed. Each opaque request counts as one outstanding until the next opaque response or
//!   unmatched SMB2 response on its connection, which makes the SMB1 NEGOTIATE (answered in SMB2)
//!   exact, but makes "outstanding" approximate on encrypted connections: an encrypted interim
//!   response cannot be told from a final one.
//!
//! Each frame is recorded when the tap has read all of it and before the tap forwards its last
//! bytes, so the order of records is the order of cause and effect.
//!
//! With `Tap::start_socks5` the upstream is a SOCKS5 proxy: the handshake in front of the SMB
//! stream is relayed message by message, each in one write, and not counted.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const STATUS_PENDING: u32 = 0x0000_0103;
const FLAG_ASYNC: u32 = 0x0000_0002;
const CMD_CANCEL: u16 = 0x000C;
const NOTIFICATION_ID: u64 = u64::MAX;

/// Name of an SMB2 command code.
pub fn command_name(code: u16) -> &'static str {
    const NAMES: [&str; 19] = [
        "NEGOTIATE",
        "SESSION_SETUP",
        "LOGOFF",
        "TREE_CONNECT",
        "TREE_DISCONNECT",
        "CREATE",
        "CLOSE",
        "FLUSH",
        "READ",
        "WRITE",
        "LOCK",
        "IOCTL",
        "CANCEL",
        "ECHO",
        "QUERY_DIRECTORY",
        "CHANGE_NOTIFY",
        "QUERY_INFO",
        "SET_INFO",
        "OPLOCK_BREAK",
    ];
    NAMES
        .get(code as usize)
        .copied()
        .unwrap_or("UNKNOWN_COMMAND")
}

/// One SMB2 request as the tap saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Connection number, 1 for the first connection the tap accepted.
    pub conn: u64,
    /// Time since the tap started.
    pub at: Duration,
    /// Flight number on this connection.
    pub flight: u64,
    pub command: &'static str,
    pub message_id: u64,
    /// Position in the compound chain (0 for the first) and length of the chain (1 if not compound).
    pub chain_index: usize,
    pub chain_len: usize,
    pub credit_charge: u16,
    pub credits_requested: u16,
}

/// One SMB2 response (interim or final) as the tap saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub conn: u64,
    pub at: Duration,
    pub command: &'static str,
    pub message_id: u64,
    pub status: u32,
    pub credits_granted: u16,
    pub interim: bool,
}

/// Everything counted since the tap started or was last reset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// TCP connections accepted from the client.
    pub connections: u64,
    /// Accepted connections for which the upstream connect failed.
    pub upstream_failures: u64,
    pub client_frames: u64,
    pub server_frames: u64,
    pub flights: u64,
    /// SMB2 requests; `per_command` splits them by command.
    pub requests: u64,
    /// SMB2 responses, interim ones included.
    pub responses: u64,
    pub interim: u64,
    pub notifications: u64,
    /// Opaque client frames; `per_command` counts them as SMB1, ENCRYPTED, COMPRESSED or UNKNOWN.
    pub opaque_requests: u64,
    pub opaque_responses: u64,
    /// Frames that start like SMB2 but whose header chain does not parse.
    pub malformed: u64,
    /// Requests outstanding when the snapshot was taken.
    pub outstanding: u64,
    /// Most requests outstanding at once.
    pub peak_outstanding: u64,
    pub per_command: BTreeMap<&'static str, u64>,
    pub request_log: Vec<Request>,
    pub response_log: Vec<Response>,
}

impl Stats {
    /// The counters as one line of JSON; with `with_log` also every request and response.
    pub fn to_json(&self, with_log: bool) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"connections\":{},\"upstream_failures\":{},\"client_frames\":{},\
             \"server_frames\":{},\"flights\":{},\"requests\":{},\"responses\":{},\"interim\":{},\
             \"notifications\":{},\"opaque_requests\":{},\"opaque_responses\":{},\"malformed\":{},\
             \"outstanding\":{},\"peak_outstanding\":{},\"per_command\":{{",
            self.connections,
            self.upstream_failures,
            self.client_frames,
            self.server_frames,
            self.flights,
            self.requests,
            self.responses,
            self.interim,
            self.notifications,
            self.opaque_requests,
            self.opaque_responses,
            self.malformed,
            self.outstanding,
            self.peak_outstanding
        );
        for (i, (name, n)) in self.per_command.iter().enumerate() {
            let _ = write!(s, "{}\"{name}\":{n}", if i > 0 { "," } else { "" });
        }
        s.push('}');
        if with_log {
            s.push_str(",\"request_log\":[");
            for (i, r) in self.request_log.iter().enumerate() {
                let _ = write!(
                    s,
                    "{}{{\"conn\":{},\"at_us\":{},\"flight\":{},\"command\":\"{}\",\
                     \"message_id\":{},\"chain_index\":{},\"chain_len\":{},\
                     \"credit_charge\":{},\"credits_requested\":{}}}",
                    if i > 0 { "," } else { "" },
                    r.conn,
                    r.at.as_micros(),
                    r.flight,
                    r.command,
                    r.message_id,
                    r.chain_index,
                    r.chain_len,
                    r.credit_charge,
                    r.credits_requested
                );
            }
            s.push_str("],\"response_log\":[");
            for (i, r) in self.response_log.iter().enumerate() {
                let _ = write!(
                    s,
                    "{}{{\"conn\":{},\"at_us\":{},\"command\":\"{}\",\"message_id\":{},\
                     \"status\":\"0x{:08x}\",\"credits_granted\":{},\"interim\":{}}}",
                    if i > 0 { "," } else { "" },
                    r.conn,
                    r.at.as_micros(),
                    r.command,
                    r.message_id,
                    r.status,
                    r.credits_granted,
                    r.interim
                );
            }
            s.push(']');
        }
        s.push('}');
        s
    }
}

/// Cuts a TCP byte stream into frames: a 4-byte header (zero byte, 24-bit big-endian length)
/// followed by the payload.
#[derive(Default)]
struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    /// Adds bytes as read from the socket; returns the payload of every frame now complete.
    fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        let mut pos = 0;
        while self.buf.len() - pos >= 4 {
            let b = &self.buf[pos..];
            let len = u32::from_be_bytes([0, b[1], b[2], b[3]]) as usize;
            if b.len() - 4 < len {
                break;
            }
            frames.push(b[4..4 + len].to_vec());
            pos += 4 + len;
        }
        self.buf.drain(..pos);
        frames
    }
}

struct Header {
    credit_charge: u16,
    status: u32,
    command: u16,
    credits: u16,
    flags: u32,
    message_id: u64,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The headers of an SMB2 frame's compound chain, or None if the frame is not plain SMB2 or the
/// chain does not parse.
fn parse_chain(frame: &[u8]) -> Option<Vec<Header>> {
    let mut headers = Vec::new();
    let mut off = 0;
    loop {
        let h = frame.get(off..off + 64)?;
        if h[..4] != *b"\xfeSMB" {
            return None;
        }
        headers.push(Header {
            credit_charge: le16(h, 6),
            status: le32(h, 8),
            command: le16(h, 12),
            credits: le16(h, 14),
            flags: le32(h, 16),
            message_id: u64::from_le_bytes(h[24..32].try_into().unwrap()),
        });
        match le32(h, 20) as usize {
            0 => return Some(headers),
            next if next < 64 => return None,
            next => off += next,
        }
    }
}

fn opaque_kind(frame: &[u8]) -> &'static str {
    match frame.get(..4) {
        Some(b"\xffSMB") => "SMB1",
        Some(b"\xfdSMB") => "ENCRYPTED",
        Some(b"\xfcSMB") => "COMPRESSED",
        _ => "UNKNOWN",
    }
}

#[derive(Default)]
struct Conn {
    from_client: Framer,
    from_server: Framer,
    sent: bool,
    answered: bool,
    flight: u64,
    opaque_outstanding: u64,
    /// The message ids of each plain request frame that may still wait for an answer, oldest
    /// first (for an encrypted answer to a plain request, `server_frame`).
    frames: VecDeque<Vec<u64>>,
}

/// All counting state. Pure: fed bytes and times, no sockets, so the unit tests drive it directly.
struct Engine {
    start: Instant,
    stats: Stats,
    conns: HashMap<u64, Conn>,
    outstanding: HashSet<(u64, u64)>,
    last_client_frame: Option<Instant>,
}

impl Engine {
    fn new(start: Instant) -> Engine {
        Engine {
            start,
            stats: Stats::default(),
            conns: HashMap::new(),
            outstanding: HashSet::new(),
            last_client_frame: None,
        }
    }

    fn open(&mut self, conn: u64) {
        self.stats.connections += 1;
        self.conns.insert(conn, Conn::default());
    }

    /// The connection is gone: nothing it sent can still be answered.
    fn close(&mut self, conn: u64) {
        self.conns.remove(&conn);
        self.outstanding.retain(|&(c, _)| c != conn);
    }

    fn outstanding(&self) -> u64 {
        self.outstanding.len() as u64
            + self
                .conns
                .values()
                .map(|c| c.opaque_outstanding)
                .sum::<u64>()
    }

    fn reset(&mut self) {
        self.stats = Stats {
            peak_outstanding: self.outstanding(),
            ..Stats::default()
        };
    }

    fn snapshot(&self) -> Stats {
        Stats {
            outstanding: self.outstanding(),
            ..self.stats.clone()
        }
    }

    fn data(&mut self, conn: u64, from_client: bool, data: &[u8], now: Instant) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        let framer = if from_client {
            &mut c.from_client
        } else {
            &mut c.from_server
        };
        for frame in framer.push(data) {
            if from_client {
                self.client_frame(conn, &frame, now);
            } else {
                self.server_frame(conn, &frame, now);
            }
        }
    }

    fn client_frame(&mut self, conn: u64, frame: &[u8], now: Instant) {
        let at = now.saturating_duration_since(self.start);
        let c = self.conns.get_mut(&conn).expect("open connection");
        if !c.sent || c.answered {
            c.flight += 1;
            self.stats.flights += 1;
        }
        c.sent = true;
        c.answered = false;
        self.stats.client_frames += 1;
        self.last_client_frame = Some(now);
        let waiting = &self.outstanding;
        while c
            .frames
            .front()
            .is_some_and(|f| f.iter().all(|id| !waiting.contains(&(conn, *id))))
        {
            c.frames.pop_front();
        }
        match parse_chain(frame) {
            Some(chain) => {
                let ids = chain.iter().filter(|h| h.command != CMD_CANCEL);
                c.frames.push_back(ids.map(|h| h.message_id).collect());
                for (i, h) in chain.iter().enumerate() {
                    let command = command_name(h.command);
                    self.stats.requests += 1;
                    *self.stats.per_command.entry(command).or_default() += 1;
                    // A CANCEL gets no response of its own.
                    if h.command != CMD_CANCEL {
                        self.outstanding.insert((conn, h.message_id));
                    }
                    self.stats.request_log.push(Request {
                        conn,
                        at,
                        flight: c.flight,
                        command,
                        message_id: h.message_id,
                        chain_index: i,
                        chain_len: chain.len(),
                        credit_charge: h.credit_charge,
                        credits_requested: h.credits,
                    });
                }
            }
            None if frame.starts_with(b"\xfeSMB") => self.stats.malformed += 1,
            None => {
                self.stats.opaque_requests += 1;
                *self
                    .stats
                    .per_command
                    .entry(opaque_kind(frame))
                    .or_default() += 1;
                c.opaque_outstanding += 1;
            }
        }
        self.stats.peak_outstanding = self.stats.peak_outstanding.max(self.outstanding());
    }

    fn server_frame(&mut self, conn: u64, frame: &[u8], now: Instant) {
        let at = now.saturating_duration_since(self.start);
        let c = self.conns.get_mut(&conn).expect("open connection");
        c.answered = true;
        self.stats.server_frames += 1;
        match parse_chain(frame) {
            Some(chain) => {
                for h in chain {
                    if h.message_id == NOTIFICATION_ID {
                        self.stats.notifications += 1;
                        continue;
                    }
                    let interim = h.status == STATUS_PENDING && h.flags & FLAG_ASYNC != 0;
                    self.stats.responses += 1;
                    if interim {
                        self.stats.interim += 1;
                    } else if !self.outstanding.remove(&(conn, h.message_id)) {
                        // The answer to an opaque request, such as the SMB2 NEGOTIATE response
                        // to an SMB1 negotiate.
                        c.opaque_outstanding = c.opaque_outstanding.saturating_sub(1);
                    }
                    self.stats.response_log.push(Response {
                        conn,
                        at,
                        command: command_name(h.command),
                        message_id: h.message_id,
                        status: h.status,
                        credits_granted: h.credits,
                        interim,
                    });
                }
            }
            None if frame.starts_with(b"\xfeSMB") => self.stats.malformed += 1,
            None => {
                self.stats.opaque_responses += 1;
                if c.opaque_outstanding > 0 {
                    c.opaque_outstanding -= 1;
                    return;
                }
                // An encrypted answer to a plain request: Samba encrypts its answer to a compound
                // whose TREE_CONNECT reaches a share that requires encryption. It answers the
                // oldest plain request frame still waiting.
                while let Some(ids) = c.frames.pop_front() {
                    let mut any = false;
                    for id in ids {
                        any |= self.outstanding.remove(&(conn, id));
                    }
                    if any {
                        break;
                    }
                }
            }
        }
    }
}

struct Shared {
    engine: Mutex<Engine>,
    /// Both sockets of every live connection, so they can be dropped on demand.
    live: Mutex<HashMap<u64, [TcpStream; 2]>>,
    stopping: AtomicBool,
}

impl Shared {
    fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn live(&self) -> MutexGuard<'_, HashMap<u64, [TcpStream; 2]>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A running tap. Dropping it stops listening and drops every live connection.
pub struct Tap {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Tap {
    /// Listens on 127.0.0.1 with a port chosen by the system; forwards each connection to `upstream`.
    pub fn start(upstream: impl ToSocketAddrs) -> io::Result<Tap> {
        Tap::start_on("127.0.0.1:0", upstream)
    }

    pub fn start_on(listen: impl ToSocketAddrs, upstream: impl ToSocketAddrs) -> io::Result<Tap> {
        Tap::open(listen, upstream, false)
    }

    /// Like `start`, with a SOCKS5 proxy as upstream: the handshake in front of the SMB stream is
    /// relayed, each message in one write, and not counted.
    pub fn start_socks5(proxy: impl ToSocketAddrs) -> io::Result<Tap> {
        Tap::open("127.0.0.1:0", proxy, true)
    }

    fn open(
        listen: impl ToSocketAddrs,
        upstream: impl ToSocketAddrs,
        socks5: bool,
    ) -> io::Result<Tap> {
        let upstream: Vec<SocketAddr> = upstream.to_socket_addrs()?.collect();
        let listener = TcpListener::bind(listen)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            engine: Mutex::new(Engine::new(Instant::now())),
            live: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
        });
        let s = shared.clone();
        thread::spawn(move || accept_loop(listener, upstream, socks5, s));
        Ok(Tap { addr, shared })
    }

    /// Where clients connect.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A copy of the counters, with `outstanding` filled in.
    pub fn stats(&self) -> Stats {
        self.shared.engine().snapshot()
    }

    /// Zeroes the counters and logs. Requests still outstanding stay tracked.
    pub fn reset(&self) {
        self.shared.engine().reset();
    }

    /// Requests sent that have no final response yet.
    pub fn outstanding(&self) -> u64 {
        self.shared.engine().outstanding()
    }

    pub fn live_connections(&self) -> usize {
        self.shared.live().len()
    }

    /// Shuts down both sockets of every live connection (the client sees the connection close).
    /// Returns how many connections were dropped.
    pub fn drop_connections(&self) -> usize {
        let live = self.shared.live();
        for socket in live.values().flatten() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        live.len()
    }

    /// Waits until nothing is outstanding and no client frame has arrived for `quiet`. Tests call
    /// this between commands, so a request a client sends in the background (a CLOSE after `cd`)
    /// is charged to the command that caused it. Returns false if `timeout` passes first.
    pub fn settle(&self, quiet: Duration, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let e = self.shared.engine();
                let quiet_now = e.last_client_frame.is_none_or(|t| t.elapsed() >= quiet);
                if quiet_now && e.outstanding() == 0 {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr); // wakes the accept loop
        self.drop_connections();
    }
}

fn accept_loop(
    listener: TcpListener,
    upstream: Vec<SocketAddr>,
    socks5: bool,
    shared: Arc<Shared>,
) {
    let mut next = 0;
    for client in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = client else { continue };
        next += 1;
        let conn = next;
        shared.engine().open(conn);
        let (upstream, shared) = (upstream.clone(), shared.clone());
        thread::spawn(move || serve(conn, client, &upstream, socks5, &shared));
    }
}

fn serve(
    conn: u64,
    mut client: TcpStream,
    upstream: &[SocketAddr],
    socks5: bool,
    shared: &Arc<Shared>,
) {
    let mut server = match TcpStream::connect(upstream) {
        Ok(s) => s,
        Err(_) => {
            let mut e = shared.engine();
            e.stats.upstream_failures += 1;
            e.close(conn);
            return;
        }
    };
    let clones = || Some([client.try_clone().ok()?, server.try_clone().ok()?]);
    let (Some(for_live), Some([c2, s2])) = (clones(), clones()) else {
        shared.engine().close(conn);
        return;
    };
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    // In `live` before the handshake, so `drop_connections` and `Drop` reach one that stalls.
    shared.live().insert(conn, for_live);
    if socks5 && socks5_preamble(&mut client, &mut server).is_err() {
        let _ = client.shutdown(Shutdown::Both);
        let _ = server.shutdown(Shutdown::Both);
        shared.live().remove(&conn);
        shared.engine().close(conn);
        return;
    }
    let sh = shared.clone();
    let back = thread::spawn(move || pump(conn, s2, c2, false, &sh));
    pump(conn, client, server, true, shared);
    let _ = back.join();
    shared.live().remove(&conn);
    shared.engine().close(conn);
}

fn take(from: &mut TcpStream, n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    from.read_exact(&mut b)?;
    Ok(b)
}

fn not_socks5(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("SOCKS5: {what}"))
}

/// A request or a reply: `VER CMD/REP RSV ATYP`, the address (4, 16 or 1 + n bytes), the port.
fn socks5_address_message(from: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut m = take(from, 4)?;
    let len = match m[3] {
        1 => 4,
        4 => 16,
        3 => {
            let n = take(from, 1)?;
            m.extend(&n);
            n[0] as usize
        }
        _ => return Err(not_socks5("address type")),
    };
    m.extend(take(from, len + 2)?);
    Ok(m)
}

/// RFC 1928 and 1929: greeting, method, user and password if the proxy chose them, request,
/// reply. Reads each message whole, then writes it once: a proxy may parse one `recv` per
/// message (microsocks answers a request split over two writes with a general failure).
fn socks5_preamble(client: &mut TcpStream, proxy: &mut TcpStream) -> io::Result<()> {
    let mut greeting = take(client, 2)?;
    if greeting[0] != 5 {
        return Err(not_socks5("version"));
    }
    greeting.extend(take(client, greeting[1] as usize)?);
    proxy.write_all(&greeting)?;
    let method = take(proxy, 2)?;
    client.write_all(&method)?;
    match method[1] {
        0 => {}
        2 => {
            let mut login = take(client, 2)?;
            login.extend(take(client, login[1] as usize)?);
            let plen = take(client, 1)?;
            login.extend(&plen);
            login.extend(take(client, plen[0] as usize)?);
            proxy.write_all(&login)?;
            let status = take(proxy, 2)?;
            client.write_all(&status)?;
            if status[1] != 0 {
                return Err(not_socks5("login refused"));
            }
        }
        _ => return Err(not_socks5("no acceptable method")),
    }
    let request = socks5_address_message(client)?;
    proxy.write_all(&request)?;
    let reply = socks5_address_message(proxy)?;
    client.write_all(&reply)?;
    if reply[1] != 0 {
        return Err(not_socks5("connect refused"));
    }
    Ok(())
}

/// Copies one direction until end of stream or error, recording each read before forwarding it.
fn pump(conn: u64, mut from: TcpStream, mut to: TcpStream, from_client: bool, shared: &Shared) {
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Write); // pass the half-close on
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        shared
            .engine()
            .data(conn, from_client, &buf[..n], Instant::now());
        if to.write_all(&buf[..n]).is_err() {
            break;
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEGOTIATE: u16 = 0;
    const TREE_CONNECT: u16 = 3;
    const CREATE: u16 = 5;
    const CLOSE: u16 = 6;
    const READ: u16 = 8;
    const QUERY_INFO: u16 = 16;
    const RESPONSE: u32 = 1;
    const RELATED: u32 = 4;

    /// A 64-byte SMB2 header followed by `body` zero bytes.
    fn msg(command: u16, flags: u32, message_id: u64, status: u32, body: usize) -> Vec<u8> {
        let mut h = vec![0u8; 64 + body];
        h[..4].copy_from_slice(b"\xfeSMB");
        h[4..6].copy_from_slice(&64u16.to_le_bytes());
        h[6..8].copy_from_slice(&1u16.to_le_bytes()); // credit charge
        h[8..12].copy_from_slice(&status.to_le_bytes());
        h[12..14].copy_from_slice(&command.to_le_bytes());
        h[14..16].copy_from_slice(&(if flags & RESPONSE != 0 { 7u16 } else { 3 }).to_le_bytes());
        h[16..20].copy_from_slice(&flags.to_le_bytes());
        h[24..32].copy_from_slice(&message_id.to_le_bytes());
        h
    }

    /// Links messages into a compound chain by setting each NextCommand offset.
    fn compound(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            let mut p = p.clone();
            if i + 1 < parts.len() {
                let next = p.len() as u32;
                p[20..24].copy_from_slice(&next.to_le_bytes());
            }
            out.extend(p);
        }
        out
    }

    /// Adds the 4-byte length header.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut f = (payload.len() as u32).to_be_bytes().to_vec();
        f.extend(payload);
        f
    }

    fn engine() -> (Engine, Instant) {
        let now = Instant::now();
        let mut e = Engine::new(now);
        e.open(1);
        (e, now)
    }

    #[test]
    fn plain_request_and_response() {
        let (mut e, now) = engine();
        e.data(1, true, &frame(&msg(CREATE, 0, 5, 0, 56)), now);
        assert_eq!(
            (e.stats.requests, e.stats.flights, e.outstanding()),
            (1, 1, 1)
        );
        e.data(1, false, &frame(&msg(CREATE, RESPONSE, 5, 0, 88)), now);
        let s = e.snapshot();
        assert_eq!((s.responses, s.outstanding, s.peak_outstanding), (1, 0, 1));
        assert_eq!(s.per_command.get("CREATE"), Some(&1));
        let r = &s.request_log[0];
        assert_eq!(
            (r.command, r.message_id, r.chain_index, r.chain_len),
            ("CREATE", 5, 0, 1)
        );
        assert_eq!((r.credit_charge, r.credits_requested, r.flight), (1, 3, 1));
        assert_eq!(
            (s.response_log[0].status, s.response_log[0].credits_granted),
            (0, 7)
        );
    }

    #[test]
    fn compound_chain_of_three() {
        let (mut e, now) = engine();
        let req = compound(&[
            msg(CREATE, 0, 10, 0, 64),
            msg(QUERY_INFO, RELATED, 11, 0, 48),
            msg(CLOSE, RELATED, 12, 0, 24),
        ]);
        e.data(1, true, &frame(&req), now);
        let s = e.snapshot();
        assert_eq!(
            (s.requests, s.client_frames, s.flights, s.outstanding),
            (3, 1, 1, 3)
        );
        let pos: Vec<_> = s
            .request_log
            .iter()
            .map(|r| (r.command, r.chain_index, r.chain_len))
            .collect();
        assert_eq!(
            pos,
            [("CREATE", 0, 3), ("QUERY_INFO", 1, 3), ("CLOSE", 2, 3)]
        );
        let resp = compound(&[
            msg(CREATE, RESPONSE | RELATED, 10, 0, 88),
            msg(QUERY_INFO, RESPONSE | RELATED, 11, 0, 16),
            msg(CLOSE, RESPONSE | RELATED, 12, 0, 60),
        ]);
        e.data(1, false, &frame(&resp), now);
        assert_eq!((e.stats.responses, e.outstanding()), (3, 0));
    }

    #[test]
    fn encrypted_transform_frames_are_opaque() {
        let (mut e, now) = engine();
        let mut transform = b"\xfdSMB".to_vec();
        transform.extend([0x5a; 48 + 200]); // transform header rest + ciphertext
        e.data(1, true, &frame(&transform), now);
        let s = e.snapshot();
        assert_eq!((s.requests, s.opaque_requests, s.outstanding), (0, 1, 1));
        assert_eq!(s.per_command.get("ENCRYPTED"), Some(&1));
        e.data(1, false, &frame(&transform), now);
        assert_eq!(
            (e.stats.opaque_responses, e.outstanding(), e.stats.flights),
            (1, 0, 1)
        );
    }

    /// Samba encrypts its answer to a plain compound whose TREE_CONNECT reaches a share that
    /// requires encryption: it answers the oldest plain request frame still waiting.
    #[test]
    fn encrypted_answer_to_a_plain_frame() {
        let (mut e, now) = engine();
        e.data(1, true, &frame(&msg(CREATE, 0, 1, 0, 56)), now);
        e.data(1, false, &frame(&msg(CREATE, RESPONSE, 1, 0, 88)), now);
        let a = compound(&[msg(TREE_CONNECT, 0, 2, 0, 8), msg(CLOSE, RELATED, 3, 0, 24)]);
        e.data(1, true, &frame(&a), now);
        e.data(1, true, &frame(&msg(READ, 0, 4, 0, 48)), now);
        assert_eq!(e.outstanding(), 3);
        let mut transform = b"\xfdSMB".to_vec();
        transform.extend([0x5a; 48 + 200]);
        e.data(1, false, &frame(&transform), now);
        assert_eq!((e.stats.opaque_responses, e.outstanding()), (1, 1));
        e.data(1, false, &frame(&msg(READ, RESPONSE, 4, 0, 16)), now);
        assert_eq!(e.outstanding(), 0);
    }

    #[test]
    fn smb1_negotiate_answered_in_smb2() {
        let (mut e, now) = engine();
        let mut smb1 = b"\xffSMB\x72".to_vec(); // SMB_COM_NEGOTIATE
        smb1.extend([0; 60]);
        e.data(1, true, &frame(&smb1), now);
        assert_eq!(e.stats.per_command.get("SMB1"), Some(&1));
        assert_eq!(e.outstanding(), 1);
        e.data(1, false, &frame(&msg(NEGOTIATE, RESPONSE, 0, 0, 64)), now);
        assert_eq!((e.stats.responses, e.outstanding()), (1, 0));
        e.data(1, true, &frame(&msg(NEGOTIATE, 0, 1, 0, 36)), now);
        assert_eq!((e.stats.flights, e.stats.requests), (2, 1));
    }

    #[test]
    fn frame_split_across_reads_and_two_frames_in_one_read() {
        let (mut e, now) = engine();
        let f = frame(&msg(CREATE, 0, 1, 0, 56));
        e.data(1, true, &f[..2], now); // inside the length header
        e.data(1, true, &f[2..30], now); // inside the SMB2 header
        assert_eq!(e.stats.client_frames, 0);
        e.data(1, true, &f[30..], now);
        assert_eq!((e.stats.client_frames, e.stats.requests), (1, 1));

        let mut two = frame(&msg(CREATE, 0, 2, 0, 56));
        two.extend(frame(&msg(CREATE, 0, 3, 0, 56)));
        e.data(1, true, &two, now);
        // Three sends with no response in between: still one flight.
        assert_eq!(
            (e.stats.requests, e.stats.flights, e.outstanding()),
            (3, 1, 3)
        );
        assert_eq!(e.stats.peak_outstanding, 3);
    }

    #[test]
    fn interim_response_keeps_request_outstanding() {
        let (mut e, now) = engine();
        e.data(1, true, &frame(&msg(READ, 0, 7, 0, 48)), now);
        e.data(
            1,
            false,
            &frame(&msg(READ, RESPONSE | FLAG_ASYNC, 7, STATUS_PENDING, 8)),
            now,
        );
        assert_eq!((e.stats.interim, e.outstanding()), (1, 1));
        e.data(
            1,
            false,
            &frame(&msg(READ, RESPONSE | FLAG_ASYNC, 7, 0, 80)),
            now,
        );
        assert_eq!(
            (e.stats.responses, e.stats.interim, e.outstanding()),
            (2, 1, 0)
        );
        assert!(e.stats.response_log[0].interim && !e.stats.response_log[1].interim);
    }

    #[test]
    fn flights_cancel_and_notifications() {
        let (mut e, now) = engine();
        e.data(1, true, &frame(&msg(CREATE, 0, 1, 0, 56)), now);
        e.data(1, false, &frame(&msg(CREATE, RESPONSE, 1, 0, 88)), now);
        e.data(1, true, &frame(&msg(CLOSE, 0, 2, 0, 24)), now);
        e.data(1, true, &frame(&msg(12, 0, 2, 0, 4)), now); // CANCEL: no response expected
        assert_eq!((e.stats.flights, e.outstanding()), (2, 1));
        e.data(1, false, &frame(&msg(18, RESPONSE, u64::MAX, 0, 24)), now); // oplock break
        assert_eq!(
            (e.stats.notifications, e.stats.responses, e.outstanding()),
            (1, 1, 1)
        );
        // A second connection has its own flights; closing it forgets what it left outstanding.
        e.open(2);
        e.data(2, true, &frame(&msg(CREATE, 0, 1, 0, 56)), now);
        assert_eq!((e.stats.flights, e.outstanding()), (3, 2));
        e.close(2);
        assert_eq!(e.outstanding(), 1);
    }

    #[test]
    fn malformed_chain_is_counted_not_parsed() {
        let (mut e, now) = engine();
        let mut bad = msg(CREATE, 0, 1, 0, 8);
        bad[20..24].copy_from_slice(&8u32.to_le_bytes()); // next header inside this one
        e.data(1, true, &frame(&bad), now);
        assert_eq!(
            (e.stats.malformed, e.stats.requests, e.outstanding()),
            (1, 0, 0)
        );
    }

    // Frames recorded with tshark from the host's smbclient 4.25 against samba (4.23.8),
    // complete with their 4-byte length header.
    /// NEGOTIATE request.
    const REC_NEGOTIATE_REQ: &str = concat!(
        "00000100fe534d42400000000000000000001f00000000000000000000000000000000000000000000000000",
        "00000000000000000000000000000000000000000000000024000500010000007f0000003232683f4727b043",
        "9437ba7520767f2c700000000500000002021002000302031103000001002600000000000100200001000b6b",
        "118afacc6f7674c8131e18d6dddfc220d1014483eeb18ef9a4564bbfd3c0000002000a000000000004000200",
        "010004000300000000000000080008000000000003000200010000000500180000000000310030002e003200",
        "310033002e00340035002e0031003000000110000000000093ad25509cb411e7b42383de968bcd7c",
    );
    /// NEGOTIATE response.
    const REC_NEGOTIATE_RESP: &str = concat!(
        "00000138fe534d42400000000000000000000100010000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000410001001103040072736d620000000000000000",
        "0000000027000000000080000000800000008000daeb7da5de76dd01000000000000000080004a00d0000000",
        "604806062b0601050502a03e303ca00e300c060a2b06010401823702020aa32a3028a0261b246e6f745f6465",
        "66696e65645f696e5f5246433431373840706c656173655f69676e6f72650000000000000100260000000000",
        "01002000010047208b369ba29ec4849e340dd63b174570d308291c892070fca91998312edf92000002000400",
        "00000000010002000000000008000400000000000100020000000000000110000000000093ad25509cb411e7",
        "b42383de968bcd7c",
    );
    /// First encrypted request on the `encrypted` share (transform header).
    const REC_ENCRYPTED_REQ: &str = concat!(
        "000000b8fd534d423fbd8aba739f9a6b1f6589320f84b18101000000000000002fb1a4e60000000084000000",
        "000001002f2aa66c00000000f296dcdcce55de8d88cd36f1f9c95a69886089f04e70d85fa1820b7f78b9d3af",
        "db6ba415ecc93a0813f566529d19a1d8c1e49be79cb6727f92ad3f26535905faa4ef98e918f21d5f39530316",
        "6a3f648fadd40505fbb92801bffe5010a7a9ade9dcd9a10d78db87ed868df2ddec419b60da01fb75e8e78898",
        "2a40888b56f1947fd76ad238",
    );
    /// Its encrypted response.
    const REC_ENCRYPTED_RESP: &str = concat!(
        "000000ccfd534d42cc3dbc66a53cbbc494efbec73fc5c7250100000000000000c6db9d570000000098000000",
        "000001002f2aa66c000000006ce19b5fcc06d2ec5875cf7800db8dc895567e1ddd13c1a4f4f4091d2b149b5b",
        "3e371f18b1e663b68a8b2a94019e354fa0fbd0bc21067812dd618bf644794b3f10b416a869bb670ab3330332",
        "be93bb22ed89d8cb78ecae0c07e3fcb835d8f026404514956c4290bd9a8bad435b50e08f57665dfbcacde35b",
        "a620243a8718d524764ce62515baee750823446cbcdec2294ef67370e52732cf",
    );
    /// Interim STATUS_PENDING response to READ message id 1327, during a 50 MiB download.
    const REC_READ_INTERIM: &str = concat!(
        "00000049fe534d4240000000030100000800800013000000000000002f050000000000002f05000000000000",
        "8894bd210000000000000000000000000000000000000000090000000000000021",
    );

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn recorded_smbclient_frames() {
        let (mut e, now) = engine();
        e.data(1, true, &unhex(REC_NEGOTIATE_REQ), now);
        e.data(1, false, &unhex(REC_NEGOTIATE_RESP), now);
        assert_eq!(e.stats.per_command.get("NEGOTIATE"), Some(&1));
        assert_eq!((e.stats.responses, e.outstanding()), (1, 0));
        assert_eq!(e.stats.response_log[0].status, 0);

        e.data(1, true, &unhex(REC_ENCRYPTED_REQ), now);
        assert_eq!((e.stats.opaque_requests, e.outstanding()), (1, 1));
        e.data(1, false, &unhex(REC_ENCRYPTED_RESP), now);
        assert_eq!((e.stats.opaque_responses, e.outstanding()), (1, 0));
        assert_eq!(e.stats.per_command.get("ENCRYPTED"), Some(&1));

        e.data(1, true, &frame(&msg(READ, 0, 1327, 0, 48)), now);
        e.data(1, false, &unhex(REC_READ_INTERIM), now);
        assert_eq!((e.stats.interim, e.outstanding()), (1, 1));
        assert_eq!(e.stats.response_log.last().unwrap().status, STATUS_PENDING);
        assert_eq!(e.stats.flights, 3);
    }

    /// The tap end to end: a fake server answers every frame with a response to the same message id.
    #[test]
    fn forwards_counts_settles_and_drops() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = server.local_addr().unwrap();
        thread::spawn(move || {
            for s in server.incoming() {
                let mut s = s.unwrap();
                let mut f = Framer::default();
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = s.read(&mut buf) {
                    for req in f.push(&buf[..n]) {
                        let mut resp = req[..64].to_vec();
                        resp[16] |= RESPONSE as u8;
                        s.write_all(&frame(&resp)).unwrap();
                    }
                }
            }
        });
        let tap = Tap::start(upstream).unwrap();
        let mut c = TcpStream::connect(tap.addr()).unwrap();
        c.write_all(&frame(&msg(CREATE, 0, 1, 0, 56))).unwrap();
        let mut resp = [0u8; 68];
        c.read_exact(&mut resp).unwrap();
        assert_eq!(&resp[4..8], b"\xfeSMB");
        assert!(tap.settle(Duration::from_millis(20), Duration::from_secs(5)));
        let s = tap.stats();
        assert_eq!(
            (s.connections, s.requests, s.responses, s.flights),
            (1, 1, 1, 1)
        );
        assert_eq!(tap.live_connections(), 1);

        assert_eq!(tap.drop_connections(), 1);
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(
            c.read(&mut resp).unwrap_or(0),
            0,
            "client sees the connection end"
        );
        tap.reset();
        assert_eq!(tap.stats(), Stats::default());
    }

    /// A fake SOCKS5 proxy that reads each handshake message with one `read` (and fails the test
    /// if a message arrives in pieces), then answers SMB2 frames like the fake server above.
    fn fake_proxy(login: bool) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let mut one = |s: &mut TcpStream, want: &[u8]| {
                let n = s.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], want, "a handshake message in one piece");
            };
            if login {
                one(&mut s, &[5, 2, 0, 2]);
                s.write_all(&[5, 2]).unwrap();
                one(&mut s, b"\x01\x02us\x02pw");
                s.write_all(&[1, 0]).unwrap();
            } else {
                one(&mut s, &[5, 1, 0]);
                s.write_all(&[5, 0]).unwrap();
            }
            one(&mut s, b"\x05\x01\x00\x03\x0bexample.com\x01\xbd");
            s.write_all(&[5, 0, 0, 1, 10, 0, 0, 1, 1, 189]).unwrap();
            let mut f = Framer::default();
            while let Ok(n @ 1..) = s.read(&mut buf) {
                for req in f.push(&buf[..n]) {
                    let mut resp = req[..64].to_vec();
                    resp[16] |= RESPONSE as u8;
                    s.write_all(&frame(&resp)).unwrap();
                }
            }
        });
        addr
    }

    #[test]
    fn socks5_preamble_is_relayed_and_not_counted() {
        for login in [true, false] {
            let tap = Tap::start_socks5(fake_proxy(login)).unwrap();
            let mut c = TcpStream::connect(tap.addr()).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut all = Vec::new();
            if login {
                all.extend([5, 2, 0, 2]);
                all.extend(b"\x01\x02us\x02pw");
            } else {
                all.extend([5, 1, 0]);
            }
            all.extend(b"\x05\x01\x00\x03\x0bexample.com\x01\xbd");
            // The client sends everything at once; the tap still relays message by message.
            c.write_all(&all).unwrap();
            let mut answers = vec![0u8; if login { 2 + 2 + 10 } else { 2 + 10 }];
            c.read_exact(&mut answers).unwrap();
            assert_eq!(&answers[answers.len() - 10..answers.len() - 8], [5, 0]);
            let mut two = frame(&msg(CREATE, 0, 1, 0, 56));
            two.extend(frame(&msg(CREATE, 0, 2, 0, 56)));
            c.write_all(&two).unwrap();
            let mut resp = [0u8; 136];
            c.read_exact(&mut resp).unwrap();
            assert!(tap.settle(Duration::from_millis(20), Duration::from_secs(5)));
            let s = tap.stats();
            assert_eq!(
                (
                    s.connections,
                    s.requests,
                    s.flights,
                    s.client_frames,
                    s.server_frames
                ),
                (1, 2, 1, 2, 2),
                "login {login}: {s:?}"
            );
            assert_eq!((s.opaque_requests, s.malformed), (0, 0), "login {login}");
        }

        // A proxy that refuses every method and closes.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = l.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 16];
            let _ = s.read(&mut b);
            s.write_all(&[5, 0xff]).unwrap();
        });
        let tap = Tap::start_socks5(proxy).unwrap();
        let mut c = TcpStream::connect(tap.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).unwrap();
        assert_eq!(got, [5, 0xff], "the two bytes, then the end of the stream");
        let deadline = Instant::now() + Duration::from_secs(5);
        while tap.live_connections() > 0 {
            assert!(
                Instant::now() < deadline,
                "the refused connection stays live"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let s = tap.stats();
        assert_eq!((s.connections, s.client_frames, s.server_frames), (1, 0, 0));

        // A handshake that stalls: `drop_connections` closes it.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = l.local_addr().unwrap();
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            thread::sleep(Duration::from_secs(30));
            drop(s);
        });
        let tap = Tap::start_socks5(proxy).unwrap();
        let mut c = TcpStream::connect(tap.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tap.live_connections() == 0 {
            assert!(
                Instant::now() < deadline,
                "the connection never became live"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(tap.drop_connections(), 1);
        let mut b = [0u8; 4];
        assert_eq!(c.read(&mut b).unwrap_or(0), 0, "the client sees the end");
        let deadline = Instant::now() + Duration::from_secs(5);
        while tap.live_connections() > 0 {
            assert!(
                Instant::now() < deadline,
                "the stalled connection stays live"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
