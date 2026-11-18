//! Slow endpoint, edge, wan-bad, SOCKS (docs/testing.md).

mod common;

use common::*;

#[test]
fn startup_and_use_slow() {
    check_startup(Ep::Slow);
    check_use(Ep::Slow);
}

#[test]
fn cd_slow() {
    check_cd(Ep::Slow);
}

#[test]
fn ls_small_slow() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    let r = check_ls(&mut c, Ep::Slow, "sizes");
    expect_samba_ls(&r, "ls sizes on slow (f = 1)");
}

#[test]
fn ls_many_slow() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    let r = check_ls(&mut c, Ep::Slow, "many");
    assert_eq!(r.of("entry").len(), 10_000);
    expect_samba_ls(&r, "ls many on slow (limit 4 flights)");
}

#[test]
fn ls_huge_slow() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    let r = check_ls(&mut c, Ep::Slow, "huge");
    assert_eq!(r.of("entry").len(), 100_000);
    expect_samba_ls(&r, "ls huge on slow (limit 7 flights)");
    let kb = c.peak_rss_kb();
    println!("ls huge: peak RSS {kb} kB, {:?}", r.took);
    assert!(
        kb <= 150_000,
        "ls huge: peak RSS {kb} kB, expected at most 150 MB"
    );
}

#[test]
fn edge_navigation() {
    let mut c = Client::tapped(Ep::Edge);
    c.cmd("use share").expect_ok("use share");
    let r = c.cmd("cd many");
    r.expect_ok("cd many");
    r.expect("cd many on edge", 2, 1);
    let r = c.cmd("ls");
    r.expect_ok("ls many on edge");
    assert_eq!(
        r.names(),
        server_names("rsmb-samba-edge", "many"),
        "ls many on edge"
    );
    let s = r.stats();
    assert!(
        s.flights <= 4 && s.requests <= 44,
        "ls many on edge: expected at most 4 flights and 44 requests, got {} in {}\n{}",
        s.requests,
        s.flights,
        log(s)
    );
    assert!(
        r.took < std::time::Duration::from_secs(30),
        "ls many on edge took {:?}",
        r.took
    );
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls sizes on edge");
    r.expect("ls sizes on edge (CREATE, 13 queries, probe, CLOSE)", 16, 1);
}

fn interrupt_with(signal: &str) {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    c.send("ls huge");
    c.wait_requests("QUERY_DIRECTORY", 1, std::time::Duration::from_secs(10));
    let t0 = std::time::Instant::now();
    let pid = c.pid().to_string();
    assert!(
        std::process::Command::new("kill")
            .args([signal, &pid])
            .status()
            .unwrap()
            .success()
    );
    let (code, rest) = c.wait_exit(NO_HANG);
    let took = t0.elapsed();
    assert_eq!(code, 130, "{signal}: exit code; {rest:?}");
    assert!(took < NO_HANG, "{signal}: took {took:?}");
    let e = rest
        .iter()
        .find(|v| v["type"] == "error")
        .unwrap_or_else(|| panic!("{signal}: no error: {rest:?}"));
    assert_eq!(e["kind"], "interrupted", "{signal}");
}

#[test]
fn interrupt_non_interactive() {
    interrupt_with("-INT");
    interrupt_with("-TERM");
}

/// An interactive run whose start-up lines are still running: SIGTERM ends it with 130 (no prompt
/// is ever shown).
#[test]
fn sigterm_during_startup_lines() {
    let dir = tmp("sigterm_during_startup_lines");
    let tap = smbtap::Tap::start(Ep::Slow.addr()).unwrap();
    let port = tap.addr().port().to_string();
    let mut args = vec!["-H", "127.0.0.1", "-P", port.as_str()];
    args.extend(LOGIN);
    args.extend(["-C", "use share", "-C", "ls huge"]);
    let p = Pty::start(&args, &dir.join("history"));
    wait_requests(&tap, "QUERY_DIRECTORY", 1, WAIT);
    let pid = p.rsmb_pid().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let out = p.transcript();
    assert_eq!(p.exit_code(NO_HANG), 130, "{out}");
}

#[test]
fn cut_during_a_listing() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    c.send("ls huge");
    c.wait_requests("QUERY_DIRECTORY", 1, std::time::Duration::from_secs(10));
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    r.expect_ok("ls huge across a cut");
    assert_eq!(r.of("entry").len(), 100_000);
    let s = r.stats();
    assert_eq!(
        s.connections,
        1,
        "expected exactly 1 new connection\n{}",
        log(s)
    );
    let conns: std::collections::BTreeSet<u64> = s.request_log.iter().map(|q| q.conn).collect();
    assert_eq!(
        conns.len(),
        2,
        "expected requests on two connections\n{}",
        log(s)
    );
    for conn in conns {
        let on = |cmd: &str| {
            s.request_log
                .iter()
                .filter(|q| q.conn == conn && q.command == cmd)
                .count()
        };
        assert!(
            on("CREATE") == 1 && on("QUERY_DIRECTORY") >= 1,
            "expected the listing's CREATE and QUERY_DIRECTORY in the request log on the first \
             connection and again on the second\n{}",
            log(s)
        );
    }
}

#[test]
fn shares_edge() {
    let mut c = Client::tapped(Ep::Edge);
    let r = c.cmd_within("shares", NO_HANG);
    r.expect_ok("shares on edge");
    let s = r.stats();
    // The access column on 16 credits: 2 requests per share, one connection, no stall.
    assert_eq!(s.connections, 0, "shares on edge: one connection");
    assert_eq!(
        (count(s, "TREE_CONNECT"), count(s, "TREE_DISCONNECT")),
        (1 + 306, 306),
        "shares on edge: IPC$ and one probe per share\n{}",
        context(s)
    );
    println!("edge shares: {:?}, {} flights", r.took, s.flights);
    // Never a credit not granted. Checked anonymously: `encrypted` then refuses in plain text,
    // whereas for tester Samba encrypts the answer to that compound and the tap cannot read the
    // credits it grants (docs/measuring.md).
    let mut anon = Client::with(None, Some(Ep::Edge), &[]);
    let a = anon.cmd_within("shares", NO_HANG);
    a.expect_ok("shares on edge as anonymous");
    let (low, peak) = credit_balance(&[anon.startup.stats(), a.stats()]);
    println!(
        "edge shares as anonymous: {:?}, peak outstanding {peak}",
        a.took
    );
    assert!(
        low >= 0 && a.stats().opaque_responses == 0,
        "shares on edge spent a credit it was not granted ({low})\n{}",
        context(a.stats())
    );
    assert!(a.of("share").iter().any(|v| v["access"] == "none"));
    let shares = r.of("share");
    for v in &shares {
        let rw = ["share", "encrypted", "tiny"].contains(&v["name"].as_str().unwrap());
        assert_eq!(
            v["access"],
            if rw { "rw" } else { "r" },
            "access of {}",
            v["name"]
        );
    }
    assert_eq!(shares.len(), 306, "shares on edge");
    let name = |v: &&serde_json::Value| v["name"].as_str().unwrap().to_string();
    assert_eq!(
        shares
            .iter()
            .filter(|v| name(v).starts_with("edge"))
            .count(),
        300
    );
    assert_eq!(shares.iter().filter(|v| v["hidden"] == true).count(), 31);
    let ipc = shares.iter().find(|v| v["name"] == "IPC$").unwrap();
    assert_eq!(ipc["share_type"], "ipc");
    assert!(
        shares
            .iter()
            .any(|v| !v["comment"].as_str().unwrap().is_empty()),
        "no comment"
    );
}

/// A connection lost while the access of the shares is probed (final review, finding 7): the list
/// that was fetched is still printed, access unknown (`null`) for the shares not answered, then
/// the error; the next command reconnects and gets the whole answer.
#[test]
fn shares_lost_during_the_probe() {
    let mut c = Client::tapped(Ep::Slow);
    c.send("shares");
    // IPC$ and the first probe compound; its answers are 200 ms away.
    c.wait_requests("TREE_CONNECT", 2, std::time::Duration::from_secs(10));
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert!(!r.ok, "{:?}", r.lines);
    let list = r.of("share");
    assert!(list.len() >= 5, "the list is printed: {:?}", r.lines);
    assert!(
        list.iter().all(|v| v["access"].is_null()),
        "nothing was answered: {:?}",
        r.lines
    );
    let last = r.lines.last().unwrap();
    assert_eq!(last["type"], "error", "{:?}", r.lines);
    assert_eq!(last["kind"], "connection_lost", "{:?}", r.lines);
    assert_eq!(r.of("error").len(), 1, "no second try: {:?}", r.lines);
    let again = c.cmd("shares");
    again.expect_ok("shares after the loss");
    assert_eq!(again.stats().connections, 1, "{}", log(again.stats()));
    assert!(
        again.of("share").iter().all(|v| !v["access"].is_null()),
        "{:?}",
        again.lines
    );
}

// ---------------------------------------------------------------------------------------------
// pty

