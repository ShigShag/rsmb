//! The harness for the integration tests (docs/testing.md): the real binary, driven line by line
//! in `--json` mode or on a pseudo-terminal, with `smbtap` between it and the server. Nothing here
//! waits without a limit: every wait kills the child and panics with what was read so far.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use smbtap::{Stats, Tap};

pub const BIN: &str = env!("CARGO_BIN_EXE_rsmb");
pub const SLOW: &str = "10.213.45.30:445";
pub const FAST: &str = "10.213.45.10:445";
/// The container behind the fast endpoint.
pub const SAMBA: &str = "rsmb-samba";
pub const MEDIUM_SHA: &str = "50556313740b0f8de619aaeccebf8c30140d68c25957b2d8164d53c374cc62ca";
pub const HOLE: &str = "10.213.45.30:4450";
pub const LOGIN: [&str; 6] = ["-u", "tester", "-p", "testpass", "-d", "TESTDOM"];
pub const WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ep {
    Fast,
    Slow,
    Edge,
    Bad,
    /// The private server through the proxy's port 1080 (no login at the proxy).
    Proxied,
    /// The private server through the proxy's port 1081 (`tester:testpass` at the proxy).
    ProxiedLogin,
}

/// The private server's name, which only the proxy resolves.
pub const PRIVATE: &str = "fileserver.rsmb.test";

impl Ep {
    /// What a tap in front of the endpoint forwards to: the server, or for the proxied two the
    /// proxy's listener.
    pub fn addr(self) -> &'static str {
        match self {
            Ep::Fast => "10.213.45.10:445",
            Ep::Slow => "10.213.45.30:445",
            Ep::Edge => "10.213.45.11:445",
            Ep::Bad => "10.213.45.31:445",
            Ep::Proxied => "10.213.45.20:1080",
            Ep::ProxiedLogin => "10.213.45.20:1081",
        }
    }
    /// The container whose disk the endpoint serves.
    pub fn container(self) -> &'static str {
        match self {
            Ep::Edge => "rsmb-samba-edge",
            Ep::Proxied | Ep::ProxiedLogin => "rsmb-samba-private",
            _ => "rsmb-samba",
        }
    }
    pub fn proxied(self) -> bool {
        matches!(self, Ep::Proxied | Ep::ProxiedLogin)
    }
    /// A new tap in front of the endpoint: `Tap::start`, or `Tap::start_socks5` for the proxied two.
    pub fn tap(self) -> Tap {
        if self.proxied() {
            Tap::start_socks5(self.addr()).unwrap()
        } else {
            Tap::start(self.addr()).unwrap()
        }
    }
    /// The flags that reach the endpoint, without the login. Direct: `-H <host> -P <port>` of the
    /// server, or of `tap`. Proxied: `--proxy socks5://[tester:testpass@]<proxy or tap> -H
    /// fileserver.rsmb.test -P 445`.
    pub fn route(self, tap: Option<&Tap>) -> Vec<String> {
        let addr = tap.map_or(self.addr().to_string(), |t| t.addr().to_string());
        if self.proxied() {
            let login = if self == Ep::ProxiedLogin {
                "tester:testpass@"
            } else {
                ""
            };
            let url = format!("socks5://{login}{addr}");
            ["--proxy", &url, "-H", PRIVATE, "-P", "445"]
                .map(String::from)
                .to_vec()
        } else {
            let (h, p) = split(&addr);
            vec!["-H".into(), h, "-P".into(), p]
        }
    }
    /// The host as the prompt and `pwd` show it: the tap's 127.0.0.1 or the server's address for
    /// a direct endpoint, `fileserver.rsmb.test` for a proxied one.
    pub fn host(self, tapped: bool) -> &'static str {
        match self {
            _ if self.proxied() => PRIVATE,
            _ if tapped => "127.0.0.1",
            _ => self.addr().rsplit_once(':').unwrap().0,
        }
    }
    /// `route(tap)` and the user login.
    pub fn args(self, tap: Option<&Tap>) -> Vec<String> {
        let mut v = self.route(tap);
        v.extend(LOGIN.iter().map(|s| s.to_string()));
        v
    }
}

/// The endpoint of tests/fast.rs: `Ep::Fast`, or `Ep::Proxied` when RSMB_TEST_VIA=socks5 (any
/// other value panics).
pub fn fast() -> Ep {
    match std::env::var("RSMB_TEST_VIA").as_deref() {
        Err(_) | Ok("") => Ep::Fast,
        Ok("socks5") => Ep::Proxied,
        Ok(v) => panic!("RSMB_TEST_VIA={v}: only socks5 is known"),
    }
}

/// Lines of the proxy's log that contain `needle` (`docker logs rsmb-socks`).
pub fn proxy_lines(needle: &str) -> usize {
    let o = Command::new("docker")
        .args(["logs", "rsmb-socks"])
        .output()
        .unwrap();
    assert!(o.status.success(), "docker logs rsmb-socks");
    [&o.stdout[..], &o.stderr[..]]
        .iter()
        .flat_map(|b| b.split(|c| *c == b'\n'))
        .filter(|l| l.windows(needle.len()).any(|w| w == needle.as_bytes()))
        .count()
}

