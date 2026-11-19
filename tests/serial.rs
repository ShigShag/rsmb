//! One at a time under the exclusive lock (docs/testing.md).

mod common;

use std::time::Duration;

use common::*;

const T5: [&str; 2] = ["--timeout", "5"];

#[test]
fn silent_link_blackhole() {
    let _s = serial();
    let mut args = LOGIN.to_vec();
    args.extend(T5);
    let mut c = Client::with(Some(HOLE), None, &args);
    c.cmd("use share").expect_ok("use share");
    c.cmd("cd tree").expect_ok("cd tree");
    c.cmd("ls").expect_ok("ls");
    let guard = Guard(vec!["blackhole", "off"]);
    ctl(&["blackhole", "on"]);
    let r = c.cmd("ls");
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    println!("ls on a silent link failed after {:?}", r.took);
    assert!(
        r.took >= Duration::from_secs(13) && r.took <= Duration::from_secs(20),
        "expected between 13 and 20 s after the request (the server's last frame was under 2 s \
         old), took {:?}",
        r.took
    );
    let p = c.cmd("pwd");
    assert!(p.took < Duration::from_secs(1), "pwd took {:?}", p.took);
    assert_eq!(p.of("pwd")[0]["path"], "/tree");
    // The link is dead and still silent: the reconnect is tried once (its own 10 s), not twice,
    // and fails with its real kind.
    let r = c.cmd("ls");
    assert_eq!(r.kind().as_deref(), Some("connect"), "{:?}", r.lines);
    println!("ls with a failing reconnect failed after {:?}", r.took);
    assert!(
        r.took >= Duration::from_secs(9) && r.took <= Duration::from_secs(13),
        "expected one 10 s reconnect attempt, took {:?}",
        r.took
    );
    drop(guard);
    let r = c.cmd("ls");
    r.expect_ok("ls after blackhole off");
    assert_eq!(
        r.of("entry").len(),
        server_names("rsmb-samba", "tree").len()
    );
}

#[test]
fn silent_link_paused_shell() {
    let _s = serial();
    let dir = tmp("silent_link_paused_shell");
    let mut args = target("10.213.45.10:445");
    args.extend(T5.iter().map(|s| s.to_string()));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = Pty::start(&a, &dir.join("history"));
    let prompt = "//10.213.45.10/share/> ";
    p.expect("//10.213.45.10> ", WAIT);
    p.line("use share", prompt);
    p.line("ls sizes", prompt);
    let guard = Guard(vec!["unpause", "samba"]);
    ctl(&["pause", "samba"]);
    let t0 = std::time::Instant::now();
    p.send(b"ls\r");
    p.expect("connection lost", Duration::from_secs(30));
    let took = t0.elapsed();
    p.expect(prompt, Duration::from_secs(5));
    println!("ls on a paused server failed after {took:?}");
    assert!(
        took >= Duration::from_secs(13) && took <= Duration::from_secs(20),
        "expected the failure 13 to 20 s after Enter, took {took:?}"
    );
    drop(guard);
    p.send(b"ls sizes\r");
    p.expect("  8388609.bin\r\n", WAIT);
    p.expect(prompt, WAIT);
}

#[test]
fn tab_on_a_silent_link() {
    let _s = serial();
    let dir = tmp("tab_on_a_silent_link");
    let (mut p, tap, _) = Pty::tapped(Ep::Fast, &[], &dir.join("history"));
    let prompt = "//127.0.0.1/share/> ";
    p.line("use share", prompt);
    take(&tap);
    let guard = Guard(vec!["unpause", "samba"]);
    ctl(&["pause", "samba"]);
    let t0 = std::time::Instant::now();
    p.send(b"cd tr\t");
    let shown = p.expect_bytes(BEL, Duration::from_secs(15));
    let took = t0.elapsed();
    assert!(
        shown.ends_with("cd tr\x07"),
        "the line does not read 'cd tr': {shown:?}"
    );
    assert!(
        took >= Duration::from_secs(4) && took <= Duration::from_secs(8),
        "expected the BEL 4 to 8 s after the Tab, took {took:?}"
    );
    p.send(b"ee");
    let shown = p.expect("ee", Duration::from_secs(2));
    assert!(!shown.contains("tree/"), "the line changed: {shown:?}");
    p.send(CTRL_U);
    drop(guard);
    take(&tap);
    p.send(b"ls sizes\r");
    p.expect("  8388609.bin\r\n", WAIT);
    p.expect(prompt, WAIT);
    // The abandoned Tab left requests unanswered: the connection is replaced, once.
    let s = take(&tap);
    assert_eq!(
        s.connections,
        1,
        "expected exactly 1 new connection for the ls after the Tab timeout, got {}\n{}",
        s.connections,
        log(&s)
    );
}

fn one_connection(ep: Ep, service: &str) {
    let dir = tmp(&format!("one_connection_{service}"));
    let s = Scratch::new(ep);
    ctl(&["conns-reset", service]);
    let mut c = Client::tapped(ep);
    let mut conns = c.startup.stats().connections;
    for line in session_script(&dir, &s.remote()) {
        let r = c.cmd(&line);
        r.expect_ok(&line);
        conns += r.stats().connections;
    }
    let (code, _) = c.finish();
    assert_eq!(code, 0);
    let server = ctl(&["conns", service]);
    assert!(
        conns == 1 && server == "1",
        "expected 1 TCP connection in the tap and 1 on the server (ctl.sh conns {service}), got \
         {conns} and {server}"
    );
}

#[test]
fn one_connection_fast() {
    let _s = serial();
    one_connection(Ep::Fast, "samba");
}

#[test]
fn one_connection_edge() {
    let _s = serial();
    one_connection(Ep::Edge, "samba-edge");
}