/// SIGTERM while a Tab's request is on the wire (the share list at the slow endpoint takes about
/// 1.4 s, the tap has seen its first request): exit 130, the terminal cooked again, the history
/// kept (final review, finding 1; `sigterm_in_the_shell` has the prompt).
#[test]
fn sigterm_in_a_tab_on_the_wire() {
    let dir = tmp("sigterm_in_a_tab");
    let hist = dir.join("history");
    let tap = Ep::Slow.tap();
    let route = Ep::Slow.args(Some(&tap)).join(" ");
    let script = format!("{BIN} {route}; echo EXIT=$?; stty -a; echo END");
    let mut p = Pty::command("sh", &["-c", &script], &hist, &[]);
    let prompt = format!("//{}> ", Ep::Slow.host(true));
    p.expect(&prompt, WAIT);
    p.line("pwd", &prompt);
    p.send(b"use sh\t");
    wait_requests(&tap, "TREE_CONNECT", 1, Duration::from_secs(10));
    let sh = p.rsmb_pid().to_string();
    let o = Command::new("pgrep").args(["-P", &sh]).output().unwrap();
    let pid = String::from_utf8(o.stdout).unwrap().trim().to_string();
    let sent = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(sent.success());
    p.expect("END", WAIT);
    let t = p.transcript();
    assert!(t.contains("EXIT=130"), "exit code; transcript:\n{t}");
    assert!(
        !t.contains("use share "),
        "the Tab had finished before the signal; transcript:\n{t}"
    );
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ';').collect();
    for w in ["icanon", "echo"] {
        assert!(
            words.contains(&w) && !words.contains(&format!("-{w}").as_str()),
            "the terminal is not back to {w}; transcript:\n{t}"
        );
    }
    let h = std::fs::read_to_string(&hist).unwrap_or_default();
    assert!(h.lines().any(|l| l == "pwd"), "history {h:?}");
}

#[test]
fn completion_costs_slow() {
    let dir = tmp("completion_costs_slow");
    let (mut p, tap, _) = Pty::tapped(Ep::Slow, &[], &dir.join("history"));
    p.line("use share", "//127.0.0.1/share/> ");
    take(&tap);
    check_tab_many(&mut p, &tap, Ep::Slow);
    let s = tab(&mut p, &tap, "ls huge/f1", Some("ls huge/f100000 "));
    expect_stats(&s, "first Tab in huge/ on slow", 5, 1);
    expect_stats(
        &tab(&mut p, &tap, "ls huge/f09999", None),
        "second Tab in huge/ on slow",
        0,
        0,
    );
    p.send(CTRL_U);
    p.line("ls tree", "//127.0.0.1/share/> ");
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "cd tree/d", None),
        "Tab after ls on slow",
        0,
        0,
    );
}

#[test]
fn edge_completion() {
    let dir = tmp("edge_completion");
    let (mut p, tap, _) = Pty::tapped(Ep::Edge, &[], &dir.join("history"));
    p.line("use share", "//127.0.0.1/share/> ");
    take(&tap);
    check_tab_many(&mut p, &tap, Ep::Edge);
    p.send(CTRL_U);
    p.line("shares", "//127.0.0.1/share/> ");
    take(&tap);
    let s = tab(&mut p, &tap, "use edge300", Some("use edge300$ "));
    expect_stats(&s, "Tab after shares on edge", 0, 0);
}

fn interrupt_in_the_shell(ep: Ep) {
    let dir = tmp(&format!("interrupt_interactive_{ep:?}"));
    let (mut p, tap, _) = Pty::tapped(ep, &[], &dir.join("history"));
    let prompt = "//127.0.0.1/share/> ";
    p.line("use share", prompt);
    take(&tap);
    p.send(b"ls huge\r");
    wait_requests(&tap, "QUERY_DIRECTORY", 1, WAIT);
    p.send(b"\x03");
    p.expect("interrupted", NO_HANG);
    p.expect(prompt, NO_HANG);
    take(&tap);
    let t0 = std::time::Instant::now();
    p.send(b"ls sizes\r");
    p.expect("  8388609.bin\r\n", WAIT);
    let took = t0.elapsed();
    p.expect(prompt, WAIT);
    assert!(
        took < NO_HANG,
        "ls sizes after the interrupt on {ep:?} took {took:?}"
    );
    p.line("cd tree", "//127.0.0.1/share/tree> ");
    p.line("cd /", prompt);
    let s = take(&tap);
    assert_eq!(
        s.connections,
        1,
        "expected exactly 1 new connection after the interrupt on {ep:?}\n{}",
        log(&s)
    );
    p.send(b"pwd\r");
    p.expect("//127.0.0.1/share/\r\n", WAIT);
}

#[test]
fn interrupt_interactive() {
    interrupt_in_the_shell(Ep::Slow);
    interrupt_in_the_shell(Ep::Edge);
}

#[test]
fn wan_bad_smoke() {
    let mut c = Client::direct("10.213.45.31:445");
    let startup = c.startup.took;
    let mut times = vec![("start-up", startup)];
    for line in ["use share", "cd tree", "ls", "ls /many"] {
        let r = c.cmd(line);
        r.expect_ok(&format!("{line} on wan-bad"));
        if line == "ls /many" {
            assert_eq!(r.of("entry").len(), 10_000);
        }
        if line == "ls" {
            assert_eq!(r.names(), server_names("rsmb-samba", "tree"));
        }
        times.push((line, r.took));
    }
    println!("wan-bad: {times:?}");
}

// ---------------------------------------------------------------------------------------------
// Stage 2: transfers (docs/plans/stage-2.md 6.4)

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// What `tests/slow.rs` asserts about time: that a command which should end promptly ends, not
/// that it ends in a given time (its tests run side by side on one link; precise bounds are in
/// `tests/serial.rs`, `docs/testing.md`). At least five times what these take alone.
const NO_HANG: Duration = Duration::from_secs(10);

fn per_conn(s: &smbtap::Stats, conn: u64) -> std::collections::BTreeMap<&'static str, u64> {
    let mut m = std::collections::BTreeMap::new();
    for q in s.request_log.iter().filter(|q| q.conn == conn) {
        *m.entry(q.command).or_default() += 1;
    }
    m
}

fn assert_nothing_left(
    s: &Scratch,
    dir: &Path,
    final_local: Option<&Path>,
    final_remote: Option<&str>,
) {
    if let Some(f) = final_local {
        assert!(!f.exists(), "nothing under the final name: {}", f.display());
    }
    if let Some(f) = final_remote {
        let o = docker_sh(
            s.container,
            &format!(
                "test -e {} && echo yes || echo no",
                sq(&format!("{}/{f}", s.server()))
            ),
        );
        assert_eq!(
            o.trim(),
            "no",
            "nothing under the final name on the server: {f}"
        );
    }
    assert_eq!(temps_local(dir), Vec::<PathBuf>::new(), "no local temp");
    assert_eq!(
        temps_on_server(s.container, &s.server()),
        Vec::<String>::new(),
        "no temp on the server"
    );
}

/// S21
#[test]
fn big_file() {
    let dir = tmp("big_file");
    let _rm = Remove(dir.join("big.bin"));
    let s = Scratch::new(Ep::Fast);
    let mut c = ready(Client::tapped(Ep::Fast), &dir, Some(&s.remote()));
    let want = "ad2f1026d65a08bc81d0198783ffd36c104bf57a6c379a650794c3dd7e14de9a";
    let r = c.cmd("get /large/big.bin");
    r.expect_ok("get big.bin");
    assert_eq!(transfers(&r)[0]["bytes"], 1_181_116_007u64);
    assert!(
        r.stats().peak_outstanding <= 16,
        "never more than 16 requests outstanding in the tap: {}",
        r.stats().peak_outstanding
    );
    assert_eq!(r.stats().connections, 0, "get: one connection");
    let kb = c.peak_rss_kb();
    println!(
        "get big.bin: {:?}, peak RSS {kb} kB, peak outstanding {}",
        r.took,
        r.stats().peak_outstanding
    );
    assert!(kb <= 64_000, "peak RSS ≤ 64 MB after get: {kb} kB");
    assert_eq!(local_sha(&dir.join("big.bin")), want);
    let r = c.cmd("put big.bin");
    r.expect_ok("put big.bin");
    assert_eq!(transfers(&r)[0]["bytes"], 1_181_116_007u64);
    assert!(
        r.stats().peak_outstanding <= 16,
        "never more than 16 requests outstanding in the tap: {}",
        r.stats().peak_outstanding
    );
    assert_eq!(r.stats().connections, 0, "put: one connection");
    let kb = c.peak_rss_kb();
    println!(
        "put big.bin: {:?}, peak RSS {kb} kB, peak outstanding {}",
        r.took,
        r.stats().peak_outstanding
    );
    assert!(kb <= 64_000, "peak RSS ≤ 64 MB after put: {kb} kB");
    assert_eq!(server_sha(SAMBA, &format!("{}/big.bin", s.server())), want);
    assert_eq!(c.startup.stats().connections, 1, "one connection");
}