fn split(addr: &str) -> (String, String) {
    let (h, p) = addr.rsplit_once(':').unwrap();
    (h.to_string(), p.to_string())
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// An empty directory for this test and this route (`<target tmp>/direct/<test>` or
/// `…/socks5/<test>`), wiped at every call: a direct and a proxied run at once keep apart.
pub fn tmp(test: &str) -> PathBuf {
    let route = if fast().proxied() { "socks5" } else { "direct" };
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(route)
        .join(test);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Reads everything from `r` in a thread into a shared string.
fn collect(mut r: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let s = Arc::new(Mutex::new(String::new()));
    let s2 = s.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            s2.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    s
}

fn wait_child(child: &mut Child, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            use std::os::unix::process::ExitStatusExt;
            return Some(st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub took: Duration,
}

impl Output {
    pub fn json(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l:?}: {e}")))
            .collect()
    }
}

/// One-shot: rsmb with `args`, stdin from `stdin` (None: /dev/null), at most 60 s.
pub fn run(args: &[&str], stdin: Option<&str>) -> Output {
    run_env(args, stdin, &[])
}

pub fn run_env(args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let t0 = Instant::now();
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RSMB_HISTORY", "");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        let mut w = child.stdin.take().unwrap();
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(input.as_bytes());
        });
    }
    let out = collect(child.stdout.take().unwrap());
    let err = collect(child.stderr.take().unwrap());
    let code = wait_child(&mut child, WAIT);
    std::thread::sleep(Duration::from_millis(20));
    let (stdout, stderr) = (out.lock().unwrap().clone(), err.lock().unwrap().clone());
    let Some(code) = code else {
        panic!("rsmb {args:?} did not end within 60 s\nstdout: {stdout}\nstderr: {stderr}");
    };
    Output {
        code,
        stdout,
        stderr,
        took: t0.elapsed(),
    }
}

/// Login flags and `-H/-P` for `addr`.
pub fn target(addr: &str) -> Vec<String> {
    let (h, p) = split(addr);
    let mut v = vec!["-H".to_string(), h, "-P".to_string(), p];
    v.extend(LOGIN.iter().map(|s| s.to_string()));
    v
}

pub struct Reply {
    /// The objects before `done`.
    pub lines: Vec<Value>,
    pub ok: bool,
    pub stats: Option<Stats>,
    pub took: Duration,
}

impl Reply {
    pub fn kind(&self) -> Option<String> {
        self.lines
            .iter()
            .find(|v| v["type"] == "error")
            .map(|v| v["kind"].as_str().unwrap().to_string())
    }
    pub fn of(&self, t: &str) -> Vec<&Value> {
        self.lines.iter().filter(|v| v["type"] == t).collect()
    }
    pub fn names(&self) -> BTreeSet<Vec<u8>> {
        self.of("entry")
            .iter()
            .map(|v| v["name"].as_str().unwrap().as_bytes().to_vec())
            .collect()
    }
    pub fn stats(&self) -> &Stats {
        self.stats.as_ref().expect("no tap on this client")
    }
    /// The `path` of every `entry` object, without the leading `/`.
    pub fn paths(&self) -> BTreeSet<Vec<u8>> {
        self.of("entry")
            .iter()
            .map(|v| {
                let p = v["path"].as_str().unwrap();
                p.strip_prefix('/').unwrap_or(p).as_bytes().to_vec()
            })
            .collect()
    }
    /// Panics unless the command cost exactly `requests` named requests in `flights` flights.
    pub fn expect(&self, what: &str, requests: u64, flights: u64) {
        let s = self.stats();
        if s.requests != requests || s.flights != flights {
            panic!(
                "{what}: expected {requests} requests in {flights} flights, got {} requests \
                 ({} opaque) in {} flights\nper command: {:?}\nlog:\n{}",
                s.requests,
                s.opaque_requests,
                s.flights,
                s.per_command,
                log(s)
            );
        }
    }
    pub fn expect_ok(&self, what: &str) {
        assert!(self.ok, "{what}: failed: {:?}", self.lines);
    }
}

/// The request log, one line per request.
pub fn log(s: &Stats) -> String {
    s.request_log
        .iter()
        .map(|r| {
            format!(
                "  conn {} flight {} {} chain {}/{}\n",
                r.conn, r.flight, r.command, r.chain_index, r.chain_len
            )
        })
        .collect()
}

/// What a count assertion prints after its message: the per-command counts and the request log.
pub fn context(s: &Stats) -> String {
    format!("per command: {:?}\nlog:\n{}", s.per_command, log(s))
}

pub fn count(s: &Stats, command: &str) -> u64 {
    s.per_command.get(command).copied().unwrap_or(0)
}

/// One rsmb process fed line by line: `--json --keep-going`, stdin and stdout piped.
pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    stderr: Arc<Mutex<String>>,
    tap: Option<Tap>,
    pub what: String,
    /// The `pwd` every constructor sends first: what start-up cost.
    pub startup: Reply,
}