/// One timed run: `use share`, the set-up lines, then `cmd` × n; text mode, direct.
fn timed(addr: &str, setup: &[&str], cmd: &str, n: usize) -> (Duration, usize) {
    let mut args = target(addr);
    args.push("-N".into());
    for l in std::iter::once(&"use share").chain(setup) {
        args.extend(["-C".to_string(), l.to_string()]);
    }
    for i in 0..n {
        args.extend(["-C".to_string(), cmd.replace("{i}", &(i % 4).to_string())]);
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = run(&a, None);
    assert_eq!(o.code, 0, "{cmd} x {n}: {}", o.stderr);
    (o.took, o.stdout.lines().count())
}

/// (t(n) - t(0)) / n in ms, median of `pairs` pairs.
fn per_command(addr: &str, setup: &[&str], cmd: &str, n: usize, pairs: usize, lines: usize) -> f64 {
    let mut v = Vec::new();
    for _ in 0..pairs {
        let (t0, _) = timed(addr, setup, cmd, 0);
        let (tn, got) = timed(addr, setup, cmd, n);
        assert_eq!(got, n * lines, "{cmd} x {n}: output lines");
        v.push((tn.as_secs_f64() - t0.as_secs_f64()) * 1e3 / n as f64);
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Through the tap at the slow endpoint: the lines cost what the timing assumes.
fn tapped_cost(setup: &[&str], cmd: &str, n: usize, requests: u64, flights: u64) {
    let mut c = Client::tapped(Ep::Slow);
    c.cmd("use share").expect_ok("use share");
    for l in setup {
        c.cmd(l).expect_ok(l);
    }
    let (mut r, mut f) = (0, 0);
    let mut seen = String::new();
    for i in 0..n {
        let line = cmd.replace("{i}", &(i % 4).to_string());
        let x = c.cmd(&line);
        x.expect_ok(&line);
        r += x.stats().requests;
        f += x.stats().flights;
        seen += &format!("{line}:\n{}", context(x.stats()));
    }
    assert!(
        r == requests && f == flights,
        "slow endpoint: expected {requests} requests in {flights} flights for {n} {cmd}, got {r} \
         in {f}\n{seen}"
    );
}

fn drops() -> u64 {
    ctl(&["drops"]).parse().unwrap()
}

#[test]
fn timing_cd() {
    let _s = serial();
    let before = drops();
    let cd = "cd /tree/d{i}";
    tapped_cost(&[], cd, 10, 20, 10);
    let rtt = connect_ms(SLOW);
    let slow = per_command(SLOW, &[], cd, 10, 3, 0);
    let rtt_fast = connect_ms(FAST);
    let fast = per_command(FAST, &[], cd, 10, 3, 0);
    println!(
        "cd: slow {slow:.1} ms (connect {rtt:.1} ms), fast {fast:.2} ms (connect {rtt_fast:.2} \
         ms); smbclient-ng baseline at slow: 1610.1 ms"
    );
    assert!(
        slow <= 1.25 * rtt,
        "cd at slow: {slow:.1} ms, expected at most 1.25 x {rtt:.1} ms"
    );
    assert!(
        fast <= 20.0,
        "cd at fast: {fast:.2} ms, expected at most 20 ms"
    );
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

#[test]
fn timing_ls_small() {
    let _s = serial();
    let before = drops();
    let setup = ["cd /tree/d0"];
    tapped_cost(&setup, "ls", 10, 50, 10);
    let rtt = connect_ms(SLOW);
    let slow = per_command(SLOW, &setup, "ls", 10, 3, 5);
    println!("ls tree/d0: slow {slow:.1} ms (connect {rtt:.1} ms); smbclient-ng: 1207.9 ms");
    assert!(
        slow <= 1.25 * rtt,
        "ls at slow: {slow:.1} ms, expected at most 1.25 x 1 x {rtt:.1} ms"
    );
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

#[test]
fn timing_ls_large() {
    let _s = serial();
    let before = drops();
    let many = per_command(SLOW, &["cd /many"], "ls", 10, 1, 10_000);
    let huge = per_command(SLOW, &["cd /huge"], "ls", 1, 1, 100_000);
    println!(
        "ls at slow: many/ {many:.1} ms (smbclient-ng 4535.4 ms), huge/ {huge:.1} ms \
         (smbclient-ng 37320.2 ms)"
    );
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

// ---------------------------------------------------------------------------------------------
// Stage 2 (docs/plans/stage-2.md 6.4)

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

/// A local copy of a small-file directory: 100 files of 1 to 4 KiB.
fn small_dir(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 1..=100u64 {
        make_file(
            &dir.join(format!("f{i:03}.bin")),
            1024 + (i * 31) % 3072,
            i as u8,
        );
    }
}

fn exists_on(s: &Scratch, rel: &str) -> bool {
    docker_sh(
        s.container,
        &format!(
            "test -e {} && echo yes || echo no",
            sq(&format!("{}/{rel}", s.server()))
        ),
    )
    .trim()
        == "yes"
}

/// X21
#[test]
fn timing_small_files() {
    let _s = serial();
    let before = drops();
    let dir = tmp("timing_small_files");
    let s = Scratch::new(Ep::Slow);
    let rt = connect_ms(SLOW);
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    let g = c.cmd("get -r /smallfiles/d01");
    g.expect_ok("get -r d01");
    let p = c.cmd("put -r d01");
    p.expect_ok("put -r d01");
    let (gm, pm) = (g.took.as_secs_f64() * 1e3, p.took.as_secs_f64() * 1e3);
    println!(
        "small files at slow (round trip {rt:.1} ms): get -r d01 {gm:.0} ms (bound 1.5 x 8 rt = \
         {:.0}, one after another 101 rt = {:.0}); put -r d01 {pm:.0} ms (bound 1.5 x 22 rt = \
         {:.0}, one after another 301 rt = {:.0})",
        1.5 * 8.0 * rt,
        101.0 * rt,
        1.5 * 22.0 * rt,
        301.0 * rt
    );
    assert!(
        gm <= 1.5 * 8.0 * rt,
        "get: {gm:.0} ms, expected at most 1.5 x 8 x {rt:.1} ms"
    );
    assert!(
        pm <= 1.5 * 22.0 * rt,
        "put: {pm:.0} ms, expected at most 1.5 x 22 x {rt:.1} ms"
    );
    let fixture = server_tree(SAMBA, "/srv/share/smallfiles/d01");
    assert_eq!(local_tree(&dir.join("d01")), fixture);
    assert_eq!(server_tree(SAMBA, &format!("{}/d01", s.server())), fixture);
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

/// X22
#[test]
fn timing_medium_slow() {
    let _s = serial();
    let before = drops();
    let dir = tmp("timing_medium_slow");
    let s = Scratch::new(Ep::Slow);
    let mut c = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    let g = c.cmd("get /large/medium.bin");
    g.expect_ok("get medium");
    let p = c.cmd("put medium.bin");
    p.expect_ok("put medium");
    let mb = |d: Duration| 52_432_897.0 / d.as_secs_f64() / 1e6;
    println!(
        "medium.bin at slow: get {:.1} MB/s, put {:.1} MB/s (iperf3 through wan: about 20 and 17 MB/s)",
        mb(g.took),
        mb(p.took)
    );
    assert_eq!(local_sha(&dir.join("medium.bin")), MEDIUM_SHA);
    assert_eq!(
        server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

/// X23
#[test]
fn silent_link_mid_get() {
    let _s = serial();
    let dir = tmp("silent_link_mid_get");
    let mut args = LOGIN.to_vec();
    args.extend(T5);
    let mut c = ready(Client::with(Some(HOLE), None, &args), &dir, None);
    let guard = Guard(vec!["blackhole", "off"]);
    c.send("get /large/medium.bin");
    wait_file(&dir.join(fnv_temp("medium.bin")), 4 << 20, WAIT);
    ctl(&["blackhole", "on"]);
    let t0 = Instant::now();
    let r = c.wait_done(WAIT);
    let took = t0.elapsed();
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    println!("get on a silent link failed {took:?} after the blackhole");
    assert!(
        took >= Duration::from_secs(13) && took <= Duration::from_secs(20),
        "expected 13 to 20 s after the blackhole, took {took:?}"
    );
    assert!(!dir.join("medium.bin").exists());
    assert!(temps_local(&dir).is_empty(), "no temp locally");
    drop(guard);
    c.cmd("ls /sizes").expect_ok("ls after blackhole off");
}

/// X24
#[test]
fn silent_link_mid_put() {
    let _s = serial();
    let dir = tmp("silent_link_mid_put");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    let temp = fnv_temp("medium.bin");
    let mut args = LOGIN.to_vec();
    args.extend(T5);
    let mut c = ready(
        Client::with(Some(HOLE), None, &args),
        &dir,
        Some(&s.remote()),
    );
    let guard = Guard(vec!["blackhole", "off"]);
    c.send("put -f medium.bin");
    wait_server_file(
        s.container,
        &format!("{}/{temp}", s.server()),
        4 << 20,
        WAIT,
    );
    ctl(&["blackhole", "on"]);
    let t0 = Instant::now();
    let r = c.wait_done(WAIT);
    let took = t0.elapsed();
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    println!("put on a silent link failed {took:?} after the blackhole");
    assert!(
        took >= Duration::from_secs(13) && took <= Duration::from_secs(20),
        "expected 13 to 20 s after the blackhole, took {took:?}"
    );
    let e = r.of("error")[0];
    let named = format!("{}/{temp}", s.remote());
    assert_eq!(
        e["left_behind"],
        serde_json::json!([named]),
        "left_behind names the temp: {e}"
    );
    assert!(e["message"].as_str().unwrap().contains(&named), "{e}");
    assert!(!exists_on(&s, "medium.bin"), "nothing under the final name");
    drop(guard);
    assert!(exists_on(&s, &temp), "the file named exists on the server");
    drop(c);
    // At once, in a new session: success, or "in use" with nothing changed.
    let mut d = ready(Client::direct(SLOW), &dir, Some(&s.remote()));
    let r = d.cmd("put -f medium.bin");
    if r.ok {
        println!("the put right after the silent link succeeded");
        assert_eq!(
            server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
            MEDIUM_SHA
        );
    } else {
        let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
        println!("the put right after the silent link: {m}");
        assert!(m.contains("in use"), "{m}");
        assert!(
            !exists_on(&s, "medium.bin"),
            "nothing under the final name changed"
        );
    }
    wait_no_open(s.container, &temp, Duration::from_secs(60));
    d.cmd("put -f medium.bin").expect_ok("put once more");
    assert_eq!(
        server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert!(
        temps_on_server(s.container, &s.server()).is_empty(),
        "the temp is gone"
    );
}

/// Review finding 1 of Stage 2: a temporary file that the cleanup could not delete is reported
/// once and then dropped from the registry. Later commands cost no extra connection, carry no
/// `left_behind`, and a retry of the same name is not refused as "running".
#[test]
fn leftover_temp_is_reported_once() {
    let _s = serial();
    let dir = tmp("leftover_temp_is_reported_once");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    let temp = fnv_temp("medium.bin");
    let mut args = LOGIN.to_vec();
    args.extend(T5);
    let mut c = ready(Client::tapped_at(HOLE, &args), &dir, Some(&s.remote()));
    let guard = Guard(vec!["blackhole", "off"]);
    c.send("put -f medium.bin");
    wait_server_file(
        s.container,
        &format!("{}/{temp}", s.server()),
        4 << 20,
        WAIT,
    );
    ctl(&["blackhole", "on"]);
    // No tap work here: the cut connection never settles.
    let (lines, ok) = c.read_done(WAIT);
    assert!(!ok, "{lines:?}");
    let e = lines.iter().find(|v| v["type"] == "error").unwrap();
    assert_eq!(e["kind"], "connection_lost", "{e}");
    assert_eq!(e["left_behind"].as_array().unwrap().len(), 1, "{e}");
    drop(guard);
    c.tap().reset();
    // The server still holds the file open. Four commands, one reconnect in all.
    let mut conns = 0;
    for line in ["mkdir a", "rmdir a", "mkdir b", "rmdir b"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        assert!(r.of("error").is_empty(), "{line}: {:?}", r.lines);
        conns += r.stats().connections;
        assert!(
            conns <= 1,
            "{line}: {conns} new connections since the failure (the leftover is cleaned again)\n{}",
            log(r.stats())
        );
    }
    // The same name again: never "running"; "in use" while the server holds the file.
    let r = c.cmd("put -f medium.bin");
    if !r.ok {
        let e = r.of("error")[0];
        let m = e["message"].as_str().unwrap();
        assert!(!m.contains("is running"), "{m}");
        assert!(m.contains("in use"), "{m}");
    }
    wait_no_open(s.container, &temp, Duration::from_secs(60));
    c.cmd("put -f medium.bin").expect_ok("put once more");
    assert_eq!(
        server_sha(SAMBA, &format!("{}/medium.bin", s.server())),
        MEDIUM_SHA
    );
    assert!(temps_on_server(s.container, &s.server()).is_empty());
}

/// X25: five rounds, because the dropped-writer design failed in 4 of 11 (finding 4).
#[test]
fn signal_mid_put_edge() {
    let _s = serial();
    let dir = tmp("signal_mid_put_edge");
    let _rm = Remove(dir.join("big300.bin"));
    make_file(&dir.join("big300.bin"), 300 << 20, 9);
    let s = Scratch::new(Ep::Edge);
    let temp = fnv_temp("big300.bin");
    for round in 1..=5 {
        let (code, rest, took, st) = interrupt(
            Ep::Edge,
            &dir,
            Some(&s.remote()),
            "put big300.bin",
            "WRITE",
            50,
            &["INT"],
        );
        assert_eq!(code, 130, "round {round}: {rest:?}");
        let e = rest.iter().find(|v| v["type"] == "error").unwrap();
        assert_eq!(e["kind"], "interrupted", "round {round}");
        assert!(e.get("left_behind").is_none(), "round {round}: {e}");
        println!(
            "round {round}: exit {took:?} after the signal, {} new connections",
            st.connections
        );
        assert_eq!(
            st.connections,
            0,
            "round {round}: expected the abort and the delete on the first connection\n{}",
            log(&st)
        );
        wait_no_open(s.container, &temp, Duration::from_secs(60));
        assert!(
            temps_on_server(s.container, &s.server()).is_empty(),
            "round {round}: no temp"
        );
        assert!(
            !exists_on(&s, "big300.bin"),
            "round {round}: nothing under the final name"
        );
    }
}

/// Starts a non-interactive client in `dir` (in `cd`), sends `line`, waits for `n` requests of
/// `cmd`, sends the signals, returns exit code, the remaining objects, the time from the first
/// signal to the exit, and the tap's stats from the line on.
fn interrupt(
    ep: Ep,
    dir: &Path,
    cd: Option<&str>,
    line: &str,
    cmd: &str,
    n: u64,
    signals: &[&str],
) -> (i32, Vec<Value>, Duration, smbtap::Stats) {
    let mut c = ready(Client::tapped(ep), dir, cd);
    c.send(line);
    c.wait_requests(cmd, n, WAIT);
    let t0 = Instant::now();
    for (i, sig) in signals.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        c.kill(sig);
    }
    let tap = c.take_tap();
    let (code, rest) = c.wait_exit(WAIT);
    let took = t0.elapsed();
    let tap = tap.unwrap();
    assert!(tap.settle(Duration::from_millis(300), WAIT));
    (code, rest, took, tap.stats())
}

/// X27
#[test]
fn signals_mid_get() {
    let _s = serial();
    let dir = tmp("signals_mid_get");
    for signals in [&["INT"][..], &["TERM"], &["INT", "INT"]] {
        let (code, rest, took, _) = interrupt(
            Ep::Slow,
            &dir,
            None,
            "get /large/medium.bin",
            "READ",
            8,
            signals,
        );
        assert_eq!(code, 130, "{signals:?}: {rest:?}");
        let e = rest
            .iter()
            .find(|v| v["type"] == "error")
            .unwrap_or_else(|| panic!("{signals:?}: {rest:?}"));
        assert_eq!(e["kind"], "interrupted", "{signals:?}");
        assert!(
            took <= Duration::from_secs(3),
            "{signals:?}: exit {took:?} after the signal"
        );
        assert!(!dir.join("medium.bin").exists(), "{signals:?}");
        assert!(temps_local(&dir).is_empty(), "{signals:?}: no temp");
    }
}

/// X28
#[test]
fn signals_mid_put() {
    let _s = serial();
    let dir = tmp("signals_mid_put");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    for signals in [&["INT"][..], &["TERM"], &["INT", "INT"]] {
        let (code, rest, took, st) = interrupt(
            Ep::Slow,
            &dir,
            Some(&s.remote()),
            "put medium.bin",
            "WRITE",
            8,
            signals,
        );
        assert_eq!(code, 130, "{signals:?}: {rest:?}");
        let e = rest
            .iter()
            .find(|v| v["type"] == "error")
            .unwrap_or_else(|| panic!("{signals:?}: {rest:?}"));
        assert_eq!(e["kind"], "interrupted", "{signals:?}");
        println!(
            "{signals:?}: exit {took:?} after the signal, {} new connections",
            st.connections
        );
        assert!(
            took <= Duration::from_secs(4),
            "{signals:?}: exit {took:?} after the signal"
        );
        assert!(
            !exists_on(&s, "medium.bin"),
            "{signals:?}: nothing under the final name"
        );
        let temps = temps_on_server(s.container, &s.server());
        if signals.len() == 1 {
            assert!(temps.is_empty(), "{signals:?}: no temp");
            assert!(e.get("left_behind").is_none(), "{signals:?}: {e}");
            assert_eq!(
                st.connections,
                0,
                "{signals:?}: expected the abort and the delete on the first connection (0 new connections)\n{}",
                log(&st)
            );
        } else {
            // Either the second signal came after the delete (nothing left, nothing named) or
            // before it (the temporary is left and the error names exactly it).
            assert_named(&format!("{signals:?}"), &temps, e);
        }
        if !temps.is_empty() {
            wait_no_open(
                s.container,
                &fnv_temp("medium.bin"),
                Duration::from_secs(60),
            );
            s.sh(&format!("rm -f {}", fnv_temp("medium.bin")));
        }
    }
}

/// X26
#[test]
fn signal_in_the_small_file_phase() {
    let _s = serial();
    let dir = tmp("signal_in_the_small_file_phase");
    let s = Scratch::new(Ep::Slow);
    small_dir(&dir.join("d01"));
    s.sh("cp -r /srv/share/smallfiles/d01 copy");
    let (code, rest, took, _) = interrupt(
        Ep::Slow,
        &dir,
        Some(&s.remote()),
        "put -r d01",
        "CREATE",
        40,
        &["INT"],
    );
    assert_eq!(code, 130, "{rest:?}");
    assert!(
        took <= Duration::from_secs(6),
        "put -r: exit {took:?} after the signal"
    );
    assert!(
        rest.iter().all(|v| v["type"] != "summary"),
        "no summary: {rest:?}"
    );
    let e = rest.iter().find(|v| v["type"] == "error").unwrap();
    assert_eq!(e["kind"], "interrupted");
    let (files, _) = server_tree(SAMBA, &format!("{}/d01", s.server()));
    let local = local_tree(&dir.join("d01")).0;
    for (k, sha) in &files {
        if k.starts_with(b"./.rsmb-") {
            continue;
        }
        assert_eq!(
            Some(sha),
            local.get(k),
            "{}: the checksum of its source",
            String::from_utf8_lossy(k)
        );
    }
    let temps = temps_on_server(s.container, &s.server());
    assert_named("put -r", &temps, e);
    println!(
        "put -r: {} files arrived before the interrupt, {took:?}",
        files.len()
    );
    let (code, rest, took, _) = interrupt(
        Ep::Slow,
        &dir,
        Some(&s.remote()),
        "get -r copy",
        "READ",
        40,
        &["INT"],
    );
    assert_eq!(code, 130, "{rest:?}");
    assert!(
        took <= Duration::from_secs(6),
        "get -r: exit {took:?} after the signal"
    );
    assert!(
        rest.iter().all(|v| v["type"] != "summary"),
        "no summary: {rest:?}"
    );
    let (got, _) = local_tree(&dir.join("copy"));
    let (want, _) = server_tree(SAMBA, &format!("{}/copy", s.server()));
    for (k, sha) in &got {
        assert_eq!(Some(sha), want.get(k), "{}", String::from_utf8_lossy(k));
    }
    assert!(
        temps_local(&dir).is_empty(),
        "no local temp after the process has ended"
    );
    println!(
        "get -r: {} files arrived before the interrupt, {took:?}",
        got.len()
    );
}

/// X29
#[test]
fn ctrl_c_in_the_shell() {
    let _s = serial();
    let dir = tmp("ctrl_c_in_the_shell");
    let s = Scratch::new(Ep::Slow);
    medium_copy(&dir);
    std::fs::rename(dir.join("medium.bin"), dir.join("up.bin")).unwrap();
    let lcd = format!("lcd {}", q(&dir.to_string_lossy()));
    let (mut p, tap, _) = Pty::tapped(Ep::Slow, &["-C", &lcd], &dir.join("history"));
    let here = format!("//127.0.0.1/share{}> ", s.remote());
    p.line("use share", "//127.0.0.1/share/> ");
    p.line(&format!("cd {}", s.remote()), &here);
    take(&tap);
    for (line, cmd) in [
        ("get /large/medium.bin", "WRITE_NONE"),
        ("put up.bin", "WRITE"),
    ] {
        p.send(format!("{line}\r").as_bytes());
        if cmd == "WRITE" {
            wait_requests(&tap, "WRITE", 8, WAIT);
        } else {
            wait_requests(&tap, "READ", 8, WAIT);
        }
        p.send(b"\x03");
        p.expect("interrupted", Duration::from_secs(5));
        p.expect(&here, Duration::from_secs(5));
        take(&tap);
        let t0 = Instant::now();
        p.send(b"ls /sizes\r");
        p.expect("  8388609.bin\r\n", WAIT);
        let took = t0.elapsed();
        p.expect(&here, WAIT);
        let st = take(&tap);
        assert!(
            took < Duration::from_secs(3),
            "{line}: ls /sizes after the interrupt took {took:?}"
        );
        assert_eq!(
            st.connections,
            1,
            "{line}: exactly 1 new connection\n{}",
            log(&st)
        );
        assert!(temps_local(&dir).is_empty(), "{line}: no local temp");
        assert!(
            temps_on_server(s.container, &s.server()).is_empty(),
            "{line}: no temp on the server"
        );
    }
    p.send(b"pwd\r");
    p.expect(&format!("//127.0.0.1/share{}\r\n", s.remote()), WAIT);
}

// ---------------------------------------------------------------------------------------------
// Stage 3: tree and find (docs/plans/stage-3.md 6.4)

/// X41
#[test]
fn timing_walk_slow() {
    let _s = serial();
    let before = drops();
    let rt = connect_ms(SLOW);
    let mut c = Client::direct(SLOW);
    c.cmd("use share").expect_ok("use share");
    let bound = 1.25 * 23.0 * rt;
    for line in ["find /tree", "tree /tree"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        assert_eq!(r.of("entry").len(), 682, "{line}");
        let ms = r.took.as_secs_f64() * 1e3;
        println!(
            "{line} at slow (round trip {rt:.1} ms): {ms:.0} ms; bound 1.25 x 23 waves = \
             {bound:.0} ms; one after another 342 round trips = {:.0} ms; Stage 0 through the \
             crate 18.5 s",
            342.0 * rt
        );
        assert!(
            ms <= bound,
            "{line}: {ms:.0} ms, expected at most 1.25 x 23 x {rt:.1} ms"
        );
    }
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}

/// X42
#[test]
fn signals_mid_walk() {
    let _s = serial();
    let dir = tmp("signals_mid_walk");
    for (line, signal) in [
        ("find /fanout", "INT"),
        ("find /fanout", "TERM"),
        ("tree /fanout", "INT"),
    ] {
        let (code, rest, took, _) = interrupt(
            Ep::Slow,
            &dir,
            None,
            line,
            "QUERY_DIRECTORY",
            200,
            &[signal],
        );
        assert_eq!(code, 130, "{line} {signal}: {rest:?}");
        assert!(
            took <= Duration::from_secs(3),
            "{line} {signal}: exit {took:?} after the signal"
        );
        for v in &rest {
            assert!(
                v["type"].is_string(),
                "{line} {signal}: not a whole line: {v}"
            );
        }
        let e = rest.iter().find(|v| v["type"] == "error").unwrap();
        assert_eq!(e["kind"], "interrupted", "{line} {signal}");
        assert!(
            rest.iter().all(|v| v["type"] != "walk_summary"),
            "{line} {signal}: no walk_summary"
        );
        let entries = rest.iter().filter(|v| v["type"] == "entry").count();
        if line.starts_with("find") {
            assert!(
                entries >= 1,
                "{line} {signal}: the matches so far were printed"
            );
        } else {
            assert_eq!(
                entries, 0,
                "{line} {signal}: tree prints nothing before the walk ends"
            );
        }
        println!("{line} {signal}: exit {took:?} after the signal, {entries} entries before it");
    }
}

/// X43
#[test]
fn ctrl_c_mid_walk() {
    let _s = serial();
    let dir = tmp("ctrl_c_mid_walk");
    let (mut p, tap, _) = Pty::tapped(Ep::Slow, &[], &dir.join("history"));
    let prompt = "//127.0.0.1/share/> ";
    p.line("use share", prompt);
    take(&tap);
    p.send(b"find /fanout\r");
    wait_requests(&tap, "QUERY_DIRECTORY", 200, WAIT);
    let t0 = Instant::now();
    p.send(b"\x03");
    p.expect("interrupted", Duration::from_secs(3));
    p.expect(prompt, Duration::from_secs(3));
    println!("find /fanout: the prompt {:?} after Ctrl-C", t0.elapsed());
    assert!(t0.elapsed() <= Duration::from_secs(3));
    take(&tap);
    let out = p.line("ls /sizes", prompt);
    assert_eq!(out.matches(".bin\r\n").count(), 11, "{out}");
    let st = take(&tap);
    assert_eq!(
        st.connections,
        1,
        "ls after the interrupt: exactly 1 new connection\n{}",
        log(&st)
    );
    let out = p.line("pwd", prompt);
    assert!(out.contains("//127.0.0.1/share/\r\n"), "{out}");
    let out = p.line("find /tree/d0", prompt);
    let lines = out
        .split("\r\n")
        .filter(|l| l.contains("/tree/d0/"))
        .count();
    assert_eq!(lines, 169, "{out}");
    let st = take(&tap);
    assert_eq!(
        st.connections,
        0,
        "find /tree/d0: 0 new connections\n{}",
        log(&st)
    );
}

/// X44: walks from the share root end at every loop.
#[test]
fn walk_root_loops() {
    let _s = serial();
    let mut c = Client::tapped(Ep::Fast);
    c.cmd("use share").expect_ok("use share");
    // /links/b/c is at depth 3: -maxdepth 4 lists it (docs/plans/stage-3.md, deviations).
    for (line, depth) in [("find / -maxdepth 4 -name self", 4), ("tree -L 3 /", 3)] {
        let r = c.cmd(line);
        r.expect_ok(line);
        let sk: Vec<(String, String)> = skipped(&r)
            .iter()
            .map(|v| {
                assert_eq!(v["reason"], "already_listed", "{v}");
                (
                    v["path"].as_str().unwrap().to_string(),
                    v["same_as"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let mut want = vec![
            ("/loops/self".to_string(), "/loops".to_string()),
            ("/loops/up".to_string(), "/".to_string()),
        ];
        if depth == 4 {
            want.insert(0, ("/links/b/c".into(), "/links/a/to-c".into()));
        }
        assert_eq!(sk, want, "{line}");
        let d: Vec<&str> = denied(&r)
            .iter()
            .map(|v| v["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            d,
            ["/forbidden", "/traverse-only", "/tree/forbidden"],
            "{line}"
        );
        let dirs = server_find(
            "rsmb-samba",
            ".",
            &["-maxdepth", &(depth - 1).to_string(), "-type", "d"],
        );
        let sum = walk_summary(&r).unwrap();
        assert_eq!(
            sum["directories"].as_u64().unwrap() as usize,
            1 + dirs.len() - 3,
            "{line}: the start, the server's directories above depth {depth}, less the 3 denied"
        );
        for e in r.of("entry") {
            let p = e["path"].as_str().unwrap();
            assert!(
                !p.starts_with("/loops/self/") && !p.starts_with("/loops/up/"),
                "{line}: {p}"
            );
        }
        assert_eq!(r.stats().connections, 0, "{line}");
        println!(
            "{line}: {} directories listed, {:?}",
            sum["directories"], r.took
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Stage 4: the proxy (docs/plans/stage-4.md 6.4)

/// Until the proxy's log has `want` lines with `needle` (it is written as the proxy connects),
/// then exactly that many.
fn expect_proxy_lines(needle: &str, want: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while proxy_lines(needle) < want && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let got = proxy_lines(needle);
    assert_eq!(
        got, want,
        "{what}: lines with {needle:?} in the proxy's log"
    );
}

/// X61: one connection on the private server and one line in the proxy's log per session, the
/// name as rsmb was given it, one more of each for a reconnect.
#[test]
fn one_connection_through_the_proxy() {
    let _s = serial();
    let line = "connected to fileserver.rsmb.test:445";
    for ep in [Ep::Proxied, Ep::ProxiedLogin] {
        let dir = tmp(&format!("one_connection_through_the_proxy_{ep:?}"));
        let s = Scratch::new(ep);
        ctl(&["conns-reset", "samba-private"]);
        let before = proxy_lines(line);
        let mut c = Client::tapped(ep);
        let mut conns = c.startup.stats().connections;
        for l in session_script(&dir, &s.remote()) {
            let r = c.cmd(&l);
            r.expect_ok(&l);
            conns += r.stats().connections;
        }
        let (code, _) = c.finish();
        assert_eq!(code, 0);
        let server = ctl(&["conns", "samba-private"]);
        assert!(
            conns == 1 && server == "1",
            "{ep:?}: expected 1 TCP connection in the tap, 1 on the server (ctl.sh conns \
             samba-private), got {conns} and {server}"
        );
        expect_proxy_lines(line, before + 1, &format!("{ep:?}: 1 more line"));
    }
    // The name goes out as given; a reconnect is one more line and one more connection.
    let mixed = "connected to FileServer.RSMB.Test:445";
    ctl(&["conns-reset", "samba-private"]);
    let before = proxy_lines(mixed);
    let tap = Ep::Proxied.tap();
    let url = format!("socks5://{}", tap.addr());
    let mut args = vec!["--proxy", url.as_str(), "-H", "FileServer.RSMB.Test"];
    args.extend(LOGIN);
    let mut c = Client::through(tap, &args);
    c.cmd("use share").expect_ok("use share");
    c.cmd("ls /sizes").expect_ok("ls /sizes");
    expect_proxy_lines(mixed, before + 1, "the name as given");
    assert_eq!(c.tap().drop_connections(), 1);
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls after the cut");
    assert_eq!(r.stats().connections, 1);
    expect_proxy_lines(mixed, before + 2, "the reconnect");
    assert_eq!(
        ctl(&["conns", "samba-private"]),
        "2",
        "two connections on the server"
    );
}

/// X62: a silent proxy: the request fails like a silent server, the reconnect says the handshake
/// did not finish and does not blame the server (finding 3); recovery.
#[test]
fn silent_proxy() {
    let _s = serial();
    let mut c = Client::to(Ep::Proxied, &T5);
    c.cmd("use share").expect_ok("use share");
    c.cmd("ls /sizes").expect_ok("ls /sizes");
    let guard = Guard(vec!["unpause", "socks"]);
    ctl(&["pause", "socks"]);
    let r = c.cmd("ls /sizes");
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    println!("ls through a paused proxy failed after {:?}", r.took);
    assert!(
        r.took >= Duration::from_secs(13) && r.took <= Duration::from_secs(20),
        "expected 13 to 20 s, took {:?}",
        r.took
    );
    let p = c.cmd("pwd");
    assert!(p.took < Duration::from_secs(1), "pwd took {:?}", p.took);
    let r = c.cmd("ls /sizes");
    println!(
        "the reconnect into a paused proxy failed after {:?}",
        r.took
    );
    assert_eq!(r.kind().as_deref(), Some("connect"), "{:?}", r.lines);
    assert!(
        r.took >= Duration::from_secs(9) && r.took <= Duration::from_secs(13),
        "expected one 10 s dial, took {:?}",
        r.took
    );
    let m = r.of("error")[0]["message"].as_str().unwrap();
    for w in [
        "reconnect failed: proxy 10.213.45.20:1080",
        "the SOCKS5 handshake did not finish within 10 s",
        "is silent",
    ] {
        assert!(m.contains(w), "{w:?} not in {m:?}");
    }
    drop(guard);
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls after the unpause");
    assert_eq!(r.of("entry").len(), 11);
}

/// X63: Ctrl-C in the shell during a download through the proxy.
#[test]
fn ctrl_c_through_the_proxy() {
    let _s = serial();
    let dir = tmp("ctrl_c_through_the_proxy");
    let lcd = format!("lcd {}", q(&dir.to_string_lossy()));
    let (mut p, tap, _) = Pty::tapped(Ep::Proxied, &["-C", &lcd], &dir.join("history"));
    let prompt = "//fileserver.rsmb.test/share/> ";
    p.line("use share", prompt);
    take(&tap);
    p.send(b"get /large/big.bin\r");
    wait_requests(&tap, "READ", 100, WAIT);
    let t0 = Instant::now();
    p.send(b"\x03");
    p.expect("interrupted", Duration::from_secs(5));
    p.expect(prompt, Duration::from_secs(5));
    let took = t0.elapsed();
    assert!(
        took <= Duration::from_secs(3),
        "the prompt came back after {took:?}"
    );
    assert!(!dir.join("big.bin").exists(), "no local file");
    assert!(temps_local(&dir).is_empty(), "no local temp");
    take(&tap);
    p.send(b"ls /sizes\r");
    p.expect("  8388609.bin\r\n", WAIT);
    p.expect(prompt, WAIT);
    let st = take(&tap);
    assert_eq!(st.connections, 1, "exactly 1 new connection\n{}", log(&st));
    p.send(b"pwd\r");
    p.expect("//fileserver.rsmb.test/share/\r\n", WAIT);
}

// ---------------------------------------------------------------------------------------------
// Stage 5: Kerberos (docs/plans/stage-5.md 7.4)

/// One-shot `-k` run: `args`, KRB5CCNAME=`cache`, KRB5_CONFIG=`conf`, `--json`.
fn run_k(args: &[&str], cache: &Path, conf: &str) -> Output {
    let mut a = args.to_vec();
    a.extend(["-k", "-N", "--json", "-C", "use share", "-C", "ls /"]);
    run_env(
        &a,
        None,
        &[
            ("KRB5CCNAME", cache.to_str().unwrap()),
            ("KRB5_CONFIG", conf),
        ],
    )
}

/// X71: a KDC that accepts the connection and says nothing: one 10 s limit for the exchange,
/// kind connect, the message names the KDC; after the unpause a login works.
#[test]
fn silent_kdc() {
    let _s = serial();
    let dir = tmp("silent_kdc");
    let conf = krb5_conf(&dir, DC);
    let conf = conf.to_str().unwrap();
    let tgt = krb_cache(&dir, "tgt.cc", "-f", "tester", &[]);
    let guard = Guard(vec!["unpause", "samba-dc"]);
    ctl(&["pause", "samba-dc"]);
    let o = run_k(&["-H", "10.213.45.10"], &tgt, conf);
    println!(
        "a TGT-only login with a paused KDC failed after {:?}",
        o.took
    );
    assert_eq!(o.code, 3, "{}", o.stdout);
    assert!(
        o.took >= Duration::from_secs(9) && o.took <= Duration::from_secs(13),
        "expected the 10 s of the exchange, took {:?}",
        o.took
    );
    let v = o.json();
    assert_eq!(v.len(), 1, "{}", o.stdout);
    assert_eq!(v[0]["kind"], "connect", "{}", o.stdout);
    let m = v[0]["message"].as_str().unwrap();
    for w in [
        "KDC 10.213.45.12:88",
        "connected to the KDC, but it did not answer within 10 s",
    ] {
        assert!(m.contains(w), "{w:?} not in {m:?}");
    }
    drop(guard);
    let o = run_k(&["-H", DC], &tgt, conf);
    assert_eq!(o.code, 0, "after the unpause: {}", o.stdout);
}

/// X72: through the proxy, by name: a TGT-only login is one connection to the KDC (the realm's
/// name) and one to the server; with the service ticket only the server's.
#[test]
fn kerberos_connections_through_the_proxy() {
    let _s = serial();
    let dir = tmp("kerberos_connections_through_the_proxy");
    let tgt = krb_cache(&dir, "tgt.cc", "-f", "tester", &[]);
    let svc = krb_cache(&dir, "svc.cc", "-f", "tester", &["cifs/dc.rsmb.test"]);
    let (kdc, server) = ("connected to rsmb.test:88", "connected to dc.rsmb.test:445");
    let args = ["--proxy", "socks5://10.213.45.20:1080", "-H", DC_NAME];
    for (cache, kdc_more) in [(&tgt, 1), (&svc, 0)] {
        let (k0, s0) = (proxy_lines(kdc), proxy_lines(server));
        let o = run_k(&args, cache, "/dev/null");
        assert_eq!(o.code, 0, "{}", o.stdout);
        expect_proxy_lines(server, s0 + 1, &format!("{}: the server", cache.display()));
        expect_proxy_lines(kdc, k0 + kdc_more, &format!("{}: the KDC", cache.display()));
    }
}

// ---------------------------------------------------------------------------------------------
// Stage 6: access (docs/plans/stage-6.md 7.4)

/// X81: `access` at the slow endpoint: one round trip for a path, two for `-e` of a small
/// directory, the probes of 2,000 entries 256 wide.
#[test]
fn timing_access() {
    let _s = serial();
    let before = drops();
    tapped_cost(&[], "access upload", 5, 20, 5);
    let mut c = Client::direct(SLOW);
    c.cmd("use share").expect_ok("use share");
    let mut median = |line: &str, n: usize| -> f64 {
        let mut v: Vec<f64> = (0..n)
            .map(|_| {
                let r = c.cmd(line);
                r.expect_ok(line);
                r.took.as_secs_f64() * 1e3
            })
            .collect();
        v.sort_by(f64::total_cmp);
        v[n / 2]
    };
    let one = median("access upload", 5);
    let tree = median("access -e /tree", 5);
    let fanout = median("access -e /fanout", 3);
    println!(
        "access at slow (100 ms each way): upload {one:.1} ms, -e /tree {tree:.1} ms, -e /fanout \
         {fanout:.0} ms"
    );
    assert!(
        one < 260.0,
        "access upload: {one:.1} ms, expected under 260"
    );
    assert!(
        tree < 500.0,
        "access -e /tree: {tree:.1} ms, expected under 500"
    );
    assert!(
        fanout < 4000.0,
        "access -e /fanout: {fanout:.0} ms, expected under 4 s"
    );
    assert_eq!(drops(), before, "netem dropped packets during the timing");
}