/// S22: the one test longer than 30 s (4.3 GiB down and up, two checksums over it).
#[test]
fn over_4g() {
    let dir = tmp("over_4g");
    let _rm = Remove(dir.join("over4g.bin"));
    let s = Scratch::new(Ep::Fast);
    let mut c = ready(Client::direct(FAST), &dir, Some(&s.remote()));
    let want = "d0b7cf9be6a8e93f2198475dd8a6a728d536f3bcb1489f545aa2f39cf4ad4d85";
    let r = c.cmd_within("get /large/over4g.bin", Duration::from_secs(120));
    r.expect_ok("get over4g.bin");
    assert_eq!(transfers(&r)[0]["bytes"], 4_617_089_843u64);
    let r2 = c.cmd_within("put over4g.bin", Duration::from_secs(120));
    r2.expect_ok("put over4g.bin");
    assert_eq!(transfers(&r2)[0]["bytes"], 4_617_089_843u64);
    println!("over4g.bin: get {:?}, put {:?}", r.took, r2.took);
    let remote = std::thread::spawn({
        let p = format!("{}/over4g.bin", s.server());
        move || server_sha(SAMBA, &p)
    });
    assert_eq!(local_sha(&dir.join("over4g.bin")), want);
    assert_eq!(remote.join().unwrap(), want);
    assert_nothing_left(&s, &dir, None, None);
}

/// S23
#[test]
fn two_thousand_small_files() {
    let dir = tmp("two_thousand_small_files");
    let s = Scratch::new(Ep::Fast);
    let mut c = ready(Client::tapped(Ep::Fast), &dir, Some(&s.remote()));
    let fixture = server_tree(SAMBA, "/srv/share/smallfiles");
    assert_eq!((fixture.0.len(), fixture.1.len()), (2000, 21));
    let r = c.cmd("get -r /smallfiles");
    r.expect_ok("get -r /smallfiles");
    let st = r.stats();
    assert_eq!(
        st.requests, 6084,
        "expected 21 listings and 2,000 small reads: 6,084 requests\n{:?}",
        st.per_command
    );
    assert!(
        st.peak_outstanding <= 64 && st.peak_outstanding > 3,
        "peak outstanding down {}",
        st.peak_outstanding
    );
    let kb = c.peak_rss_kb();
    println!(
        "get -r /smallfiles: {:?}, peak outstanding {}, peak RSS {kb} kB",
        r.took, st.peak_outstanding
    );
    assert_eq!(st.connections, 0, "get: one connection");
    assert_eq!(local_tree(&dir.join("smallfiles")), fixture);
    let r = c.cmd("put -r smallfiles");
    r.expect_ok("put -r smallfiles");
    let st = r.stats();
    assert_eq!(st.connections, 0, "put: one connection");
    assert_eq!(
        st.requests, 14042,
        "expected 21 mkdir and 2,000 small writes: 14,042\n{:?}",
        st.per_command
    );
    assert!(
        st.peak_outstanding <= 48 && st.peak_outstanding > 3,
        "peak outstanding up {}",
        st.peak_outstanding
    );
    let kb = c.peak_rss_kb();
    println!(
        "put -r smallfiles: {:?}, peak outstanding {}, peak RSS {kb} kB",
        r.took, st.peak_outstanding
    );
    assert!(kb <= 64_000, "peak RSS {kb} kB");
    assert_eq!(
        server_tree(SAMBA, &format!("{}/smallfiles", s.server())),
        fixture
    );
}

/// S24
#[test]
fn small_files_counts_slow() {
    let dir = tmp("small_files_counts_slow");
    let s = Scratch::new(Ep::Slow);
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    let r = c.cmd("get -r /smallfiles/d01");
    r.expect_ok("get -r d01");
    let st = r.stats();
    assert_eq!(
        st.requests, 304,
        "expected 304 requests (1 listing, 100 files × 3)\n{:?}",
        st.per_command
    );
    assert!(
        (6..=48).contains(&st.peak_outstanding),
        "peak outstanding {}",
        st.peak_outstanding
    );
    assert_eq!(st.connections, 0, "get: one connection");
    assert_eq!(
        local_tree(&dir.join("d01")),
        server_tree(SAMBA, "/srv/share/smallfiles/d01")
    );
    let r = c.cmd("put -r d01");
    r.expect_ok("put -r d01");
    let st = r.stats();
    assert_eq!(
        st.requests, 702,
        "expected 702 (1 mkdir, 100 × 7)\n{:?}",
        st.per_command
    );
    assert!(
        (6..=48).contains(&st.peak_outstanding),
        "peak outstanding {}",
        st.peak_outstanding
    );
    assert_eq!(st.connections, 0, "put: one connection");
    assert_eq!(
        server_tree(SAMBA, &format!("{}/d01", s.server())),
        local_tree(&dir.join("d01"))
    );
    assert_eq!(c.startup.stats().connections, 1);
    let (code, _) = c.finish();
    assert_eq!(code, 0);
}