impl Client {
    /// Through a new tap, user login.
    pub fn tapped(ep: Ep) -> Client {
        Client::with(None, Some(ep), &LOGIN)
    }
    /// No tap, user login.
    pub fn direct(addr: &str) -> Client {
        Client::with(Some(addr), None, &LOGIN)
    }
    /// The endpoint without a tap, user login (and `extra`).
    pub fn to(ep: Ep, extra: &[&str]) -> Client {
        let mut args = LOGIN.to_vec();
        args.extend(extra);
        Client::build(ep.route(None), None, &args)
    }
    /// Full control: `addr` (None with a tap: the tap; None without: `args` names the host).
    pub fn with(addr: Option<&str>, tap: Option<Ep>, args: &[&str]) -> Client {
        match (tap, addr) {
            (Some(ep), _) => {
                let t = ep.tap();
                Client::build(ep.route(Some(&t)), Some(t), args)
            }
            (None, Some(a)) => {
                let (h, p) = split(a);
                Client::build(vec!["-H".into(), h, "-P".into(), p], None, args)
            }
            (None, None) => Client::build(vec![], None, args),
        }
    }
    /// A tap in front of any address (the blackhole endpoint, which has no `Ep`).
    pub fn tapped_at(addr: &str, args: &[&str]) -> Client {
        let t = Tap::start(addr).unwrap();
        let (h, p) = split(&t.addr().to_string());
        Client::build(vec!["-H".into(), h, "-P".into(), p], Some(t), args)
    }
    /// A session whose tap the test made itself; `args` is the whole command line (route and
    /// login). The client owns the tap; `take_tap()` gives it back, and dropping it closes its port.
    pub fn through(tap: Tap, args: &[&str]) -> Client {
        Client::build(vec![], Some(tap), args)
    }
    fn build(route: Vec<String>, tap: Option<Tap>, args: &[&str]) -> Client {
        let mut argv = route;
        argv.extend(args.iter().map(|s| s.to_string()));
        argv.extend(["--json".to_string(), "--keep-going".to_string()]);
        let mut child = Command::new(BIN)
            .args(&argv)
            .env("RSMB_HISTORY", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| Value::String(format!("NOT JSON ({e}): {line}")));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        let stderr = collect(child.stderr.take().unwrap());
        let mut c = Client {
            stdin: child.stdin.take(),
            child,
            lines: rx,
            stderr,
            tap,
            what: format!("rsmb {}", argv.join(" ")),
            startup: Reply {
                lines: vec![],
                ok: false,
                stats: None,
                took: Duration::ZERO,
            },
        };
        c.startup = c.cmd("pwd");
        assert!(c.startup.ok, "start-up failed: {:?}", c.startup.lines);
        c
    }

    pub fn tap(&self) -> &Tap {
        self.tap.as_ref().expect("no tap")
    }
    /// The tap, kept alive after the process has gone (for what a signal made it send).
    pub fn take_tap(&mut self) -> Option<Tap> {
        self.tap.take()
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send(&mut self, line: &str) {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{line}").unwrap();
        w.flush().unwrap();
    }

    fn die(&mut self, why: &str, got: &[Value]) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::thread::sleep(Duration::from_millis(50));
        panic!(
            "{}: {why}\nreceived: {got:#?}\nstderr: {}",
            self.what,
            self.stderr.lock().unwrap()
        );
    }