/// S25
#[test]
fn cut_mid_get() {
    let dir = tmp("cut_mid_get");
    let s = Scratch::new(Ep::Slow);
    let mut c = ready(Client::tapped(Ep::Slow), &dir, None);
    c.send("get /large/medium.bin");
    c.wait_requests("READ", 8, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert!(!r.ok);
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    assert_eq!(
        r.stats().connections,
        0,
        "expected 0 new connections for the failed get"
    );
    assert_nothing_left(&s, &dir, Some(&dir.join("medium.bin")), None);
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls after the cut");
    assert_eq!(
        r.stats().connections,
        1,
        "exactly 1 new connection\n{}",
        log(r.stats())
    );
    assert_eq!(
        count(r.stats(), "READ"),
        0,
        "no READ on the second connection"
    );
}

/// S26
#[test]
fn cut_mid_put() {
    let dir = tmp("cut_mid_put");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    for (round, line) in ["put medium.bin", "put -f medium.bin"]
        .into_iter()
        .enumerate()
    {
        let old = (round == 1).then(|| {
            s.sh("echo old > medium.bin");
            server_sha(SAMBA, &format!("{}/medium.bin", s.server()))
        });
        c.send(line);
        c.wait_requests("WRITE", 8, WAIT);
        c.tap().drop_connections();
        let r = c.wait_done(WAIT);
        assert_eq!(
            r.kind().as_deref(),
            Some("connection_lost"),
            "{line}: {:?}",
            r.lines
        );
        let st = r.stats();
        assert_eq!(
            st.connections,
            1,
            "{line}: expected exactly 1 new connection\n{}",
            log(st)
        );
        let second = st.request_log.iter().map(|q| q.conn).max().unwrap();
        let want: std::collections::BTreeMap<&str, u64> = [
            ("NEGOTIATE", 1),
            ("SESSION_SETUP", 2),
            ("TREE_CONNECT", 1),
            ("CREATE", 1),
            ("SET_INFO", 1),
            ("CLOSE", 1),
        ]
        .into();
        assert_eq!(
            per_conn(st, second),
            want,
            "{line}: expected exactly 1 new connection, carrying no WRITE and no CREATE of the target: NEGOTIATE 1, SESSION_SETUP 2, TREE_CONNECT 1, CREATE 1, SET_INFO 1, CLOSE 1\n{}",
            log(st)
        );
        assert_eq!(
            temps_on_server(s.container, &s.server()),
            Vec::<String>::new(),
            "{line}: no temp"
        );
        match &old {
            None => assert_nothing_left(&s, &dir, None, Some("medium.bin")),
            Some(o) => assert_eq!(
                &server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
                o,
                "the old target is unchanged"
            ),
        }
        let r = c.cmd("pwd");
        r.expect_ok("pwd");
        let r = c.cmd("ls");
        r.expect_ok("the next command");
        assert_eq!(
            r.stats().connections,
            0,
            "{line}: the next command 0 new connections"
        );
    }
}

/// S30
#[test]
fn kill_9_leaves_only_temporaries() {
    let dir = tmp("kill_9");
    let s = Scratch::new(Ep::Slow);
    std::fs::rename(medium_copy(&dir), dir.join("up.bin")).unwrap();
    // get
    let mut c = ready(Client::tapped(Ep::Slow), &dir, None);
    c.send("get /large/medium.bin");
    c.wait_requests("READ", 8, WAIT);
    c.kill("KILL");
    drop(c);
    assert!(
        !dir.join("medium.bin").exists(),
        "nothing under the final name"
    );
    let left = temps_local(&dir);
    assert_eq!(
        left,
        [dir.join(fnv_temp("medium.bin"))],
        "exactly one temporary file, the one temp_name gives"
    );
    // put
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    c.send("put up.bin");
    c.wait_requests("WRITE", 8, WAIT);
    c.kill("KILL");
    drop(c);
    let left = temps_on_server(s.container, &s.server());
    assert_eq!(left, [format!("{}/{}", s.server(), fnv_temp("up.bin"))]);
    assert_eq!(
        docker_sh(
            s.container,
            &format!("test -e {}/up.bin && echo yes || echo no", s.server())
        )
        .trim(),
        "no"
    );
    wait_no_open(s.container, &fnv_temp("up.bin"), Duration::from_secs(60));
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    c.cmd("get /large/medium.bin").expect_ok("get again");
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
    c.cmd("put up.bin").expect_ok("put again");
    assert_eq!(
        server_sha(SAMBA, &format!("{}/up.bin", s.server())),
        local_sha(&dir.join("up.bin"))
    );
    assert_nothing_left(&s, &dir, None, None);
}

/// S31
#[test]
fn cut_during_mkdir() {
    let dir = tmp("cut_during_mkdir");
    let s = Scratch::new(Ep::Slow);
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    c.send("mkdir x");
    c.wait_requests("CREATE", 1, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert_eq!(r.kind().as_deref(), Some("connection_lost"));
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    assert!(m.contains("may or may not"), "{m}");
    assert_eq!(
        count(r.stats(), "CREATE"),
        1,
        "not sent twice\n{}",
        log(r.stats())
    );
    let r = c.cmd("ls");
    r.expect_ok("ls after the cut");
    assert_eq!(r.stats().connections, 1);
}

/// S32
#[test]
fn edge_and_encrypted() {
    let limit = Duration::from_secs(60);
    let dir = tmp("edge_and_encrypted");
    let s = Scratch::new(Ep::Edge);
    let mut c = Client::tapped(Ep::Edge);
    let mut replies = Vec::new();
    for line in [
        "use share".to_string(),
        format!("lcd {}", q(&dir.to_string_lossy())),
        format!("cd {}", s.remote()),
    ] {
        let r = c.cmd(&line);
        r.expect_ok(&line);
        replies.push(r);
    }
    for line in [
        "get /large/medium.bin",
        "put medium.bin",
        "get -r /sizes",
        "get -r /fanout",
    ] {
        let r = c.cmd_within(line, limit);
        r.expect_ok(&format!("{line} on edge"));
        assert_eq!(r.stats().connections, 0, "{line} on edge: one connection");
        println!(
            "edge {line}: {:?}, peak outstanding {}",
            r.took,
            r.stats().peak_outstanding
        );
        replies.push(r);
    }
    let mut parts = vec![c.startup.stats()];
    parts.extend(replies.iter().map(|r| r.stats()));
    // On edge the bound is the server's credits, not a number: Samba with `smb2 max credits = 16`
    // grants a few more while 16 are outstanding (17 to 19 seen), so the test checks that the
    // client never spends a credit it was not granted (docs/plans/stage-2.md, deviation I6).
    let (low, peak) = credit_balance(&parts);
    println!("edge: lowest credit balance {low}, peak outstanding {peak}");
    assert!(
        low >= 0,
        "on edge the client spent a credit it was not granted (balance {low})"
    );
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
    assert_eq!(
        server_sha("rsmb-samba-edge", &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert_eq!(
        local_tree(&dir.join("sizes")),
        server_tree("rsmb-samba-edge", "/srv/share/sizes")
    );
    assert_eq!(
        local_tree(&dir.join("fanout")),
        server_tree("rsmb-samba-edge", "/srv/share/fanout")
    );
    // encrypted, fast endpoint
    let dir = tmp("edge_and_encrypted_enc");
    let s = Scratch::new(Ep::Fast);
    let mut c = Client::tapped(Ep::Fast);
    c.cmd("use encrypted").expect_ok("use encrypted");
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd");
    c.cmd(&format!("cd {}", s.remote())).expect_ok("cd");
    for line in [
        "get /large/medium.bin",
        "put medium.bin",
        "get -r /sizes",
        "put -r sizes",
    ] {
        let r = c.cmd_within(line, limit);
        r.expect_ok(&format!("{line} on encrypted"));
        assert_eq!(r.stats().requests, 0, "{line}: only opaque frames");
        assert!(r.stats().opaque_requests > 0);
    }
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
    assert_eq!(
        server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert_eq!(
        server_tree(SAMBA, &format!("{}/sizes", s.server())),
        server_tree(SAMBA, "/srv/share/sizes")
    );
}

/// S33
#[test]
fn wan_bad_transfer() {
    let dir = tmp("wan_bad_transfer");
    let s = Scratch::new(Ep::Bad);
    let mut c = ready(Client::direct("10.213.45.31:445"), &dir, Some(&s.remote()));
    let r = c.cmd("get /sizes/1048577.bin");
    r.expect_ok("get on wan-bad");
    let r2 = c.cmd("put 1048577.bin");
    r2.expect_ok("put on wan-bad");
    println!("wan-bad 1 MiB: get {:?}, put {:?}", r.took, r2.took);
    let want = server_sha(SAMBA, "/srv/share/sizes/1048577.bin");
    assert_eq!(local_sha(&dir.join("1048577.bin")), want);
    assert_eq!(
        server_sha(SAMBA, &format!("{}/1048577.bin", s.server())),
        want
    );
}

/// S34
#[test]
fn file_shrinks_during_get() {
    let dir = tmp("file_shrinks_during_get");
    let s = Scratch::new(Ep::Slow);
    s.sh("cp /srv/share/large/medium.bin m.bin");
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    c.send("get m.bin");
    wait_file(&dir.join(fnv_temp("m.bin")), 4 << 20, WAIT);
    s.sh("truncate -s 20971520 m.bin");
    let r = c.wait_done(WAIT);
    assert_eq!(r.kind().as_deref(), Some("other"), "{:?}", r.lines);
    assert!(
        r.of("error")[0]["message"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    assert_nothing_left(&s, &dir, Some(&dir.join("m.bin")), None);
    c.cmd("ls").expect_ok("the next command");
}

/// S35
#[test]
fn file_vanishes_during_get_r() {
    let dir = tmp("file_vanishes_during_get_r");
    let s = Scratch::new(Ep::Slow);
    s.sh("cp -r /srv/share/smallfiles/d01 d01");
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    c.send("get -r d01");
    c.wait_requests("READ", 1, WAIT);
    s.sh("rm d01/f100.bin");
    let r = c.wait_done(WAIT);
    assert!(!r.ok);
    let items = item_errors(&r);
    assert_eq!(items.len(), 1, "{:?}", r.lines);
    assert_eq!(items[0]["kind"], "not_found");
    assert_eq!(
        items[0]["path"],
        format!("{}/d01/f100.bin", s.remote()).as_str()
    );
    let (files, _) = local_tree(&dir.join("d01"));
    assert_eq!(files.len(), 99);
    let (want, _) = server_tree(SAMBA, &format!("{}/d01", s.server()));
    assert_eq!(files, want);
}

fn spawn_rsmb(dir: &Path, args: &[String]) -> Child {
    Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .env("RSMB_HISTORY", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn done(child: Child) -> (i32, String) {
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

/// S36
#[test]
fn two_processes_one_target() {
    let dir = tmp("two_processes_one_target");
    let s = Scratch::new(Ep::Slow);
    let args = |line: &str| {
        let mut a = target(SLOW);
        a.extend(["--json", "-C", "use share", "-C"].map(String::from));
        a.push(format!("cd {}", s.remote()));
        a.push("-C".into());
        a.push(line.to_string());
        a
    };
    let first = spawn_rsmb(&dir, &args("get /large/medium.bin"));
    wait_file(&dir.join(fnv_temp("medium.bin")), 4 << 20, WAIT);
    let (code2, out2) = done(spawn_rsmb(&dir, &args("get /large/medium.bin")));
    let mut first = first;
    assert!(
        first.try_wait().unwrap().is_none(),
        "the first is still running"
    );
    assert_eq!(code2, 1, "{out2}");
    assert!(out2.contains("another transfer"), "{out2}");
    let (code1, out1) = done(first);
    assert_eq!(code1, 0, "{out1}");
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
    assert!(temps_local(&dir).is_empty());
    // put, both with -f
    let first = spawn_rsmb(&dir, &args("put -f medium.bin"));
    wait_server_file(
        s.container,
        &format!("{}/{}", s.server(), fnv_temp("medium.bin")),
        4 << 20,
        WAIT,
    );
    let (code2, out2) = done(spawn_rsmb(&dir, &args("put -f medium.bin")));
    assert_eq!(code2, 1, "{out2}");
    assert!(out2.contains("in use"), "{out2}");
    let (code1, out1) = done(first);
    assert_eq!(code1, 0, "{out1}");
    assert_eq!(
        server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert!(temps_on_server(s.container, &s.server()).is_empty());
}

/// S37
#[test]
fn target_appears_during_put() {
    let dir = tmp("target_appears_during_put");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    c.send("put medium.bin");
    wait_server_file(
        s.container,
        &format!("{}/{}", s.server(), fnv_temp("medium.bin")),
        4 << 20,
        WAIT,
    );
    s.sh("echo planted > medium.bin");
    let r = c.wait_done(WAIT);
    assert_eq!(r.kind().as_deref(), Some("already_exists"), "{:?}", r.lines);
    assert_eq!(s.sh("cat medium.bin"), "planted\n");
    assert!(temps_on_server(s.container, &s.server()).is_empty());
    let (code, _) = c.finish();
    assert_eq!(code, 1, "the exit status of the command");
}

/// S38
#[test]
fn target_appears_during_get() {
    let dir = tmp("target_appears_during_get");
    let s = Scratch::new(Ep::Slow);
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    for line in ["get /large/medium.bin", "get -f /large/medium.bin"] {
        let _ = std::fs::remove_file(dir.join("medium.bin"));
        c.send(line);
        wait_file(&dir.join(fnv_temp("medium.bin")), 4 << 20, WAIT);
        std::fs::write(dir.join("medium.bin"), b"planted\n").unwrap();
        let r = c.wait_done(WAIT);
        assert_eq!(
            r.kind().as_deref(),
            Some("already_exists"),
            "{line}: {:?}",
            r.lines
        );
        assert_eq!(
            std::fs::read(dir.join("medium.bin")).unwrap(),
            b"planted\n",
            "{line}"
        );
        assert!(temps_local(&dir).is_empty(), "{line}");
    }
    c.cmd("get -f /large/medium.bin")
        .expect_ok("present from the start: replaced");
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
}

/// S39
#[test]
fn source_changes_during_put() {
    let dir = tmp("source_changes_during_put");
    let s = Scratch::new(Ep::Slow);
    let src = medium_copy(&dir);
    let mut c = ready(Client::tapped(Ep::Slow), &dir, Some(&s.remote()));
    c.send("put medium.bin");
    c.wait_requests("WRITE", 8, WAIT);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&src)
        .unwrap()
        .write_all(&[7u8; 1000])
        .unwrap();
    let r = c.wait_done(WAIT);
    assert_eq!(r.kind().as_deref(), Some("other"), "{:?}", r.lines);
    assert!(
        r.of("error")[0]["message"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    assert_nothing_left(&s, &dir, None, Some("medium.bin"));
    c.cmd("ls").expect_ok("the next command");
}

// ---------------------------------------------------------------------------------------------
// Stage 3: tree and find (docs/plans/stage-3.md 6.4)

/// The `path`s of the `entry` objects, in order.
fn entry_lines(r: &Reply) -> Vec<String> {
    r.of("entry")
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect()
}

/// S41
#[test]
fn walk_counts_slow() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    let want = server_find(SAMBA, "tree", &[]);
    let mut first = None;
    for line in ["find /tree", "tree /tree", "find /tree"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        let s = r.stats();
        assert_eq!(
            s.requests, 1_368,
            "{line}: expected 1,368 requests (342 listings × 4)\n{:?}",
            s.per_command
        );
        assert!(
            (8..=64).contains(&s.peak_outstanding),
            "{line}: peak outstanding {}, expected 8 to 64",
            s.peak_outstanding
        );
        assert_eq!(s.connections, 0, "{line}: 0 new connections");
        same_paths(line, &r.paths(), &want);
        assert_eq!(denied(&r).len(), 1);
        assert_eq!(denied(&r)[0]["path"], "/tree/forbidden");
        println!(
            "{line} at slow: {:?}, peak outstanding {}",
            r.took, s.peak_outstanding
        );
        if line == "find /tree" {
            let lines = entry_lines(&r);
            match &first {
                None => first = Some(lines),
                Some(f) => assert_eq!(
                    &lines, f,
                    "find /tree twice: the same lines in the same order"
                ),
            }
        }
    }
    assert_eq!(c.startup.stats().connections, 1);
}

/// S42
#[test]
fn walk_deep_slow() {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    let r = c.cmd("find /deep");
    r.expect_ok("find /deep");
    r.expect("find /deep: 13 listings one after another", 52, 13);
    assert!(
        r.of("entry").iter().any(|e| e["name"] == "bottom.txt"),
        "bottom.txt found"
    );
    println!("find /deep at slow: {:?}", r.took);
}

/// S43
#[test]
fn walk_edge() {
    let mut c = Client::tapped(Ep::Edge);
    let mut replies = vec![c.cmd("use share")];
    replies[0].expect_ok("use share");
    let r = c.cmd_within("find /fanout", Duration::from_secs(60));
    r.expect_ok("find /fanout on edge");
    same_paths(
        "find /fanout on edge",
        &r.paths(),
        &server_find("rsmb-samba-edge", "fanout", &[]),
    );
    println!(
        "edge find /fanout: {:?}, {} requests, peak outstanding {}",
        r.took,
        r.stats().requests,
        r.stats().peak_outstanding
    );
    replies.push(r);
    let r = c.cmd("find /tree");
    r.expect_ok("find /tree on edge");
    assert_eq!(
        r.stats().requests,
        1_368,
        "find /tree on edge\n{:?}",
        r.stats().per_command
    );
    replies.push(r);
    let r = c.cmd("tree /many");
    r.expect_ok("tree /many on edge");
    assert_eq!(r.of("entry").len(), 10_000);
    replies.push(r);
    let mut parts = vec![c.startup.stats()];
    parts.extend(replies.iter().map(|r| r.stats()));
    let (low, peak) = credit_balance(&parts);
    println!("edge walks: lowest credit balance {low}, peak outstanding {peak}");
    assert!(
        low >= 0,
        "on edge a walk spent a credit it was not granted (balance {low})"
    );
}

/// S44: a cut in the middle of a walk; three sessions.
#[test]
fn cut_mid_walk() {
    let want = server_find(SAMBA, "tree", &[]);
    // tree: run again on one new connection.
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    c.send("tree /tree");
    c.wait_requests("QUERY_DIRECTORY", 100, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    r.expect_ok("tree /tree across a cut");
    same_paths("tree /tree across a cut", &r.paths(), &want);
    assert_eq!(r.of("entry").len(), 682);
    assert!(walk_summary(&r).is_some());
    assert_eq!(
        r.stats().connections,
        1,
        "tree: exactly 1 new connection\n{}",
        log(r.stats())
    );
    // find cut in the middle: it has printed, so it is not run again.
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    c.send("find /tree");
    c.wait_requests("QUERY_DIRECTORY", 100, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert!(!r.ok);
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines.last()
    );
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    assert!(m.contains("incomplete"), "{m}");
    assert!(
        walk_summary(&r).is_none(),
        "no walk_summary after a lost connection"
    );
    let n = r.of("entry").len();
    assert!(n > 0 && n < 682, "{n} entries before the error");
    assert_eq!(
        r.stats().connections,
        0,
        "find: 0 new connections for the failed command"
    );
    let r = c.cmd("find /tree/d0");
    r.expect_ok("find /tree/d0 after the cut");
    assert_eq!(r.of("entry").len(), 169);
    assert_eq!(
        r.stats().connections,
        1,
        "exactly 1 new connection\n{}",
        log(r.stats())
    );
    // find cut at its first listing: nothing printed yet, so it is run again.
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    c.send("find /tree");
    c.wait_requests("QUERY_DIRECTORY", 1, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    r.expect_ok("find /tree cut at its first listing");
    let lines = entry_lines(&r);
    assert_eq!(lines.len(), 682, "each entry once");
    same_paths("find /tree run again", &r.paths(), &want);
    assert!(walk_summary(&r).is_some());
    assert_eq!(
        r.stats().connections,
        1,
        "find: exactly 1 new connection\n{}",
        log(r.stats())
    );
}

/// S46: a reader that goes away stops the walk.
#[test]
fn find_and_a_closed_pipe() {
    let p = piped(Ep::Slow, "find /fanout", "head -n 5");
    assert_eq!(p.out.lines().count(), 5, "{}", p.out);
    assert_ne!(p.code, 0, "rsmb's exit code");
    assert!(p.took < NO_HANG, "took {:?}", p.took);
    // Start-up 3, use share 1, the first listing 4: the rest came after the reader left.
    let after = p.stats.requests.saturating_sub(3 + 1 + 4);
    println!(
        "find /fanout | head -n 5: {:?}, {} requests in all ({after} after the first listing), \
         exit {}",
        p.took, p.stats.requests, p.code
    );
    assert!(
        after <= 200,
        "expected at most 200 requests after the reader left, of the 8,004 the whole walk sends; \
         got {after}\n{:?}",
        p.stats.per_command
    );
    assert!(!p.stderr.contains("panicked"), "{}", p.stderr);
}

/// S45
#[test]
fn walk_wan_bad() {
    let mut c = Client::direct("10.213.45.31:445");
    c.cmd("use share").expect_ok("use share");
    let r = c.cmd("find /tree/d0");
    r.expect_ok("find /tree/d0 on wan-bad");
    assert_eq!(r.of("entry").len(), 169);
    println!("wan-bad find /tree/d0: {:?}", r.took);
}

// ---------------------------------------------------------------------------------------------
// Stage 4: the proxy (docs/plans/stage-4.md 6.4)

const BIG_SHA: &str = "ad2f1026d65a08bc81d0198783ffd36c104bf57a6c379a650794c3dd7e14de9a";

/// The fixture's big.bin as a local file (copied out of the private server's container).
fn big_copy(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    let f = std::fs::File::create(&p).unwrap();
    let ok = Command::new("docker")
        .args([
            "exec",
            Ep::Proxied.container(),
            "cat",
            "/srv/share/large/big.bin",
        ])
        .stdout(f)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    p
}

/// S61: every command through port 1081, with a login at the proxy, on one connection.
#[test]
fn proxy_with_login() {
    let ep = Ep::ProxiedLogin;
    let dir = tmp("proxy_with_login");
    let s = Scratch::new(ep);
    let mut c = Client::tapped(ep);
    let mut conns = c.startup.stats().connections;
    for line in session_script(&dir, &s.remote()) {
        let r = c.cmd(&line);
        r.expect_ok(&line);
        conns += r.stats().connections;
        if line == "find /tree -name file.txt" {
            assert_eq!(r.of("entry").len(), 340, "{line}");
            same_paths(
                &line,
                &r.paths(),
                &server_find(ep.container(), "tree", &["-name", "file.txt"]),
            );
        }
    }
    let (code, _) = c.finish();
    assert_eq!(code, 0);
    let sizes = server_sha(ep.container(), "/srv/share/sizes/65537.bin");
    assert_eq!(local_sha(&dir.join("65537.bin")), sizes, "the download");
    assert_eq!(
        server_sha(ep.container(), &format!("{}/65537.bin", s.server())),
        sizes,
        "the upload"
    );
    assert_eq!(
        server_tree(ep.container(), &format!("{}/d01", s.server())),
        server_tree(ep.container(), "/srv/share/smallfiles/d01"),
        "put -r d01"
    );
    assert_eq!(
        conns, 1,
        "expected 1 TCP connection in the tap for the whole session"
    );
}

/// S62: the 1.1 GiB file both ways through the proxy.
#[test]
fn big_file_through_the_proxy() {
    let ep = Ep::Proxied;
    let dir = tmp("big_file_through_the_proxy");
    let _rm = Remove(dir.join("big.bin"));
    let s = Scratch::new(ep);
    let mut c = ready(Client::tapped(ep), &dir, Some(&s.remote()));
    for line in ["get /large/big.bin", "put big.bin"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        assert_eq!(transfers(&r)[0]["bytes"], 1_181_116_007u64, "{line}");
        assert!(
            r.stats().peak_outstanding <= 16,
            "{line}: never more than 16 requests outstanding in the tap: {}",
            r.stats().peak_outstanding
        );
        assert_eq!(r.stats().connections, 0, "{line}: 0 new connections");
        let kb = c.peak_rss_kb();
        println!("{line} through the proxy: {:?}, peak RSS {kb} kB", r.took);
        assert!(
            kb <= 64_000,
            "{line}: peak RSS {kb} kB, expected at most 64 MB"
        );
    }
    assert_eq!(local_sha(&dir.join("big.bin")), BIG_SHA);
    assert_eq!(
        server_sha(ep.container(), &format!("{}/big.bin", s.server())),
        BIG_SHA
    );
}

/// `--proxy <url> -H <host> [-P <port>]`, the login, `-N`, `use share`, `ls /sizes`, `extra`; text
/// and `--json`, side by side.
fn proxy_runs(url: &str, host: &str, extra: &[&str]) -> (Output, Output) {
    let mut a = vec!["--proxy", url, "-H", host];
    a.extend(LOGIN);
    a.extend(["-N", "-C", "use share", "-C", "ls /sizes"]);
    a.extend(extra);
    let mut j = a.clone();
    j.push("--json");
    std::thread::scope(|sc| {
        let t = sc.spawn(|| run(&a, None));
        let j = run(&j, None);
        (t.join().unwrap(), j)
    })
}

/// URL, `-H`, more flags, the proxy the message names, words it holds, the most it may take.
type FailureCase = (
    String,
    &'static str,
    Vec<&'static str>,
    &'static str,
    Vec<&'static str>,
    Duration,
);

/// S63: every proxy failure: kind connect, exit 3, our words, the password never shown.
#[test]
fn proxy_failures() {
    let pw = "Wr0ngSecret";
    let p1080 = format!("socks5://{}", Ep::Proxied.addr());
    let cases: Vec<FailureCase> = vec![
        (
            format!("socks5://tester:{pw}@10.213.45.20:1081"),
            PRIVATE,
            vec![],
            "10.213.45.20:1081",
            vec!["user name or password refused"],
            NO_HANG,
        ),
        (
            format!("socks5://nobody:{pw}@10.213.45.20:1081"),
            PRIVATE,
            vec![],
            "10.213.45.20:1081",
            vec!["user name or password refused"],
            NO_HANG,
        ),
        (
            "socks5://10.213.45.20:1081".into(),
            PRIVATE,
            vec![],
            "10.213.45.20:1081",
            vec!["wants a user name and password"],
            NO_HANG,
        ),
        (
            format!("socks5://tester:{pw}@10.213.45.20:1082"),
            PRIVATE,
            vec![],
            "10.213.45.20:1082",
            vec!["refused"],
            NO_HANG,
        ),
        // The server is reachable without any proxy here: it must not be dialled (finding 1).
        (
            "socks5://10.213.45.20:1082".into(),
            "10.213.45.10",
            vec![],
            "10.213.45.20:1082",
            vec!["refused"],
            NO_HANG,
        ),
        (
            p1080.clone(),
            PRIVATE,
            vec!["-P", "446"],
            "10.213.45.20:1080",
            vec!["cannot reach fileserver.rsmb.test:446: connection refused"],
            Duration::from_secs(12),
        ),
        (
            p1080.clone(),
            "nosuch.rsmb.test",
            vec![],
            "10.213.45.20:1080",
            vec!["cannot reach nosuch.rsmb.test:445"],
            Duration::from_secs(12),
        ),
        (
            p1080.clone(),
            "10.213.47.99",
            vec![],
            "10.213.45.20:1080",
            vec!["cannot reach 10.213.47.99:445: host unreachable"],
            Duration::from_secs(12),
        ),
        (
            "socks5://10.213.45.10:445".into(),
            PRIVATE,
            vec!["--timeout", "5"],
            "10.213.45.10:445",
            vec![
                "connected to the proxy, but the SOCKS5 handshake did not finish within 10 s",
                "the proxy is silent",
                "is not a SOCKS5 proxy",
                "cannot reach fileserver.rsmb.test:445",
            ],
            Duration::from_secs(12),
        ),
    ];
    let runs: Vec<(Output, Output)> = std::thread::scope(|sc| {
        let hs: Vec<_> = cases
            .iter()
            .map(|(url, host, extra, ..)| sc.spawn(move || proxy_runs(url, host, extra)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for ((url, host, extra, proxy, words, limit), (text, json)) in cases.iter().zip(&runs) {
        let what = format!("{url} -H {host} {extra:?}");
        for o in [text, json] {
            assert_eq!(o.code, 3, "{what}: {} {}", o.stdout, o.stderr);
            assert!(
                o.took <= *limit,
                "{what}: took {:?}, at most {limit:?}",
                o.took
            );
            assert!(
                !o.stdout.contains(pw) && !o.stderr.contains(pw),
                "{what}: the password is shown"
            );
        }
        let v = json.json();
        assert_eq!(v.len(), 1, "{what}: one error and no done: {}", json.stdout);
        assert_eq!(v[0]["type"], "error", "{what}");
        assert_eq!(v[0]["kind"], "connect", "{what}");
        assert!(v[0]["command"].is_null(), "{what}");
        let m = v[0]["message"].as_str().unwrap();
        assert!(m.starts_with(&format!("proxy {proxy}: ")), "{what}: {m}");
        for w in words {
            assert!(m.contains(w), "{what}: {w:?} not in {m:?}");
        }
        assert_eq!(text.stderr, format!("rsmb: {m}\n"), "{what}: text mode");
    }
    // A wrong SMB password through the proxy: the server refused, not the proxy.
    let mut a = vec!["--proxy", &p1080, "-H", PRIVATE];
    a.extend([
        "-u", "tester", "-p", pw, "-d", "TESTDOM", "-N", "--json", "-C", "pwd",
    ]);
    let o = run(&a, None);
    assert_eq!(o.code, 3, "{}", o.stdout);
    let v = o.json();
    assert_eq!(v[0]["kind"], "auth", "{}", o.stdout);
    assert!(
        !v[0]["message"].as_str().unwrap().contains("proxy"),
        "{}",
        o.stdout
    );
}

/// S64: the private server cannot be reached around the proxy, and this host never resolves its
/// name.
#[test]
fn no_way_around_the_proxy() {
    use std::net::ToSocketAddrs;
    assert!(
        (PRIVATE, 445).to_socket_addrs().is_err(),
        "precondition: {PRIVATE} must not resolve on this host"
    );
    let direct = |host: &str, extra: &[&str]| {
        let mut a = vec!["-H", host];
        a.extend(LOGIN);
        a.extend(["-N", "--json", "-C", "pwd"]);
        a.extend(extra);
        run(&a, None)
    };
    for (host, extra) in [
        (PRIVATE, &[][..]),
        ("10.213.47.10", &["--timeout", "5"][..]),
    ] {
        let o = direct(host, extra);
        assert_eq!(o.code, 3, "direct to {host}: {}", o.stdout);
        assert_eq!(o.json()[0]["kind"], "connect", "direct to {host}");
        // No lower bound: how fast a lookup or an unreachable address fails is the host's
        // network, not rsmb (finding 9).
        assert!(
            o.took <= Duration::from_secs(12),
            "direct to {host}: {:?}",
            o.took
        );
    }
    let url = format!("socks5://{}", Ep::Proxied.addr());
    for host in [PRIVATE, "10.213.47.10", "FileServer.RSMB.Test"] {
        let mut a = vec!["--proxy", &url, "-H", host];
        a.extend(LOGIN);
        a.extend(["-N", "-C", "use share", "-C", "ls /sizes"]);
        let o = run(&a, None);
        assert_eq!(o.code, 0, "through the proxy to {host}: {}", o.stderr);
        assert_eq!(o.stdout.lines().count(), 11, "{host}: {}", o.stdout);
    }
    // The loader's own log: a name lookup on this host loads a name-service module. The control
    // (direct to the name) shows that the probe can see one.
    let nss = |args: &[&str], env: &[(&str, &str)]| {
        let mut env = env.to_vec();
        env.push(("LD_DEBUG", "files"));
        let o = run_env(args, None, &env);
        o.stderr
            .lines()
            .filter(|l| l.contains("file=libnss_"))
            .count()
    };
    let mut control = vec!["-H", PRIVATE];
    control.extend(LOGIN);
    control.extend(["-N", "-C", "pwd"]);
    let seen = nss(&control, &[]);
    if seen == 0 {
        println!("leak probe blind on this host");
    } else {
        let mut proxied = vec!["--proxy", &url, "-H", PRIVATE];
        proxied.extend(LOGIN);
        proxied.extend(["-N", "-C", "use share", "-C", "ls /sizes"]);
        assert_eq!(
            nss(&proxied, &[]),
            0,
            "a name-service module was loaded through the proxy (the control loaded {seen})"
        );
        // Kerberos with a TGT only: neither the server's name nor the KDC's (the realm's name,
        // rsmb.test) is looked up on this host (docs/plans/stage-5.md, S64).
        let dir = tmp("no_way_around_the_proxy");
        let tgt = krb_cache(&dir, "tgt.cc", "-f", "tester", &[]);
        let env = [
            ("KRB5CCNAME", tgt.to_str().unwrap()),
            ("KRB5_CONFIG", "/dev/null"),
        ];
        let kerberos = [
            "--proxy",
            &url,
            "-H",
            DC_NAME,
            "-k",
            "-N",
            "-C",
            "use share",
            "-C",
            "ls /",
        ];
        let o = run_env(&kerberos, None, &env);
        assert_eq!(o.code, 0, "Kerberos through the proxy: {}", o.stderr);
        assert_eq!(
            nss(&kerberos, &env),
            0,
            "a name-service module was loaded in a Kerberos login through the proxy (the \
             control loaded {seen})"
        );
    }
}

/// S65: cuts through the proxy, and no fallback when the proxy is gone.
#[test]
fn cut_through_the_proxy() {
    let ep = Ep::Proxied;
    let dir = tmp("cut_through_the_proxy");
    let _rm = Remove(dir.join("big.bin"));
    let s = Scratch::new(ep);
    big_copy(&dir, "big.bin");
    let mut c = ready(Client::tapped(ep), &dir, Some(&s.remote()));
    // While idle.
    assert_eq!(c.tap().drop_connections(), 1);
    let deadline = std::time::Instant::now() + WAIT;
    while c.tap().live_connections() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the cut connection stays live"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    c.cmd("pwd").expect("pwd after an idle cut", 0, 0);
    let r = c.cmd("ls");
    r.expect_ok("ls after the cut");
    assert_eq!(
        r.stats().connections,
        1,
        "exactly 1 new connection\n{}",
        log(r.stats())
    );
    assert_eq!(
        r.stats().requests,
        9,
        "3 login, TREE_CONNECT, 5\n{}",
        log(r.stats())
    );
    // At the first QUERY_DIRECTORY of a listing: run again on a new connection.
    c.send("ls /huge");
    c.wait_requests("QUERY_DIRECTORY", 1, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    r.expect_ok("ls /huge cut at its first QUERY_DIRECTORY");
    assert_eq!(r.of("entry").len(), 100_000);
    assert_eq!(
        r.stats().connections,
        1,
        "exactly 1 new connection\n{}",
        context(r.stats())
    );
    // At the 100th READ of a download: not run again, nothing left.
    let get = dir.join("dl");
    std::fs::create_dir(&get).unwrap();
    c.cmd(&format!("lcd {}", q(&get.to_string_lossy())))
        .expect_ok("lcd dl");
    c.send("get /large/big.bin");
    c.wait_requests("READ", 100, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    assert!(
        m.contains("was not run again and may or may not have taken effect"),
        "{m}"
    );
    assert_eq!(
        r.stats().connections,
        0,
        "0 new connections for the failed get"
    );
    assert!(
        !get.join("big.bin").exists(),
        "nothing under the final name"
    );
    assert!(temps_local(&get).is_empty(), "no local temp");
    // At the 100th WRITE of an upload to a new name.
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd back");
    c.send("put big.bin up.bin");
    c.wait_requests("WRITE", 100, WAIT);
    c.tap().drop_connections();
    let r = c.wait_done(WAIT);
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    wait_no_open(s.container, &fnv_temp("up.bin"), WAIT);
    assert_eq!(
        temps_on_server(s.container, &s.server()),
        Vec::<String>::new()
    );
    assert_eq!(
        docker_sh(
            s.container,
            &format!("test -e {}/up.bin && echo yes || echo no", s.server())
        )
        .trim(),
        "no",
        "nothing under the final name on the server"
    );
    c.cmd("ls").expect_ok("ls after the cut put");

    // The proxy gone, the server reachable directly (finding 1): the tap to the proxy goes to
    // `samba`, which this host reaches without any proxy. A client that fell back to a direct
    // connection would list 11 names.
    let tap = smbtap::Tap::start_socks5(Ep::Proxied.addr()).unwrap();
    let port = tap.addr().port();
    let url = format!("socks5://127.0.0.1:{port}");
    let mut args = vec!["--proxy", url.as_str(), "-H", "10.213.45.10"];
    args.extend(LOGIN);
    let mut c = Client::through(tap, &args);
    c.cmd("use share").expect_ok("use share through the tap");
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls /sizes through the tap");
    assert_eq!(r.of("entry").len(), 11);
    assert_eq!(c.startup.stats().connections + r.stats().connections, 1);
    drop(c.take_tap());
    for n in ["first", "second"] {
        let r = c.cmd("ls /sizes");
        assert_eq!(
            r.kind().as_deref(),
            Some("connect"),
            "{n} ls: {:?}",
            r.lines
        );
        let m = r.of("error")[0]["message"].as_str().unwrap();
        assert!(
            m.contains(&format!("reconnect failed: proxy 127.0.0.1:{port}")),
            "{n} ls: {m}"
        );
        assert!(
            r.of("entry").is_empty(),
            "{n} ls: a direct connection listed names"
        );
    }
    c.cmd("pwd").expect_ok("pwd");
}

/// S66: signals in a transfer through the proxy.
#[test]
fn signals_through_the_proxy() {
    let ep = Ep::Proxied;
    let dir = tmp("signals_through_the_proxy");
    let _rm = Remove(dir.join("up.bin"));
    let s = Scratch::new(ep);
    big_copy(&dir, "up.bin");
    for (line, cmd, signal, limit) in [
        ("get /large/big.bin", "READ", "INT", 10),
        ("get /large/big.bin", "READ", "TERM", 10),
        ("put up.bin", "WRITE", "INT", 10),
    ] {
        let mut c = ready(Client::tapped(ep), &dir, Some(&s.remote()));
        c.send(line);
        c.wait_requests(cmd, 100, WAIT);
        let t0 = std::time::Instant::now();
        c.kill(signal);
        let _tap = c.take_tap();
        let (code, rest) = c.wait_exit(WAIT);
        let took = t0.elapsed();
        let what = format!("{line}, SIG{signal}");
        assert_eq!(code, 130, "{what}: {rest:?}");
        assert!(
            took <= Duration::from_secs(limit),
            "{what}: exit {took:?} after the signal"
        );
        let e = rest
            .iter()
            .find(|v| v["type"] == "error")
            .unwrap_or_else(|| panic!("{what}: {rest:?}"));
        assert_eq!(e["kind"], "interrupted", "{what}");
        assert!(e.get("left_behind").is_none(), "{what}: {e}");
        assert!(!dir.join("big.bin").exists(), "{what}: a local file");
        assert!(temps_local(&dir).is_empty(), "{what}: a local temp");
        assert!(
            temps_on_server(s.container, &s.server()).is_empty(),
            "{what}: a temp on the server"
        );
        assert_eq!(
            docker_sh(
                s.container,
                &format!("test -e {}/up.bin && echo yes || echo no", s.server())
            )
            .trim(),
            "no",
            "{what}: nothing under the final name on the server"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Stage 5: Kerberos (docs/plans/stage-5.md 7.4)

/// Until the KDC's log has `want` TGS requests of `user` from `addr` (it is written as the KDC
/// answers), then exactly that many.
fn expect_kdc_requests(user: &str, addr: &str, want: usize, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while kdc_requests(user, addr) < want && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        kdc_requests(user, addr),
        want,
        "{what}: TGS requests of {user} from {addr} in the KDC's log"
    );
}

/// One-shot `-k` run: `args`, KRB5CCNAME=`cache`, KRB5_CONFIG=`conf`.
fn run_k(args: &[&str], cache: &Path, conf: &str) -> Output {
    let mut a = args.to_vec();
    a.extend(["-k", "-N"]);
    run_env(
        &a,
        None,
        &[
            ("KRB5CCNAME", cache.to_str().unwrap()),
            ("KRB5_CONFIG", conf),
        ],
    )
}

/// S71: with a TGT only, the KDC is asked once per login: direct from this host (the control:
/// the probe can see), through the proxy only from the proxy's address, also with a KDC the host
/// reaches directly, by address or by the realm's name; with the service ticket not at all. The
/// users kdirect and kproxy belong to this test alone.
#[test]
fn kerberos_kdc_is_asked_through_the_proxy() {
    let dir = tmp("kerberos_kdc_is_asked_through_the_proxy");
    let conf = krb5_conf(&dir, DC);
    let conf = conf.to_str().unwrap();
    let kdirect = krb_cache(&dir, "kdirect.cc", "-f", "kdirect", &[]);
    let kproxy = krb_cache(&dir, "kproxy.cc", "-f", "kproxy", &[]);
    let kproxy_svc = krb_cache(
        &dir,
        "kproxy-svc.cc",
        "-f",
        "kproxy",
        &["cifs/dc.rsmb.test"],
    );
    let ls = ["-C", "use share", "-C", "ls /"];
    let (host, proxy) = ("10.213.45.1", "10.213.45.20");

    let before = kdc_requests("kdirect", host);
    let mut a = vec!["-H", DC];
    a.extend(ls);
    let o = run_k(&a, &kdirect, conf);
    assert_eq!(o.code, 0, "direct: {}", o.stderr);
    expect_kdc_requests("kdirect", host, before + 1, "the control, direct");

    // The log is the container's whole log: what counts is that nothing is added to it.
    let around = kdc_requests("kproxy", host);
    let mut n = kdc_requests("kproxy", proxy);
    for (url, conf, line) in [
        (
            "socks5://10.213.45.20:1080",
            conf,
            "connected to 10.213.45.12:88",
        ),
        (
            "socks5://10.213.45.20:1080",
            "/dev/null",
            "connected to rsmb.test:88",
        ),
        (
            "socks5://tester:testpass@10.213.45.20:1081",
            conf,
            "connected to 10.213.45.12:88",
        ),
    ] {
        let lines = proxy_lines(line);
        let mut a = vec!["--proxy", url, "-H", DC_NAME];
        a.extend(ls);
        let o = run_k(&a, &kproxy, conf);
        assert_eq!(o.code, 0, "{url} {conf}: {}", o.stderr);
        n += 1;
        expect_kdc_requests("kproxy", proxy, n, &format!("{url} {conf}: from the proxy"));
        assert_eq!(
            kdc_requests("kproxy", host),
            around,
            "{url} {conf}: around the proxy"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while proxy_lines(line) <= lines && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            proxy_lines(line) > lines,
            "{url} {conf}: the proxy's log lacks {line:?}"
        );
    }
    // With the service ticket in the cache the KDC is not asked.
    let mut a = vec!["--proxy", "socks5://10.213.45.20:1080", "-H", DC_NAME];
    a.extend(ls);
    let o = run_k(&a, &kproxy_svc, "/dev/null");
    assert_eq!(o.code, 0, "service ticket: {}", o.stderr);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        kdc_requests("kproxy", proxy),
        n,
        "service ticket: the KDC was asked"
    );
    assert_eq!(
        kdc_requests("kproxy", host),
        around,
        "around the proxy, at the end"
    );
}

/// One case of `kerberos_failures`: what, arguments, cache, KRB5_CONFIG, kind, words in the
/// message, at most so many seconds.
type KCase<'a> = (
    &'a str,
    Vec<&'a str>,
    &'a Path,
    &'a str,
    &'a str,
    Vec<String>,
    u64,
);

/// S72: the KDC's failures in our words, each of the kind a script acts on: `connect` when the
/// KDC could not be asked, `auth` when it or the server refused.
#[test]
fn kerberos_failures() {
    let dir = tmp("kerberos_failures");
    let conf = krb5_conf(&dir, DC);
    let conf = conf.to_str().unwrap();
    let no88 = krb5_conf(&dir, "10.213.45.10");
    let no88 = no88.to_str().unwrap();
    let nosuch = krb5_conf(&dir, "nosuch.rsmb.test");
    let absent = krb5_conf(&dir, "10.213.47.99");
    let tgt = krb_cache(&dir, "tgt.cc", "-f", "tester", &[]);
    let samba_ticket = krb_cache(&dir, "samba.cc", "-f", "tester", &["cifs/10.213.45.10"]);
    let via1080 = ["--proxy", "socks5://10.213.45.20:1080", "-H", DC_NAME];
    let cases: Vec<KCase> = vec![
        (
            "a KDC with port 88 closed",
            vec!["-H", DC],
            &tgt,
            no88,
            "connect",
            vec![
                "Kerberos: asking the KDC 10.213.45.10:88".into(),
                "refused".into(),
                no88.into(),
            ],
            2,
        ),
        (
            "no kdc in the files: the realm's name, which the host cannot resolve",
            vec!["-H", DC],
            &tgt,
            "/dev/null",
            "connect",
            vec![
                "KDC rsmb.test:88".into(),
                "so the realm's name was tried".into(),
                "KRB5_CONFIG".into(),
                "kvno cifs/10.213.45.12".into(),
            ],
            12,
        ),
        (
            "an SPN the realm does not know",
            vec!["-H", "10.213.45.11"],
            &tgt,
            conf,
            "auth",
            vec!["has no service cifs/10.213.45.11".into()],
            12,
        ),
        (
            "a valid ticket at a server that does no Kerberos",
            vec!["-H", "10.213.45.10"],
            &samba_ticket,
            conf,
            "auth",
            vec!["did not accept the ticket".into()],
            12,
        ),
        (
            "through the proxy to a KDC name it cannot resolve",
            via1080.to_vec(),
            &tgt,
            nosuch.to_str().unwrap(),
            "connect",
            vec![
                "Kerberos: asking the KDC nosuch.rsmb.test:88".into(),
                "proxy 10.213.45.20:1080: cannot reach nosuch.rsmb.test:88".into(),
            ],
            12,
        ),
        (
            "through the proxy to a KDC address that is not there",
            via1080.to_vec(),
            &tgt,
            absent.to_str().unwrap(),
            "connect",
            vec![
                "Kerberos: asking the KDC 10.213.47.99:88".into(),
                "proxy 10.213.45.20:1080: cannot reach 10.213.47.99:88".into(),
            ],
            12,
        ),
        (
            "a closed proxy port, with a server and a KDC the host reaches directly",
            vec!["--proxy", "socks5://10.213.45.20:1082", "-H", DC],
            &tgt,
            conf,
            "connect",
            vec!["proxy 10.213.45.20:1082".into()],
            12,
        ),
    ];
    for (what, args, cache, conf, kind, words, within) in &cases {
        for json in [true, false] {
            let mut a = args.clone();
            a.extend(["-C", "use share"]);
            if json {
                a.push("--json");
            }
            let o = run_k(&a, cache, conf);
            assert_eq!(o.code, 3, "{what}, json {json}: {} {}", o.stdout, o.stderr);
            assert!(
                o.took <= Duration::from_secs(*within),
                "{what}: took {:?}",
                o.took
            );
            let m = if json {
                let v = o.json();
                assert_eq!(v.len(), 1, "{what}: one error and no done: {}", o.stdout);
                assert_eq!(v[0]["type"], "error", "{what}");
                assert_eq!(v[0]["command"], serde_json::Value::Null, "{what}");
                assert_eq!(v[0]["kind"], *kind, "{what}: {}", o.stdout);
                v[0]["message"].as_str().unwrap().to_string()
            } else {
                o.stderr.clone()
            };
            for w in words {
                assert!(m.contains(w.as_str()), "{what}: {w:?} not in {m}");
            }
            if json
                && *what == "a closed proxy port, with a server and a KDC the host reaches directly"
            {
                assert!(m.starts_with("proxy 10.213.45.20:1082"), "{m}");
            }
        }
    }
}