    /// Objects up to `done`; no tap work.
    pub fn read_done(&mut self, limit: Duration) -> (Vec<Value>, bool) {
        let deadline = Instant::now() + limit;
        let mut got = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(v) if v["type"] == "done" => return (got, v["ok"] == true),
                Ok(v) => got.push(v),
                Err(RecvTimeoutError::Timeout) => self.die("no done line in time", &got),
                Err(RecvTimeoutError::Disconnected) => self.die("rsmb closed stdout", &got),
            }
        }
    }

    /// Reads to `done`, settles the tap, reads and resets it.
    pub fn wait_done(&mut self, limit: Duration) -> Reply {
        let t0 = Instant::now();
        let (lines, ok) = self.read_done(limit);
        let took = t0.elapsed();
        let stats = self.tap.as_ref().map(|t| {
            assert!(
                t.settle(Duration::from_millis(300), WAIT),
                "tap did not settle"
            );
            let s = t.stats();
            t.reset();
            s
        });
        Reply {
            lines,
            ok,
            stats,
            took,
        }
    }

    pub fn cmd(&mut self, line: &str) -> Reply {
        self.cmd_within(line, WAIT)
    }

    pub fn cmd_within(&mut self, line: &str, limit: Duration) -> Reply {
        let t0 = Instant::now();
        self.send(line);
        let mut r = self.wait_done(limit);
        r.took = t0.elapsed();
        r
    }

    /// Waits until the tap has seen `n` requests of `command` since its last reset.
    pub fn wait_requests(&self, command: &str, n: u64, limit: Duration) {
        wait_requests(self.tap(), command, n, limit)
    }

    /// VmHWM of the process, kB.
    pub fn peak_rss_kb(&self) -> u64 {
        let s = std::fs::read_to_string(format!("/proc/{}/status", self.pid())).unwrap();
        s.lines()
            .find(|l| l.starts_with("VmHWM:"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            .unwrap()
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// `kill -<signal>` to the rsmb process.
    pub fn kill(&mut self, signal: &str) {
        let ok = Command::new("kill")
            .args([format!("-{signal}"), self.pid().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -{signal}");
    }

    /// Closes stdin and waits at most 60 s: exit code and stderr.
    pub fn finish(mut self) -> (i32, String) {
        drop(self.stdin.take());
        let code = wait_child(&mut self.child, WAIT);
        std::thread::sleep(Duration::from_millis(20));
        let err = self.stderr();
        match code {
            Some(c) => (c, err),
            None => panic!("{}: did not exit within 60 s; stderr: {err}", self.what),
        }
    }

    /// Waits for the process to end on its own (after a signal).
    pub fn wait_exit(mut self, limit: Duration) -> (i32, Vec<Value>) {
        let code = wait_child(&mut self.child, limit);
        let mut rest = Vec::new();
        while let Ok(v) = self.lines.recv_timeout(Duration::from_millis(200)) {
            rest.push(v);
        }
        match code {
            Some(c) => (c, rest),
            None => panic!("{}: did not exit within {limit:?}: {rest:?}", self.what),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn wait_requests(tap: &Tap, command: &str, n: u64, limit: Duration) {
    let deadline = Instant::now() + limit;
    while count(&tap.stats(), command) < n {
        assert!(
            Instant::now() < deadline,
            "the tap did not see {n} {command} within {limit:?}: {:?}",
            tap.stats().per_command
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Names in a directory of the share as the server's disk has them.
pub fn server_names(container: &str, dir: &str) -> BTreeSet<Vec<u8>> {
    let o = Command::new("docker")
        .args([
            "exec",
            container,
            "sh",
            "-c",
            &format!("ls -A '/srv/share/{dir}'"),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "docker exec ls {dir}");
    o.stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| l.to_vec())
        .collect()
}

/// A variable of testenv/samba/names.sh, one name per line.
pub fn fixture_names(var: &str) -> Vec<String> {
    let names = root().join("testenv/samba/names.sh");
    let o = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}'; printf '%s\\n' \"${var}\"",
            names.display()
        ))
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "sourcing testenv/samba/names.sh for {var}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

pub fn fixture(var: &str) -> String {
    let v = fixture_names(var);
    assert_eq!(v.len(), 1, "{var}");
    v[0].clone()
}

/// testenv/ctl.sh
pub fn ctl(args: &[&str]) -> String {
    let o = Command::new(root().join("testenv/ctl.sh"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ctl.sh {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// Median of 5 TCP connects after one warm-up, ms.
pub fn connect_ms(addr: &str) -> f64 {
    let a = addr.to_socket_addrs().unwrap().next().unwrap();
    let once = || {
        let t0 = Instant::now();
        let s = TcpStream::connect_timeout(&a, Duration::from_secs(5)).unwrap();
        let t = t0.elapsed().as_secs_f64() * 1e3;
        drop(s);
        t
    };
    once();
    let mut v: Vec<f64> = (0..5).map(|_| once()).collect();
    v.sort_by(f64::total_cmp);
    v[2]
}

/// The tokeniser's quoting, written again here on purpose.
pub fn q(name: &str) -> String {
    if name.is_empty() {
        return "''".into();
    }
    let mut s = String::new();
    for c in name.chars() {
        if matches!(
            c,
            ' ' | '\t' | '\r' | '\n' | '\x0b' | '\x0c' | '\'' | '"' | '\\'
        ) {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

/// Undoes a server pause or the blackhole when dropped, also when the test fails.
pub struct Guard(pub Vec<&'static str>);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = Command::new(root().join("testenv/ctl.sh"))
            .args(&self.0)
            .output();
    }
}

/// rsmb on a pseudo-terminal: `python3 testenv/ptyrun.py 200 50 <rsmb> <args…>`.
pub struct Pty {
    child: Child,
    stdin: ChildStdin,
    chunks: Receiver<Vec<u8>>,
    all: Vec<u8>,
    seen: usize,
}

impl Pty {
    /// `args` in full (host, login); `TERM=xterm`; history in `history`.
    pub fn start(args: &[&str], history: &Path) -> Pty {
        Pty::command(BIN, args, history)
    }

    pub fn command(program: &str, args: &[&str], history: &Path) -> Pty {
        let mut child = Command::new("python3")
            .arg(root().join("testenv/ptyrun.py"))
            .args(["200", "50", program])
            .args(args)
            .env("TERM", "xterm")
            .env("RSMB_HISTORY", history)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Pty {
            stdin: child.stdin.take().unwrap(),
            child,
            chunks: rx,
            all: Vec::new(),
            seen: 0,
        }
    }

    /// Through a new tap at `ep`, user login: waits for the first prompt, settles, returns the
    /// start-up's stats and resets the tap.
    pub fn tapped(ep: Ep, extra: &[&str], history: &Path) -> (Pty, Tap, Stats) {
        let tap = ep.tap();
        let route = ep.args(Some(&tap));
        let mut args: Vec<&str> = route.iter().map(String::as_str).collect();
        args.extend(extra);
        let mut p = Pty::start(&args, history);
        p.expect(&format!("//{}> ", ep.host(true)), WAIT);
        assert!(tap.settle(Duration::from_millis(300), WAIT));
        let s = tap.stats();
        tap.reset();
        (p, tap, s)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.stdin.write_all(bytes).unwrap();
        self.stdin.flush().unwrap();
    }

    fn pump(&mut self, until: Instant) -> bool {
        let left = until.saturating_duration_since(Instant::now());
        match self.chunks.recv_timeout(left) {
            Ok(c) => {
                self.all.extend(c);
                true
            }
            Err(_) => false,
        }
    }

    fn find(&self, needle: &[u8]) -> Option<usize> {
        self.all[self.seen..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| self.seen + i)
    }

    /// Output since the last expect, up to and including `needle`.
    pub fn expect(&mut self, needle: &str, limit: Duration) -> String {
        self.expect_bytes(needle.as_bytes(), limit)
    }

    pub fn expect_bytes(&mut self, needle: &[u8], limit: Duration) -> String {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(i) = self.find(needle) {
                let end = i + needle.len();
                let s = String::from_utf8_lossy(&self.all[self.seen..end]).into_owned();
                self.seen = end;
                return s;
            }
            if !self.pump(deadline) {
                let _ = self.child.kill();
                panic!(
                    "pty: {:?} did not come within {limit:?}; transcript:\n{}",
                    String::from_utf8_lossy(needle),
                    self.all.escape_ascii()
                );
            }
        }
    }

    /// Everything received so far that no expect has consumed (waits `quiet` for more).
    pub fn rest(&mut self, quiet: Duration) -> String {
        while self.pump(Instant::now() + quiet) {}
        let s = String::from_utf8_lossy(&self.all[self.seen..]).into_owned();
        self.seen = self.all.len();
        s
    }

    pub fn transcript(&self) -> String {
        self.all.escape_ascii().to_string()
    }

    /// pid of rsmb (the child of ptyrun.py).
    pub fn rsmb_pid(&self) -> u32 {
        let o = Command::new("pgrep")
            .args(["-P", &self.child.id().to_string()])
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().parse().unwrap()
    }

    /// Waits for the exit, then everything the pty received: exit code and transcript (raw).
    pub fn finish(mut self, limit: Duration) -> (i32, Vec<u8>) {
        let Some(code) = wait_child(&mut self.child, limit) else {
            panic!(
                "pty: no exit within {limit:?}; transcript:\n{}",
                self.transcript()
            );
        };
        while self.pump(Instant::now() + Duration::from_secs(5)) {}
        (code, std::mem::take(&mut self.all))
    }

    pub fn exit_code(mut self, limit: Duration) -> i32 {
        match wait_child(&mut self.child, limit) {
            Some(c) => c,
            None => panic!(
                "pty: no exit within {limit:?}; transcript:\n{}",
                self.transcript()
            ),
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A process-wide lock for tests that must not overlap.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Bodies shared by tests/fast.rs and tests/slow.rs (the same row at another endpoint).

pub fn check_startup(ep: Ep) {
    let c = Client::tapped(ep);
    let s = c.startup.stats();
    c.startup.expect(
        &format!("start-up on {ep:?}: NEGOTIATE 1, SESSION_SETUP 2 and nothing else"),
        3,
        3,
    );
    assert_eq!(count(s, "NEGOTIATE"), 1);
    assert_eq!(count(s, "SESSION_SETUP"), 2);
    assert_eq!(
        s.connections, 1,
        "start-up on {ep:?}: expected 1 connection"
    );
}

pub fn check_use(ep: Ep) {
    let mut c = Client::tapped(ep);
    let r = c.cmd("use share");
    r.expect_ok("use share");
    r.expect(&format!("use share (first) on {ep:?}"), 1, 1);
    assert_eq!(count(r.stats(), "TREE_CONNECT"), 1);
    c.cmd("use share")
        .expect(&format!("use share (again) on {ep:?}"), 0, 0);
    let r = c.cmd("use readonly");
    r.expect_ok("use readonly");
    r.expect(&format!("use readonly on {ep:?}"), 1, 1);
    c.cmd("use share")
        .expect(&format!("use share (third) on {ep:?}"), 0, 0);
}

/// `cd` into `path` costs CREATE and CLOSE in one compound, and `pwd` shows it.
pub fn check_cd_one(c: &mut Client, ep: Ep, line: &str, path: &str) {
    let r = c.cmd(line);
    r.expect_ok(line);
    r.expect(&format!("{line} on {ep:?}"), 2, 1);
    let log = &r.stats().request_log;
    let shape: Vec<(&str, usize, usize)> = log
        .iter()
        .map(|q| (q.command, q.chain_index, q.chain_len))
        .collect();
    assert_eq!(
        shape,
        [("CREATE", 0, 2), ("CLOSE", 1, 2)],
        "{line} on {ep:?}"
    );
    let p = c.cmd("pwd");
    assert_eq!(p.of("pwd")[0]["path"], path, "{line}");
}

pub fn check_cd(ep: Ep) {
    let mut c = Client::tapped(ep);
    c.cmd("use share").expect_ok("use share");
    check_cd_one(&mut c, ep, "cd tree/d0/d1", "/tree/d0/d1");
    let ideo = fixture("IDEO_SPACE");
    check_cd_one(
        &mut c,
        ep,
        &format!("cd /cjk/{ideo}"),
        &format!("/cjk/{ideo}"),
    );
    let umlaut = fixture("UMLAUT");
    check_cd_one(
        &mut c,
        ep,
        &format!("cd \"/special/dir with space/{umlaut}\""),
        &format!("/special/dir with space/{umlaut}"),
    );
}

/// `ls <dir>` through the tap: names equal the server's, 5 requests in 1 flight.
pub fn check_ls(c: &mut Client, ep: Ep, dir: &str) -> Reply {
    let line = format!("ls {}", q(dir));
    let r = c.cmd(&line);
    r.expect_ok(&line);
    let want = server_names(ep.container(), dir);
    assert_eq!(
        r.of("entry").len(),
        want.len(),
        "{line} on {ep:?}: entry count"
    );
    if r.names() != want {
        let got = r.names();
        let missing: Vec<_> = want
            .difference(&got)
            .take(5)
            .map(|n| n.escape_ascii().to_string())
            .collect();
        let extra: Vec<_> = got
            .difference(&want)
            .take(5)
            .map(|n| n.escape_ascii().to_string())
            .collect();
        panic!(
            "{line} on {ep:?}: names differ from the server's; missing {missing:?}, extra {extra:?}"
        );
    }
    let names: Vec<&str> = r
        .of("entry")
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.is_sorted(), "{line}: entries not sorted");
    r
}

/// Counts of one `ls` on Samba: CREATE 1, QUERY_DIRECTORY 3, CLOSE 1, in 1 flight.
pub fn expect_samba_ls(r: &Reply, what: &str) {
    r.expect(what, 5, 1);
    let s = r.stats();
    assert_eq!(
        (
            count(s, "CREATE"),
            count(s, "QUERY_DIRECTORY"),
            count(s, "CLOSE")
        ),
        (1, 3, 1),
        "{what}: expected CREATE 1, QUERY_DIRECTORY 3, CLOSE 1: {:?}",
        s.per_command
    );
}

impl Pty {
    /// Types `text` and Enter, waits for the newline and then for `prompt`.
    pub fn line(&mut self, text: &str, prompt: &str) -> String {
        self.send(format!("{text}\r").as_bytes());
        self.expect("\r\n", WAIT);
        self.expect(prompt, WAIT)
    }
}

/// Settles the tap, returns its stats and resets it.
pub fn take(tap: &Tap) -> Stats {
    assert!(
        tap.settle(Duration::from_millis(300), WAIT),
        "tap did not settle"
    );
    let s = tap.stats();
    tap.reset();
    s
}

pub fn expect_stats(s: &Stats, what: &str, requests: u64, flights: u64) {
    assert!(
        s.requests == requests && s.flights == flights,
        "{what}: expected {requests} requests in {flights} flights, got {} in {}\nper command: \
         {:?}\nlog:\n{}",
        s.requests,
        s.flights,
        s.per_command,
        log(s)
    );
}

pub const CTRL_U: &[u8] = b"\x15";
pub const BEL: &[u8] = b"\x07";

/// Tab after `text` on a fresh line; waits for `shows` (or the BEL), returns the tap's stats.
pub fn tab(p: &mut Pty, tap: &Tap, text: &str, shows: Option<&str>) -> Stats {
    p.send(CTRL_U);
    p.send(format!("{text}\t").as_bytes());
    match shows {
        Some(s) => p.expect(s, WAIT),
        None => p.expect_bytes(BEL, WAIT),
    };
    take(tap)
}

/// `ls many/f1` Tab, then a second completion in the same directory.
pub fn check_tab_many(p: &mut Pty, tap: &Tap, ep: Ep) {
    let s = tab(p, tap, "ls many/f1", Some("ls many/f10000 "));
    if ep == Ep::Edge {
        assert_eq!(
            s.flights,
            4,
            "first Tab in many/ on edge: expected 4 flights\n{}",
            context(&s)
        );
    } else {
        expect_stats(&s, &format!("first Tab in many/ on {ep:?}"), 5, 1);
    }
    let s = tab(p, tap, "ls many/f0999", None);
    expect_stats(&s, &format!("second Tab in many/ on {ep:?}"), 0, 0);
}

/// The scripted session of X1, X2 and F14: every Stage 1 command, then every Stage 2 command in
/// the local directory `dir` and the remote scratch directory `scratch`.
pub fn session_script(dir: &Path, scratch: &str) -> Vec<String> {
    let ko = fixture("KO_NFC");
    [
        "pwd",
        "shares",
        "use share",
        "cd tree/d0",
        "ls",
        &format!("cd /cjk/{ko}"),
        "ls /many",
        "refresh",
        "help all",
        "use readonly",
        "ls",
        "use share",
        "cd ..",
        "shares",
        &format!("lcd {}", q(&dir.to_string_lossy())),
        &format!("cd {scratch}"),
        "get /sizes/65537.bin",
        "put 65537.bin",
        "put -f 65537.bin",
        "cat /tree/d0/file.txt",
        "mkdir d",
        "put 65537.bin d/",
        "rm d/65537.bin",
        "rmdir d",
        "get -r /smallfiles/d01",
        "put -r d01",
        "tree -L 2 /tree",
        "find /tree -name file.txt",
        "find /fanout -maxdepth 1 -type d",
        "lpwd",
        "lls",
        "exit",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ---------------------------------------------------------------------------------------------
// Stage 2: scratch directories on the servers, checksums, trees, temporary files, waits

static SCRATCH_N: AtomicUsize = AtomicUsize::new(0);

/// `sh -c <script>` in a container; panics with stderr unless it succeeds. Returns stdout.
pub fn docker_sh(container: &str, script: &str) -> String {
    let o = Command::new("docker")
        .args(["exec", container, "sh", "-c", script])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "docker exec {container} sh -c {script:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A directory of its own under `upload/` on `ep`'s server, owned by tester; removed on drop,
/// also when the test fails.
pub struct Scratch {
    pub container: &'static str,
    pub name: String,
}

impl Scratch {
    pub fn new(ep: Ep) -> Scratch {
        let n = SCRATCH_N.fetch_add(1, Ordering::SeqCst);
        let s = Scratch {
            container: ep.container(),
            name: format!("rsmb-test-{}-{n}", std::process::id()),
        };
        docker_sh(
            s.container,
            &format!("mkdir -p '{0}' && chown tester:tester '{0}'", s.server()),
        );
        s
    }
    /// The path in the share: `/upload/rsmb-test-…`.
    pub fn remote(&self) -> String {
        format!("/upload/{}", self.name)
    }
    /// The path in the container: `/srv/share/upload/rsmb-test-…`.
    pub fn server(&self) -> String {
        format!("/srv/share/upload/{}", self.name)
    }
    /// `sh -c` in the container, in `server()`, as tester.
    pub fn sh(&self, script: &str) -> String {
        docker_sh(
            self.container,
            &format!(
                "cd '{}' && su -s /bin/sh tester -c {}",
                self.server(),
                sq(script)
            ),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["exec", self.container, "rm", "-rf", &self.server()])
            .output();
    }
}

/// Single-quoted for sh.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn server_sha(container: &str, path: &str) -> String {
    let o = docker_sh(container, &format!("sha256sum {}", sq(path)));
    o.split_whitespace().next().unwrap_or_default().to_string()
}

pub fn local_sha(path: &Path) -> String {
    let o = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(o.status.success(), "sha256sum {}", path.display());
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

pub type Tree = (BTreeMap<Vec<u8>, String>, BTreeSet<Vec<u8>>);

const TREE_SCRIPT: &str = "export LC_ALL=C; find . -type f -print0 | sort -z | xargs -0 -r \
    sha256sum; echo '--- dirs'; find . -type d | sort";

fn parse_tree(out: &[u8]) -> Tree {
    let text = out;
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    let mut in_dirs = false;
    for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        if line == b"--- dirs" {
            in_dirs = true;
        } else if in_dirs {
            dirs.insert(line.to_vec());
        } else {
            let i = line.windows(2).position(|w| w == b"  ").unwrap();
            files.insert(
                line[i + 2..].to_vec(),
                String::from_utf8_lossy(&line[..i]).into_owned(),
            );
        }
    }
    (files, dirs)
}

/// Relative path → sha256 for every regular file below `dir` in the container, and the
/// directories (`.` included). The same pipeline as `local_tree`.
pub fn server_tree(container: &str, dir: &str) -> Tree {
    let o = Command::new("docker")
        .args([
            "exec",
            container,
            "sh",
            "-c",
            &format!("cd {} && {TREE_SCRIPT}", sq(dir)),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "server_tree {dir}");
    parse_tree(&o.stdout)
}

pub fn local_tree(dir: &Path) -> Tree {
    let o = Command::new("sh")
        .args([
            "-c",
            &format!("cd {} && {TREE_SCRIPT}", sq(&dir.to_string_lossy())),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "local_tree {}", dir.display());
    parse_tree(&o.stdout)
}

pub fn temps_on_server(container: &str, dir: &str) -> Vec<String> {
    docker_sh(container, &format!("find {} -name '.rsmb-*.part'", sq(dir)))
        .lines()
        .map(String::from)
        .collect()
}

/// The temporaries found on the server and the ones the error's `left_behind` names are the same
/// files (compared by name, which is the hash of the final one).
pub fn assert_named(what: &str, temps: &[String], e: &Value) {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let mut have: Vec<String> = temps.iter().map(|t| base(t)).collect();
    let mut named: Vec<String> = e["left_behind"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| base(v.as_str().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    have.sort();
    named.sort();
    assert_eq!(
        have, named,
        "{what}: every temp on the server is named and only those: {temps:?} {e}"
    );
}

pub fn temps_local(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            } else if e.file_name().to_string_lossy().starts_with(".rsmb-") {
                v.push(p);
            }
        }
    }
    v
}

/// rsmb with that working directory.
pub fn run_in(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let t0 = Instant::now();
    let mut cmd = Command::new(BIN);
    cmd.current_dir(dir)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RSMB_HISTORY", "");
    for (k, v) in env {
        cmd.env(k, v);
    }
    finish_run(cmd.spawn().unwrap(), stdin, &format!("{args:?}"), t0)
}

fn finish_run(mut child: Child, stdin: Option<&str>, what: &str, t0: Instant) -> Output {
    if let Some(input) = stdin {
        let mut w = child.stdin.take().unwrap();
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(input.as_bytes());
        });
    }
    let out = collect(child.stdout.take().unwrap());
    let err = collect(child.stderr.take().unwrap());
    let code = wait_child(&mut child, WAIT);
    std::thread::sleep(Duration::from_millis(20));
    let (stdout, stderr) = (out.lock().unwrap().clone(), err.lock().unwrap().clone());
    let Some(code) = code else {
        panic!("{what} did not end within 60 s\nstdout: {stdout}\nstderr: {stderr}");
    };
    Output {
        code,
        stdout,
        stderr,
        took: t0.elapsed(),
    }
}

/// `bash -c <script>`, for a child with a file size limit (bash counts 1,024-byte blocks).
pub fn run_sh(script: &str) -> Output {
    let t0 = Instant::now();
    let mut cmd = Command::new("bash");
    cmd.args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RSMB_HISTORY", "");
    finish_run(cmd.spawn().unwrap(), None, script, t0)
}

/// Until a local file has grown that far.
pub fn wait_file(path: &Path, at_least: u64, limit: Duration) {
    let deadline = Instant::now() + limit;
    while std::fs::metadata(path).map_or(0, |m| m.len()) < at_least {
        assert!(
            Instant::now() < deadline,
            "{} did not reach {at_least} bytes within {limit:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The same in the container (`stat`).
pub fn wait_server_file(container: &str, path: &str, at_least: u64, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        let o = Command::new("docker")
            .args(["exec", container, "stat", "-c", "%s", path])
            .output()
            .unwrap();
        let n: u64 = String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
        if n >= at_least {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{path} in {container} did not reach {at_least} bytes within {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Until `smbstatus -L` lists no open of that name.
pub fn wait_no_open(container: &str, name: &str, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        let o = docker_sh(container, "smbstatus -L 2>&1 || true");
        if !o.contains(name) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} still open in {container} after {limit:?}:\n{o}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The temporary name, computed by the test's own FNV-1a.
pub fn fnv_temp(name: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!(".rsmb-{h:016x}.part")
}

/// A local file of `len` bytes of a reproducible pattern.
pub fn make_file(path: &Path, len: u64, seed: u8) {
    let mut f = std::fs::File::create(path).unwrap();
    let block: Vec<u8> = (0..65_536u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 ^ seed)
        .collect();
    let mut left = len;
    while left > 0 {
        let n = left.min(block.len() as u64) as usize;
        f.write_all(&block[..n]).unwrap();
        left -= n as u64;
    }
}

/// The `transfer` objects of a reply.
pub fn transfers(r: &Reply) -> Vec<&Value> {
    r.of("transfer")
}

/// The `error` objects of a reply that carry a `path` (items of a recursive transfer).
pub fn item_errors(r: &Reply) -> Vec<&Value> {
    r.lines
        .iter()
        .filter(|v| v["type"] == "error" && v.get("path").is_some())
        .collect()
}

/// Removes a local file or tree when dropped (the multi-gigabyte files).
pub struct Remove(pub PathBuf);
impl Drop for Remove {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A client with `use share`, `lcd <dir>` and, if given, `cd <remote dir>` done.
pub fn ready(mut c: Client, dir: &Path, cd: Option<&str>) -> Client {
    c.cmd("use share").expect_ok("use share");
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd");
    if let Some(d) = cd {
        c.cmd(&format!("cd {}", q(d))).expect_ok("cd scratch");
    }
    if c.tap.is_some() {
        take(c.tap());
    }
    c
}

/// Over the logs of one whole connection (start-up included, in order), the client never spent a
/// credit the server had not granted: from the connection's first credit, granted minus charged
/// never goes below 0. Returns the lowest balance and the most requests outstanding at once.
pub fn credit_balance(parts: &[&Stats]) -> (i64, i64) {
    let mut ev: Vec<(Duration, i64, i64)> = Vec::new();
    for s in parts {
        for q in &s.request_log {
            ev.push((q.at, -i64::from(q.credit_charge.max(1)), 1));
        }
        for r in &s.response_log {
            ev.push((
                r.at,
                i64::from(r.credits_granted),
                if r.interim { 0 } else { -1 },
            ));
        }
    }
    // A response before a request at the same instant: the tap records the cause first.
    ev.sort_by_key(|e| (e.0, -e.2));
    let (mut bal, mut low, mut out, mut peak) = (1i64, 1i64, 0i64, 0i64);
    for (_, credits, o) in ev {
        bal += credits;
        out += o;
        low = low.min(bal);
        peak = peak.max(out);
    }
    (low, peak)
}

// ---------------------------------------------------------------------------------------------
// Stage 3: walks (docs/plans/stage-3.md 6.1)

/// The server's own `find` below /srv/share, run as tester so that it sees what the SMB user
/// sees: `docker exec -u tester -w /srv/share <container> find <dir> -mindepth 1 <args…>
/// -print0`, as an argument vector (no shell). Stderr and the exit status are ignored (a
/// directory it may not read). Paths relative to the share root (`./x` becomes `x`).
pub fn server_find(container: &str, dir: &str, args: &[&str]) -> BTreeSet<Vec<u8>> {
    let o = Command::new("docker")
        .args([
            "exec",
            "-u",
            "tester",
            "-w",
            "/srv/share",
            container,
            "find",
            dir,
        ])
        .args(["-mindepth", "1"])
        .args(args)
        .arg("-print0")
        .output()
        .unwrap();
    o.stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| p.strip_prefix(b"./").unwrap_or(p).to_vec())
        .collect()
}

pub fn skipped(r: &Reply) -> Vec<&Value> {
    r.of("skipped")
}

pub fn denied(r: &Reply) -> Vec<&Value> {
    r.of("denied")
}

pub fn walk_summary(r: &Reply) -> Option<&Value> {
    r.lines.iter().find(|v| v["type"] == "walk_summary")
}

/// Panics with the first differences unless the two sets of paths are equal.
pub fn same_paths(what: &str, got: &BTreeSet<Vec<u8>>, want: &BTreeSet<Vec<u8>>) {
    if got != want {
        let show = |a: &BTreeSet<Vec<u8>>, b: &BTreeSet<Vec<u8>>| -> Vec<String> {
            a.difference(b)
                .take(5)
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .collect()
        };
        panic!(
            "{what}: {} paths, the server has {}; missing {:?}, extra {:?}",
            got.len(),
            want.len(),
            show(want, got),
            show(got, want)
        );
    }
}

pub struct Piped {
    /// rsmb's exit code.
    pub code: i32,
    /// What the filter printed.
    pub out: String,
    pub stderr: String,
    pub took: Duration,
    pub stats: Stats,
}

/// A tap in front of `ep`, then `bash -c '<rsmb through the tap> -C "use share" -C <line> |
/// <filter>; exit ${PIPESTATUS[0]}'`: rsmb's exit code, the filter's output, the time, the tap's
/// stats.
pub fn piped(ep: Ep, line: &str, filter: &str) -> Piped {
    let tap = ep.tap();
    let mut argv = vec![BIN.to_string()];
    argv.extend(ep.args(Some(&tap)));
    argv.extend([
        "-C".into(),
        "use share".into(),
        "-C".into(),
        line.to_string(),
    ]);
    let cmd: Vec<String> = argv.iter().map(|a| sq(a)).collect();
    let o = run_sh(&format!(
        "{} | {filter}; exit ${{PIPESTATUS[0]}}",
        cmd.join(" ")
    ));
    let stats = take(&tap);
    Piped {
        code: o.code,
        out: o.stdout,
        stderr: o.stderr,
        took: o.took,
        stats,
    }
}

/// The fixture's medium.bin as a local file (copied out of the container).
pub fn medium_copy(dir: &Path) -> PathBuf {
    let p = dir.join("medium.bin");
    let f = std::fs::File::create(&p).unwrap();
    let ok = Command::new("docker")
        .args(["exec", SAMBA, "cat", "/srv/share/large/medium.bin"])
        .stdout(f)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    p
}
