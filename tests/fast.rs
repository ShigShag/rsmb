//! Fast endpoint, safe to run in parallel (docs/testing.md). Rows of docs/plans/stage-1.md 6.4.
//! `./test.sh` runs this binary twice: direct to `samba`, and with RSMB_TEST_VIA=socks5 through
//! the proxy to the private server (`fast()`; docs/plans/stage-4.md 6.1). A test names `fast()`,
//! never an address.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use serde_json::Value;

/// The route and login of `fast()` (direct, or through the proxy with RSMB_TEST_VIA=socks5),
/// then `extra`.
fn fast_args(extra: &[&str]) -> Vec<String> {
    let mut v = fast().args(None);
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

/// The container that serves `fast()`.
fn samba() -> &'static str {
    fast().container()
}

/// The prompt at the root of `share` on a tapped session: `//127.0.0.1/share/> ` direct.
fn p1() -> String {
    format!("//{}/share/> ", fast().host(true))
}

fn run_fast(extra: &[&str], stdin: Option<&str>) -> Output {
    let args = fast_args(extra);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&a, stdin)
}

fn error_kind(o: &Output) -> String {
    let v = o.json();
    let e = v
        .iter()
        .find(|v| v["type"] == "error")
        .unwrap_or_else(|| panic!("no error object: {}", o.stdout));
    e["kind"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------------------------
// T0, T1: skeleton, connect

#[test]
fn help_all_needs_no_server() {
    let o = run(&["--help-all"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for name in [
        "shares", "use", "cd", "pwd", "ls", "tree", "find", "get", "put", "cat", "mkdir", "rm",
        "rmdir", "lcd", "lpwd", "lls", "refresh", "help", "exit", "access",
    ] {
        assert!(o.stdout.contains(&format!("\n{name}")), "{name} missing");
    }
    let j = run(&["--help-all", "--json"], None);
    assert_eq!(j.code, 0);
    let objs = j.json();
    let names: BTreeSet<&str> = objs
        .iter()
        .filter(|v| v["type"] == "command")
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 20, "{names:?}");
}

/// A flag error is one JSON `error` object of kind `usage` with --json, clap's text without it.
#[test]
fn flag_errors_in_json() {
    for args in [
        vec!["--json", "--bogus"],
        vec!["--bogus", "--json"],
        vec!["--json"], // -H missing
        vec!["--json", "-H", "h", "--timeout", "0"],
    ] {
        let o = run(&args, None);
        assert_eq!(o.code, 2, "{args:?}: {}", o.stderr);
        assert_eq!(o.stderr, "", "{args:?}: stderr must stay empty");
        let v = o.json();
        assert_eq!(v.len(), 1, "{args:?}: {:?}", o.stdout);
        assert_eq!(v[0]["type"], "error", "{args:?}");
        assert_eq!(v[0]["kind"], "usage", "{args:?}");
        assert_eq!(v[0]["command"], Value::Null, "{args:?}");
        assert!(v[0]["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    let o = run(&["--bogus"], None);
    assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{}", o.stderr);
    assert!(o.stderr.contains("--bogus"), "{}", o.stderr);
    let o = run(&["--json", "--version"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.starts_with("rsmb "), "{:?}", o.stdout);
}

#[test]
fn closed_stdout_is_not_a_panic() {
    // The read end is closed before rsmb starts, so every write fails, however little it writes
    // (the manual is 33 KB, less than a pipe buffer: closing after the spawn would be a race).
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = Command::new(BIN)
        .arg("--help-all")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(101), "{err}");
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

/// A stdout that goes away ends the run, `--keep-going` or not: no more commands are sent to the server.
#[test]
fn closed_stdout_ends_a_keep_going_run() {
    use std::io::{BufRead, BufReader};
    let tap = fast().tap();
    let route = fast().args(Some(&tap));
    let mut args: Vec<&str> = route.iter().map(String::as_str).collect();
    args.extend(["--keep-going", "-C", "use share"]);
    args.extend([
        "-C", "ls /many", "-C", "ls /many", "-C", "ls /many", "-C", "pwd",
    ]);
    let mut child = Command::new(BIN)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap(); // and the reader is gone
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(err.matches("cannot write to stdout").count(), 1, "{err}");
    assert!(tap.settle(Duration::from_millis(300), WAIT));
    let s = tap.stats();
    assert_eq!(
        count(&s, "CREATE"),
        1,
        "expected one listing before the run ended\n{}",
        log(&s)
    );
}

#[test]
fn startup_is_three_requests() {
    let c = Client::tapped(fast());
    let s = c.startup.stats();
    c.startup
        .expect("start-up on fast: NEGOTIATE 1, SESSION_SETUP 2", 3, 3);
    assert_eq!(count(s, "NEGOTIATE"), 1);
    assert_eq!(count(s, "SESSION_SETUP"), 2);
    assert_eq!(s.connections, 1, "start-up on fast: expected 1 connection");
}

#[test]
fn logins() {
    let ok = run_fast(&["--json", "-C", "use share", "-C", "ls"], None);
    assert_eq!(ok.code, 0, "{}", ok.stdout);
    let route = fast().route(None);
    let with = |extra: &[&str]| {
        let mut a: Vec<&str> = route.iter().map(String::as_str).collect();
        a.push("--json");
        a.extend(extra);
        run(&a, None)
    };
    let o = with(&[
        "-u",
        "TESTDOM\\tester",
        "-p",
        "testpass",
        "-C",
        "use share",
        "-C",
        "ls",
    ]);
    assert_eq!(o.code, 0, "DOMAIN\\user without -d: {}", o.stdout);
    for login in [&["-u", "Guest", "--no-pass"][..], &[][..]] {
        let mut a = login.to_vec();
        a.extend(["-C", "use guest", "-C", "ls"]);
        let o = with(&a);
        assert_eq!(o.code, 0, "{login:?}: {}", o.stdout);
        assert!(
            o.stdout.contains("\"hello.txt\""),
            "{login:?}: {}",
            o.stdout
        );
        let mut a = login.to_vec();
        a.extend(["-C", "use share"]);
        let o = with(&a);
        assert_eq!(o.code, 1, "{login:?} on share: {}", o.stdout);
        assert_eq!(error_kind(&o), "access_denied", "{login:?}");
    }
    let o = with(&["-u", "tester", "-p", "wrong", "-d", "TESTDOM", "-C", "pwd"]);
    assert_eq!(o.code, 3, "wrong password: {}", o.stdout);
    assert_eq!(error_kind(&o), "auth");
    let o = with(&["-u", "nosuchuser", "-p", "x", "-C", "pwd"]);
    assert_eq!(o.code, 3, "unknown user: {}", o.stdout);
    assert!(o.stdout.contains("-u Guest"), "{}", o.stdout);
}

#[test]
fn no_password_no_prompt() {
    let route = fast().route(None);
    let mut args: Vec<&str> = route.iter().map(String::as_str).collect();
    args.extend(["-u", "tester", "-d", "TESTDOM", "--json", "-C", "pwd"]);
    let o = run(&args, None);
    assert_eq!(o.code, 2, "{}", o.stdout);
    assert!(o.took < Duration::from_secs(2), "took {:?}", o.took);
    assert_eq!(error_kind(&o), "usage");
    // On a terminal, but with stdin not one: still no prompt.
    let dir = tmp("no_password_no_prompt");
    let line = format!(
        "{BIN} {} -u tester -d TESTDOM -C pwd < /dev/null",
        route.join(" ")
    );
    let mut pty = Pty::command("sh", &["-c", &line], &dir.join("history"), &[]);
    let rest = pty.rest(Duration::from_millis(1500));
    // The usage error itself says "no password", so the check is for the prompt's own text.
    assert!(!rest.contains("Password for"), "prompted: {rest:?}");
    assert!(
        rest.contains("no password: give -p or --no-pass"),
        "{rest:?}"
    );
    assert_eq!(pty.exit_code(Duration::from_secs(5)), 2);
}

#[test]
fn ways_to_give_commands() {
    let lines = ["use share", "pwd", "cd tree", "pwd", "cd d0", "pwd"];
    let mut c = Vec::new();
    for l in lines {
        c.extend(["-C", l]);
    }
    let a = run_fast(&c, None);
    assert_eq!(a.code, 0, "{}", a.stderr);
    let dir = tmp("ways_to_give_commands");
    let script = dir.join("script");
    std::fs::write(&script, lines.join("\n")).unwrap();
    let b = run_fast(&["-S", script.to_str().unwrap()], None);
    let piped = run_fast(&[], Some(&format!("{}\n", lines.join("\n"))));
    assert_eq!(a.stdout.lines().count(), 3, "{}", a.stdout);
    assert_eq!(a.stdout, b.stdout, "-S");
    assert_eq!(a.stdout, piped.stdout, "stdin");
    assert!(a.stdout.ends_with("/share/tree/d0\n"), "{}", a.stdout);
    // -S runs before -C
    std::fs::write(&script, "use share\ncd tree\n").unwrap();
    let o = run_fast(&["-S", script.to_str().unwrap(), "-C", "pwd"], None);
    assert!(o.stdout.ends_with("/share/tree\n"), "{}", o.stdout);
    // With -C, a stdin pipe that is never closed is not read.
    let args = fast_args(&["-C", "use share", "-C", "pwd"]);
    let mut child = Command::new(BIN)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _keep_open = child.stdin.take();
    let t0 = std::time::Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert!(st.success());
            break;
        }
        if t0.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("with -C and an open stdin pipe rsmb did not exit within 5 s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn startup_failures() {
    let o = run(
        &[
            "-H",
            "127.0.0.1",
            "-P",
            "1",
            "--no-pass",
            "--json",
            "-C",
            "pwd",
        ],
        None,
    );
    assert_eq!(o.code, 3, "{}", o.stdout);
    assert_eq!(error_kind(&o), "connect");
    assert!(o.took < Duration::from_secs(2), "took {:?}", o.took);
    let objs = o.json();
    assert!(
        objs.iter().all(|v| v["type"] != "done"),
        "no done after a failed start-up"
    );
    assert_eq!(objs[0]["command"], Value::Null);
    // Built here, not from the route: through the proxy the route holds a --proxy already, and
    // clap's refusal of a second one has the same exit code (docs/plans/stage-4.md, finding 4).
    for json in [false, true] {
        let mut a = vec![
            "--proxy",
            "http://x",
            "-H",
            PRIVATE,
            "--no-pass",
            "-C",
            "pwd",
        ];
        if json {
            a.push("--json");
        }
        let o = run(&a, None);
        assert_eq!(o.code, 2, "{}", o.stderr);
        let text = if json { &o.stdout } else { &o.stderr };
        assert!(text.contains("expected socks5://"), "{text}");
        if json {
            assert_eq!(error_kind(&o), "usage");
        }
    }
    let o = run_fast(&["-S", "/nonexistent", "-C", "pwd"], None);
    assert_eq!(o.code, 2, "{}", o.stderr);
}

/// A start-up flag error with `--json` is a JSON `error`: for an argument that is not UTF-8 (it
/// used to panic, exit 101, in the code that looks for `--json`) and for a missing `-H`, whose
/// message must name the argument (final review, finding 3 and 13).
#[test]
fn startup_usage_errors() {
    use std::os::unix::ffi::OsStrExt;
    let bad = std::ffi::OsStr::from_bytes(b"\xff");
    let out = Command::new(BIN)
        .args(["-H", "h", "--no-pass", "-C"])
        .arg(bad)
        .arg("--json")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    let line = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(
        (v["type"].as_str(), v["kind"].as_str()),
        (Some("error"), Some("usage")),
        "{line}"
    );
    let o = run(&["--no-pass", "--json", "-C", "pwd"], None);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert_eq!(error_kind(&o), "usage");
    assert!(o.stdout.contains("--host"), "{}", o.stdout);
}

// ---------------------------------------------------------------------------------------------
// T2: tokeniser, table, help

fn manual_json() -> Vec<Value> {
    let o = run(&["--help-all", "--json"], None);
    assert_eq!(o.code, 0);
    o.json()
}

#[test]
fn docs_match_the_table() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/commands.md");
    let doc = std::fs::read_to_string(path).unwrap();
    let manual = manual_json();
    let mut names = BTreeSet::new();
    for c in manual.iter().filter(|v| v["type"] == "command") {
        let name = c["name"].as_str().unwrap();
        names.insert(name.to_string());
        let heading = format!("## `{name}`");
        assert!(
            doc.lines().any(|l| l == heading),
            "docs/commands.md has no line '{heading}'"
        );
        let syntax = c["syntax"].as_str().unwrap();
        assert!(
            doc.contains(syntax),
            "docs/commands.md lacks the syntax '{syntax}'"
        );
        for e in c["examples"].as_array().unwrap() {
            let line = e["line"].as_str().unwrap();
            assert!(
                doc.contains(line),
                "docs/commands.md lacks the example '{line}'"
            );
        }
    }
    for l in doc.lines() {
        if let Some(n) = l.strip_prefix("## `").and_then(|r| r.strip_suffix('`')) {
            assert!(
                names.contains(n),
                "docs/commands.md has a section for unknown command {n}"
            );
        }
    }
    for o in manual.iter().filter(|v| v["type"] == "option") {
        let flag = o["flag"].as_str().unwrap();
        let long = flag.split(' ').find(|w| w.starts_with("--")).unwrap();
        let long = long.trim_end_matches(',');
        assert!(doc.contains(long), "docs/commands.md does not name {long}");
        if let Some((_, d)) = o["help"].as_str().unwrap().split_once("[default: ") {
            let d = format!("default {}", d.trim_end_matches(']'));
            assert!(doc.contains(&d), "docs/commands.md lacks '{d}' for {long}");
        }
    }
    // The section "Proxy" holds every message of a proxy failure (docs/plans/stage-4.md 5.3).
    let proxy = doc
        .split("\n## Proxy\n")
        .nth(1)
        .and_then(|r| r.split("\n## ").next())
        .expect("docs/commands.md has a section '## Proxy'");
    for m in [
        "--proxy: expected socks5://[user:pass@]host:port (SOCKS5 only",
        "no answer within 10 s",
        "connected to the proxy, but the SOCKS5 handshake did not finish within 10 s",
        "user name or password refused",
        "wants a user name and password",
        "accepts neither a session without login nor a user name and password",
        "not a SOCKS5 answer",
        "closed the connection during the SOCKS5 handshake",
        "cannot reach <server>:<port>: ",
        "the proxy reports a general failure",
        "fallback",
    ] {
        assert!(proxy.contains(m), "the section Proxy lacks {m:?}");
    }
    // The section "Kerberos" holds every message of a Kerberos failure (docs/plans/stage-5.md,
    // H2); the unit tests assert the same words in the code.
    let kerberos = doc
        .split("\n## Kerberos\n")
        .nth(1)
        .and_then(|r| r.split("\n## ").next())
        .expect("docs/commands.md has a section '## Kerberos'");
    for m in [
        "names a ticket cache of type",
        "is not a ticket cache rsmb can read",
        "belongs to uid",
        "own user only",
        "more than one name part",
        "not to '",
        "expired at",
        "holds no ticket for cifs/",
        "KRB5CCNAME is not set, so the default file",
        "asking the KDC",
        "connected to the KDC, but it did not answer within 10 s",
        "closed the connection without an answer",
        "has no service cifs/",
        "KDC_ERR_BADOPTION",
        "kinit -f",
        "kvno cifs/",
        "the ticket-granting ticket has expired",
        "clocks",
        "KRB_AP_ERR_BADMATCH",
        "KRB_AP_ERR_MODIFIED",
        "the KDC's answer",
        "does not decrypt",
        "KDC error",
        "so the realm's name was tried",
        "did not accept the ticket",
        "could not be used",
        "ended the session",
        "`auth`",
        "`connect`",
    ] {
        assert!(kerberos.contains(m), "the section Kerberos lacks {m:?}");
    }
    // The exit codes: the manual, commands.md and scripting.md say the same.
    let scripting =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/scripting.md")).unwrap();
    let codes: Vec<&Value> = manual.iter().filter(|v| v["type"] == "exit_code").collect();
    assert_eq!(codes.len(), 5);
    for c in codes {
        let row = format!("| {} | {} |", c["code"], c["meaning"].as_str().unwrap());
        assert!(doc.contains(&row), "docs/commands.md lacks the row {row}");
        assert!(
            scripting.contains(&row),
            "docs/scripting.md lacks the row {row}"
        );
    }
}

#[test]
fn help_in_a_session() {
    let mut c = Client::tapped(fast());
    for line in ["help", "help cd", "help all"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        r.expect(line, 0, 0);
    }
    let r = c.cmd("help all");
    assert_eq!(
        r.lines,
        manual_json(),
        "help all differs from --help-all --json"
    );
    let r = c.cmd("help nosuch");
    assert_eq!(r.kind().as_deref(), Some("usage"));
}

#[test]
fn every_json_line_has_a_type() {
    let types: BTreeSet<String> = manual_json()
        .iter()
        .filter(|v| v["type"] == "json_object")
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    let dir = tmp("every_json_line_has_a_type");
    let s = Scratch::new(fast());
    std::fs::create_dir_all(dir.join("up/sub")).unwrap();
    std::fs::write(dir.join("up/a.txt"), b"a").unwrap();
    std::os::unix::fs::symlink(dir.join("up/a.txt"), dir.join("up/link")).unwrap();
    let lcd = format!("lcd {}", q(&dir.to_string_lossy()));
    let cd = format!("cd {}", s.remote());
    let lines = [
        "lpwd",
        &lcd,
        "lls",
        "lls /nonexistent",
        "lcd /nonexistent",
        "shares",
        "use share",
        "cd tree",
        "pwd",
        "ls",
        "refresh",
        "help",
        "help cd",
        "help all",
        "cd nope",
        "# a comment",
        "",
        "ls /forbidden",
        "nosuch",
        "get /tree/d0/file.txt",
        "get /tree/d0/file.txt",
        "get -r /loops",
        "find /loops",
        "tree -L 2 /tree",
        "find /nope",
        "tree /nope",
        "cat /tree/d0/file.txt",
        "cat /nope",
        &cd,
        "put file.txt",
        "put file.txt",
        "put -r up",
        "mkdir d",
        "mkdir d",
        "rm file.txt",
        "rm nope",
        "rmdir d",
        "rmdir nope",
        "exit",
    ];
    let o = run_fast(
        &["--json", "--keep-going"],
        Some(&format!("{}\n", lines.join("\n"))),
    );
    assert!(o.stderr.is_empty(), "stderr: {}", o.stderr);
    let objs = o.json();
    let mut dones = Vec::new();
    for v in &objs {
        let t = v["type"].as_str().unwrap_or_else(|| panic!("no type: {v}"));
        assert!(types.contains(t), "type {t} is not in the json_object list");
        if t == "done" {
            dones.push(v["command"].as_str().unwrap().to_string());
        }
        if let Some(p) = v.get("path") {
            assert!(p.is_string(), "path is a string: {v}");
        }
        if let Some(l) = v.get("left_behind") {
            assert!(
                l.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
                "{v}"
            );
        }
    }
    let run_lines: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(dones, run_lines, "one done per command line, in order");
    assert_eq!(objs.last().unwrap()["type"], "done");
    for t in [
        "share",
        "entry",
        "pwd",
        "error",
        "command",
        "option",
        "exit_code",
        "topic",
        "json_object",
        "transfer",
        "summary",
        "content",
        "local_entry",
        "lpwd",
        "denied",
        "skipped",
        "walk_summary",
    ] {
        assert!(
            objs.iter().any(|v| v["type"] == t),
            "no {t} object was printed"
        );
    }
    assert!(
        objs.iter()
            .any(|v| v["type"] == "error" && v["path"].is_string()),
        "no item error with a path"
    );
    let failed: BTreeSet<&str> = objs
        .iter()
        .filter(|v| v["type"] == "done" && v["ok"] == false)
        .map(|v| v["command"].as_str().unwrap())
        .collect();
    for l in [
        "get /tree/d0/file.txt",
        "put file.txt",
        "mkdir d",
        "rm nope",
        "rmdir nope",
        "cat /nope",
        "lcd /nonexistent",
        "lls /nonexistent",
        "put -r up",
        "get -r /loops",
        "find /nope",
        "tree /nope",
    ] {
        assert!(failed.contains(l), "a failing {l} was expected");
    }
}

// ---------------------------------------------------------------------------------------------
// T3: use, cd

#[test]
fn use_once_per_share() {
    check_use(fast());
}

#[test]
fn free_commands_send_nothing() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    c.cmd("cd tree/d0/d1").expect_ok("cd tree/d0/d1");
    for line in [
        "cd ..", "cd /tree", "cd .", "cd /", "cd", "pwd", "help", "help cd", "help all", "refresh",
    ] {
        let r = c.cmd(line);
        r.expect_ok(line);
        r.expect(line, 0, 0);
    }
}

#[test]
fn cd_is_one_compound() {
    check_cd(fast());
}

#[test]
fn cd_answers() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    for (dir, kind) in [
        ("nope", Some("not_found")),
        ("sizes/1.bin", Some("not_a_directory")),
        ("forbidden", Some("access_denied")),
        ("traverse-only", None),
    ] {
        let line = format!("cd {dir}");
        let r = c.cmd(&line);
        r.expect(&line, 2, 1);
        assert_eq!(r.kind().as_deref(), kind, "{line}");
        let p = c.cmd("pwd");
        let want = if kind.is_some() {
            "/".to_string()
        } else {
            format!("/{dir}")
        };
        assert_eq!(p.of("pwd")[0]["path"], want.as_str(), "{line}");
    }
}

#[test]
fn stop_at_first_failure() {
    let o = run_fast(&["-C", "use share", "-C", "cd nope", "-C", "pwd"], None);
    assert_eq!(o.code, 1);
    assert!(!o.stdout.contains("//"), "pwd ran: {}", o.stdout);
    let o = run_fast(
        &[
            "--keep-going",
            "-C",
            "use share",
            "-C",
            "cd nope",
            "-C",
            "pwd",
        ],
        None,
    );
    assert_eq!(o.code, 1);
    assert!(
        o.stdout.contains("/share/"),
        "pwd missing with --keep-going: {}",
        o.stdout
    );
    let o = run_fast(&["--json", "-C", "nosuch"], None);
    assert_eq!(o.code, 2);
    assert_eq!(error_kind(&o), "usage");
    let dir = tmp("stop_at_first_failure");
    let args = fast_args(&["-N"]);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let pty = Pty::start(&a, &dir.join("history"));
    assert_eq!(
        pty.exit_code(Duration::from_secs(5)),
        2,
        "-N alone on a terminal"
    );
}

// ---------------------------------------------------------------------------------------------
// T4: listing

/// `[A-Z0-9_]{6}~[A-Z0-9](\.[A-Z0-9]{1,3})?`
fn is_mangled(n: &str) -> bool {
    let ok = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    let (stem, ext) = match n.split_once('.') {
        Some((s, e)) => (s, Some(e)),
        None => (n, None),
    };
    let s: Vec<char> = stem.chars().collect();
    s.len() == 8
        && s[..6].iter().all(|&c| ok(c) || c == '_')
        && s[6] == '~'
        && ok(s[7])
        && ext.is_none_or(|e| (1..=3).contains(&e.len()) && e.chars().all(ok))
}

fn rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && s.chars().filter(char::is_ascii_digit).count() == 14
}

#[test]
fn ls_equals_the_server() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    for dir in ["sizes", "many", "longnames", "cjk", "special"] {
        let r = check_ls(&mut c, fast(), dir);
        expect_samba_ls(&r, &format!("ls {dir} on fast"));
        for e in r.of("entry") {
            let name = e["name"].as_str().unwrap();
            assert_eq!(e["path"], format!("/{dir}/{name}").as_str());
            assert!(
                e["file_id"]
                    .as_str()
                    .unwrap()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            );
            assert!(rfc3339(e["mtime"].as_str().unwrap()), "{e}");
            if dir == "sizes" {
                let n: u64 = name.strip_suffix(".bin").unwrap().parse().unwrap();
                assert_eq!(e["size"], n, "{name}");
            }
            let dir_entry = matches!(
                (dir, name),
                ("cjk", "nested") | ("special", "dir with space")
            );
            if dir_entry {
                assert_eq!(e["is_dir"], true, "{name}");
            }
            if name.ends_with(".txt") || name.ends_with(".bin") {
                assert_eq!(e["is_dir"], false, "{name}");
            }
        }
    }
    let r = c.cmd("ls mangled");
    expect_samba_ls(&r, "ls mangled on fast");
    let names: Vec<&str> = r
        .of("entry")
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "{names:?}");
    for n in names {
        assert!(is_mangled(n), "{n} is not a mangled 8.3 name");
    }
}

#[test]
fn ls_text() {
    let o = run_fast(&["-C", "use share", "-C", "ls sizes"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.is_empty(), "{}", o.stderr);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(lines.len(), 11, "{}", o.stdout);
    let mut names = Vec::new();
    for l in &lines {
        // "-" + 13 columns of size, two spaces, "YYYY-MM-DD HH:MM:SS", two spaces, the name
        assert!(l.starts_with('-'), "{l}");
        let size: u64 = l[1..14].trim().parse().unwrap();
        assert_eq!(&l[14..16], "  ");
        assert_eq!(&l[16..17], "2");
        assert_eq!((&l[20..21], &l[26..27], &l[29..30]), ("-", " ", ":"), "{l}");
        assert_eq!(&l[35..37], "  ");
        let name = &l[37..];
        assert_eq!(name, format!("{size}.bin"));
        names.push(name);
    }
    assert!(names.is_sorted(), "{names:?}");
    let o = run_fast(&["-C", "use share", "-C", "ls cjk"], None);
    assert!(
        o.stdout
            .lines()
            .any(|l| l.starts_with('d') && l.ends_with("  nested/")),
        "{}",
        o.stdout
    );
}

#[test]
fn ls_denied() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    for dir in ["forbidden", "traverse-only"] {
        let r = c.cmd(&format!("ls {dir}"));
        assert_eq!(r.kind().as_deref(), Some("access_denied"), "ls {dir}");
    }
    let r = check_ls(&mut c, fast(), "sizes");
    expect_samba_ls(&r, "ls sizes after a denied listing");
    let o = run_fast(
        &["-C", "use share", "-C", "ls forbidden", "-C", "pwd"],
        None,
    );
    assert_eq!(o.code, 1);
    assert!(!o.stdout.contains("//"), "pwd ran: {}", o.stdout);
}

#[test]
fn file_ids_follow_links() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    let id = |r: &Reply, name: &str| {
        r.of("entry")
            .iter()
            .find(|v| v["name"] == name)
            .map(|v| v["file_id"].as_str().unwrap().to_string())
    };
    let root = c.cmd("ls /");
    let loops = c.cmd("ls /loops");
    let loops_id = id(&root, "loops").unwrap();
    assert_eq!(
        id(&loops, "self").unwrap(),
        loops_id,
        "self has the id of loops"
    );
    let up = id(&loops, "up").unwrap();
    assert_ne!(up, "0");
    assert_ne!(up, loops_id);
    assert!(id(&loops, "dangling").is_none(), "dangling is not listed");
}

#[test]
fn encrypted_share() {
    let mut c = Client::tapped(fast());
    let r = c.cmd("use encrypted");
    r.expect_ok("use encrypted");
    r.expect("use encrypted", 1, 1);
    let r = c.cmd("cd tree");
    r.expect_ok("cd tree");
    let s = r.stats();
    assert_eq!(
        (s.requests, s.opaque_requests, s.flights),
        (0, 1, 1),
        "cd tree on encrypted: {s:?}"
    );
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls /sizes");
    assert_eq!(r.names(), server_names(samba(), "sizes"));
    let s = r.stats();
    assert_eq!(
        (s.requests, s.opaque_requests, s.flights),
        (0, 1, 1),
        "ls on encrypted: {s:?}"
    );
}

/// Every example in a fresh session and a fresh local directory holding notes.txt and photos/;
/// the examples that change the server run in a fresh scratch directory holding old.txt and
/// olddir/ (the context `help all` states).
#[test]
fn examples_run() {
    let manual = manual_json();
    let mut n = 0;
    for c in manual.iter().filter(|v| v["type"] == "command") {
        let name = c["name"].as_str().unwrap();
        for e in c["examples"].as_array().unwrap() {
            let line = e["line"].as_str().unwrap();
            let dir = tmp(&format!("examples_run_{n}"));
            std::fs::write(dir.join("notes.txt"), b"notes\n").unwrap();
            std::fs::create_dir(dir.join("photos")).unwrap();
            std::fs::write(dir.join("photos/a.jpg"), b"a").unwrap();
            std::fs::write(dir.join("photos/b.jpg"), b"b").unwrap();
            let scratch = matches!(name, "get" | "put" | "mkdir" | "rm" | "rmdir").then(|| {
                let s = Scratch::new(fast());
                s.sh("echo old > old.txt && mkdir olddir");
                s
            });
            let mut args = fast_args(&["--json", "-C", "use share"]);
            if let Some(s) = &scratch {
                args.extend(["-C".to_string(), format!("cd {}", s.remote())]);
            }
            args.extend(["-C".to_string(), line.to_string()]);
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let home = dir.to_string_lossy().to_string();
            let o = run_in(&dir, &a, None, &[("HOME", &home)]);
            assert_eq!(o.code, 0, "example '{line}': {}", o.stdout);
            let last = o.json().pop().unwrap();
            assert_eq!(last["type"], "done", "{line}");
            assert_eq!(last["ok"], true, "{line}");
            assert_eq!(last["command"], line);
            n += 1;
        }
    }
    assert!(n >= 39, "{n} examples");
}

// ---------------------------------------------------------------------------------------------
// T5: cache, refresh, reconnect

#[test]
fn reconnect_after_idle_cut() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    c.cmd("cd tree/d0").expect_ok("cd tree/d0");
    assert_eq!(c.tap().drop_connections(), 1);
    // The cut is done when the tap holds no connection any more (not after a fixed time).
    let deadline = std::time::Instant::now() + WAIT;
    while c.tap().live_connections() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the tap still holds a connection"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for line in ["pwd", "help", "cd ..", "refresh"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        let s = r.stats();
        assert!(
            s.requests == 0 && s.connections == 0,
            "{line}: expected 0 requests and 0 new connections on a dead connection, got {} and {}",
            s.requests,
            s.connections
        );
        assert!(r.took < Duration::from_secs(1), "{line} took {:?}", r.took);
    }
    let a = c.cmd("cd d0");
    a.expect_ok("cd d0 after the cut");
    let b = c.cmd("ls");
    b.expect_ok("ls after the cut");
    assert_eq!(b.names(), server_names(samba(), "tree/d0"));
    assert_eq!(b.of("entry").len(), 5);
    let (sa, sb) = (a.stats(), b.stats());
    let mut per = sa.per_command.clone();
    for (k, v) in &sb.per_command {
        *per.entry(k).or_default() += v;
    }
    let want: std::collections::BTreeMap<&str, u64> = [
        ("NEGOTIATE", 1),
        ("SESSION_SETUP", 2),
        ("TREE_CONNECT", 1),
        ("CREATE", 2),
        ("CLOSE", 2),
        ("QUERY_DIRECTORY", 3),
    ]
    .into();
    assert!(
        sa.connections + sb.connections == 1 && per == want,
        "expected exactly 1 new connection and 11 requests {want:?}, got {} connections and {per:?}",
        sa.connections + sb.connections
    );
    let p = c.cmd("pwd");
    assert_eq!(p.of("pwd")[0]["path"], "/tree/d0");
    assert_eq!(p.of("pwd")[0]["share"], "share");
}

// ---------------------------------------------------------------------------------------------
// T6: shares

#[test]
fn shares_fast() {
    let mut c = Client::tapped(fast());
    let a = c.cmd("shares");
    a.expect_ok("shares");
    let names: BTreeSet<&str> = a
        .of("share")
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    for n in ["share", "readonly", "guest", "encrypted", "tiny", "IPC$"] {
        assert!(names.contains(n), "{n} missing: {names:?}");
    }
    let ipc = a
        .of("share")
        .into_iter()
        .find(|v| v["name"] == "IPC$")
        .unwrap();
    assert_eq!(ipc["share_type"], "ipc");
    assert_eq!(ipc["hidden"], true);
    let share = a
        .of("share")
        .into_iter()
        .find(|v| v["name"] == "share")
        .unwrap();
    assert_eq!(
        (share["share_type"].as_str(), share["hidden"].as_bool()),
        (Some("disk"), Some(false))
    );
    // The access column: the share-level access the server reports, one compound for all six.
    assert_eq!(
        access_of(&a),
        access_map(&[
            ("share", "rw"),
            ("readonly", "r"),
            ("guest", "r"),
            ("encrypted", "rw"),
            ("tiny", "rw"),
            ("IPC$", "r"),
        ])
    );
    a.expect("shares (first)", 19, 8);
    let b = c.cmd("shares");
    b.expect("shares (again)", 18, 7);
    assert_eq!(b.lines, a.lines);
    for (r, n) in [(&a, 7), (&b, 6)] {
        let s = r.stats();
        assert_eq!(
            count(s, "TREE_DISCONNECT"),
            6,
            "shares on fast: expected 6 TREE_DISCONNECT, one per share\n{}",
            context(s)
        );
        assert_eq!(
            count(s, "TREE_CONNECT"),
            n,
            "shares on fast: expected {n} TREE_CONNECT\n{}",
            context(s)
        );
    }
    // The probes joined nothing to the tree map: `use readonly` still costs its TREE_CONNECT.
    c.cmd("use readonly")
        .expect("use readonly after shares", 1, 1);
    // No access: an anonymous login is refused `share`, `readonly`, `encrypted` and `tiny`.
    let mut anon = Client::with(None, Some(fast()), &[]);
    let r = anon.cmd("shares");
    r.expect_ok("shares as anonymous");
    let want = access_map(&[
        ("share", "none"),
        ("readonly", "none"),
        ("guest", "r"),
        ("encrypted", "none"),
        ("tiny", "none"),
        ("IPC$", "r"),
    ]);
    assert_eq!(access_of(&r), want, "shares as anonymous");
    // Text: the column after the type.
    let route = fast().route(None);
    let mut args: Vec<&str> = route.iter().map(String::as_str).collect();
    args.extend(LOGIN);
    args.extend(["-C", "shares"]);
    let o = run(&args, None);
    assert_eq!(o.code, 0, "{}", o.stdout);
    for line in [
        "share                disk     rw",
        "readonly             disk     r-",
        "IPC$                 ipc      r-   IPC Service",
    ] {
        assert!(
            o.stdout.lines().any(|l| l.starts_with(line)),
            "no line starting {line:?} in\n{}",
            o.stdout
        );
    }
}

/// `access` of every `share` object, by name.
fn access_of(r: &Reply) -> BTreeMap<String, String> {
    r.of("share")
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap().into(),
                v["access"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn access_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// T7: the interactive shell

#[test]
fn interactive_basics() {
    let dir = tmp("interactive_basics");
    let hist = dir.join("history");
    let (mut p, tap, s) = Pty::tapped(fast(), &[], &hist);
    expect_stats(&s, "start-up", 3, 3);
    // The banner (P11): the proxy's address, never its user; nothing about a proxy direct.
    let banner = p.transcript();
    if fast().proxied() {
        let via = format!("through proxy 127.0.0.1:{}", tap.addr().port());
        assert!(banner.contains(&via), "{banner}");
    } else {
        assert!(banner.contains("logged in as"), "{banner}");
        assert!(!banner.contains("proxy"), "{banner}");
    }
    std::thread::sleep(Duration::from_secs(1));
    expect_stats(&take(&tap), "1 s at the first prompt", 0, 0);
    p.line("use share", &p1());
    expect_stats(&take(&tap), "use share", 1, 1);
    let tree = format!("//{}/share/tree> ", fast().host(true));
    p.line("cd tree", &tree);
    expect_stats(&take(&tap), "cd tree", 2, 1);
    p.send(b"\x1b[A");
    p.expect("> cd tree", WAIT);
    p.send(CTRL_U);
    p.send(b"xyz\x03");
    p.expect(&tree, WAIT);
    p.line("pwd", &tree);
    expect_stats(&take(&tap), "Up, Ctrl-C, pwd", 0, 0);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0, "exit");
    // A second process reads the history file.
    let (mut p, _tap, _) = Pty::tapped(fast(), &[], &hist);
    p.send(b"\x1b[A");
    p.expect("> exit", WAIT);
    p.send(CTRL_U);
    p.send(b"\x04");
    assert_eq!(p.exit_code(WAIT), 0, "Ctrl-D");
}

/// SIGTERM at the prompt, empty or with the text of a completed Tab in the line: exit 130, the
/// terminal is cooked again (`stty -a` after rsmb shows `icanon` and `echo`), the history has the
/// lines entered so far. A Tab whose request is still on the wire is `sigterm_in_a_tab_on_the_wire`
/// in `tests/slow.rs`: at this endpoint the answer comes too fast to hit.
#[test]
fn sigterm_in_the_shell() {
    for in_tab in [false, true] {
        let dir = tmp(&format!("sigterm_in_the_shell_{in_tab}"));
        let hist = dir.join("history");
        let mut script = format!(
            "{BIN} {}; echo EXIT=$?; stty -a",
            fast().args(None).join(" ")
        );
        script.push_str("; echo END");
        let mut p = Pty::command("sh", &["-c", &script], &hist, &[]);
        let prompt = format!("//{}> ", fast().host(false));
        p.expect(&prompt, WAIT);
        p.line("pwd", &prompt);
        if in_tab {
            p.send(b"use sh\t");
            p.expect("use share ", WAIT);
        }
        // ptyrun's child is the sh; rsmb is its child.
        let sh = p.rsmb_pid().to_string();
        let o = Command::new("pgrep").args(["-P", &sh]).output().unwrap();
        let pid = String::from_utf8(o.stdout).unwrap().trim().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        p.expect("END", WAIT);
        let t = p.transcript();
        assert!(
            t.contains("EXIT=130"),
            "in_tab {in_tab}: exit code; transcript:\n{t}"
        );
        let words: Vec<String> = t
            .split(|c: char| c.is_whitespace() || c == ';')
            .map(String::from)
            .collect();
        for w in ["icanon", "echo"] {
            assert!(
                words.contains(&w.to_string()) && !words.contains(&format!("-{w}")),
                "in_tab {in_tab}: the terminal is not back to {w}; transcript:\n{t}"
            );
        }
        let h = std::fs::read_to_string(&hist).unwrap_or_default();
        assert!(
            h.lines().any(|l| l == "pwd"),
            "in_tab {in_tab}: history {h:?}"
        );
    }
}

#[test]
fn password_prompt() {
    let dir = tmp("password_prompt");
    let h = fast().host(false);
    let route = fast().route(None);
    let mut args: Vec<&str> = route.iter().map(String::as_str).collect();
    args.extend(["-u", "tester", "-d", "TESTDOM"]);
    let mut p = Pty::start(&args, &dir.join("h"));
    p.expect("Password for TESTDOM\\tester: ", WAIT);
    p.send(b"testpass\r");
    let after = p.expect(&format!("//{h}> "), WAIT);
    assert!(
        !after.contains("testpass"),
        "the password was echoed: {after:?}"
    );
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

#[test]
fn names_through_the_shell() {
    let dir = tmp("names_through_the_shell");
    let h = fast().host(false);
    let route = fast().args(None);
    let args: Vec<&str> = route.iter().map(String::as_str).collect();
    let mut p = Pty::start(&args, &dir.join("history"));
    let root = format!("//{h}/share/");
    p.expect(&format!("//{h}> "), WAIT);
    p.line("use share", &root);
    let cjk = format!("{root}cjk> ");
    p.line("cd cjk", &cjk);
    for name in fixture_names("CJK_NAMES") {
        let mut part: Vec<char> = name.chars().collect();
        part.pop();
        let part: String = part.into_iter().collect();
        p.send(format!("cd {part}\t").as_bytes());
        p.expect(&format!("> cd {name}/"), WAIT);
        p.line("", &format!("{root}cjk/{name}> "));
        p.send(b"ls\r");
        p.expect(&format!("  {name}.txt\r\n"), WAIT);
        p.expect("> ", WAIT);
        p.line("cd ..", &cjk);
    }
    // The NFC and NFD Korean directories, from their first character.
    for var in ["KO_NFC", "KO_NFD"] {
        let name = fixture(var);
        let first = name.chars().next().unwrap();
        p.send(format!("cd {first}\t").as_bytes());
        p.expect(&format!("> cd {name}/"), WAIT);
        p.line("", &format!("{root}cjk/{name}> "));
        p.send(b"ls\r");
        p.expect(&format!("  {name}.txt\r\n"), WAIT);
        p.expect("> ", WAIT);
        p.line("cd ..", &cjk);
    }
    p.line("cd /special", &format!("{root}special> "));
    p.send(b"cd dir\t");
    p.expect("> cd dir\\ with\\ space/", WAIT);
    p.line("", &format!("{root}special/dir with space> "));
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

#[test]
fn completion_costs() {
    let dir = tmp("completion_costs");
    let (mut p, tap, _) = Pty::tapped(fast(), &[], &dir.join("history"));
    p.line("use share", &p1());
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "sh", Some("shares ")),
        "Tab on a command name",
        0,
        0,
    );
    check_tab_many(&mut p, &tap, fast());
    p.send(CTRL_U);
    p.line("refresh", &p1());
    expect_stats(&take(&tap), "refresh", 0, 0);
    expect_stats(
        &tab(&mut p, &tap, "ls many/f0999", None),
        "Tab after refresh",
        5,
        1,
    );
    p.send(CTRL_U);
    p.line("ls tree", &p1());
    take(&tap);
    expect_stats(&tab(&mut p, &tap, "cd tree/d", None), "Tab after ls", 0, 0);
    let s = tab(&mut p, &tap, "use rea", Some("use readonly "));
    expect_stats(&s, "first Tab after use (the share list)", 7, 7);
    expect_stats(
        &tab(&mut p, &tap, "use enc", Some("use encrypted ")),
        "second Tab after use",
        0,
        0,
    );
    p.send(CTRL_U);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

/// A listing that fails (traverse-only: enterable, not listable; a path that does not exist) is
/// asked for once per refresh; `ls` itself always asks and reports the error.
#[test]
fn failed_completion_is_remembered() {
    let dir = tmp("failed_completion_is_remembered");
    let (mut p, tap, _) = Pty::tapped(fast(), &[], &dir.join("history"));
    p.line("use share", &p1());
    take(&tap);
    for (name, line) in [
        ("traverse-only/", "cd traverse-only/k"),
        ("a missing path", "cd nowhere/k"),
    ] {
        let s = tab(&mut p, &tap, line, None);
        assert_eq!(s.flights, 1, "first Tab in {name}: {:?}", s.per_command);
        for n in ["second", "third"] {
            let s = tab(&mut p, &tap, line, None);
            expect_stats(&s, &format!("{n} Tab in {name}"), 0, 0);
        }
        p.send(CTRL_U);
        p.line("refresh", &p1());
        take(&tap);
        let s = tab(&mut p, &tap, line, None);
        assert_eq!(
            s.flights, 1,
            "Tab in {name} after refresh: {:?}",
            s.per_command
        );
    }
    // `ls` is never answered from what a failed Tab left behind.
    p.send(CTRL_U);
    take(&tap);
    p.send(b"ls traverse-only\r");
    p.expect("access denied", WAIT);
    p.expect(&p1(), WAIT);
    let s = take(&tap);
    assert_eq!(s.flights, 1, "ls traverse-only: {:?}", s.per_command);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

#[test]
fn idle_sends_nothing() {
    let dir = tmp("idle_sends_nothing");
    let (mut p, tap, _) = Pty::tapped(fast(), &[], &dir.join("history"));
    p.line("use share", &p1());
    take(&tap);
    std::thread::sleep(Duration::from_secs(12));
    let s = tap.stats();
    assert_eq!(
        s.requests + s.opaque_requests,
        0,
        "expected 0 requests in 12 s idle: {:?}",
        s.per_command
    );
    assert_eq!(s.connections, 0);
}

// ---------------------------------------------------------------------------------------------
// T8

/// F14. Requests add up exactly. Flights add up exactly for every line but the two recursive
/// transfers and the walks: with 16 requests in flight, how many sends follow an answer depends
/// on the order the answers arrive in (docs/plans/stage-2.md, deviations).
#[test]
fn counts_add_up() {
    let concurrent = |l: &str| {
        ["get -r", "put -r", "tree", "find"]
            .iter()
            .any(|p| l.starts_with(p))
    };
    let runs: Vec<_> = (0..3)
        .map(|i| (tmp(&format!("counts_add_up_{i}")), Scratch::new(fast())))
        .collect();
    let script = session_script(&runs[0].0, &runs[0].1.remote());
    let mut c = Client::tapped(fast());
    let (mut requests, mut flights) = (0, 0);
    for line in &script {
        let r = c.cmd(line);
        r.expect_ok(line);
        requests += r.stats().requests + r.stats().opaque_requests;
        if !concurrent(line) {
            flights += r.stats().flights;
        }
    }
    let streamed = |i: usize, keep: &dyn Fn(&str) -> bool| {
        let script = session_script(&runs[i].0, &runs[i].1.remote());
        let mut d = Client::tapped(fast());
        for line in script.iter().filter(|l| keep(l)) {
            d.send(line);
            let (_, ok) = d.read_done(WAIT);
            assert!(ok, "{line}");
        }
        take(d.tap())
    };
    let all = streamed(1, &|_| true);
    assert_eq!(
        requests,
        all.requests + all.opaque_requests,
        "the per-command requests do not add up to the session total {:?}",
        all.per_command
    );
    let seq = streamed(2, &|l| !concurrent(l));
    assert_eq!(
        flights, seq.flights,
        "the per-command flights do not add up to the session total {:?}",
        seq.per_command
    );
}

// ---------------------------------------------------------------------------------------------
// Stage 2: transfers and file operations (docs/plans/stage-2.md 6.4)

fn direct_in(dir: &std::path::Path, cd: Option<&str>) -> Client {
    ready(Client::to(fast(), &[]), dir, cd)
}

fn tapped_in(dir: &std::path::Path, cd: Option<&str>) -> Client {
    ready(Client::tapped(fast()), dir, cd)
}

/// sha256 of a file of the share, in the container.
fn sha_of(remote: &str) -> String {
    server_sha(samba(), &format!("/srv/share{remote}"))
}

fn no_temps(s: &Scratch, dir: &std::path::Path) {
    assert_eq!(
        temps_on_server(s.container, &s.server()),
        Vec::<String>::new(),
        "temp on the server"
    );
    assert_eq!(
        temps_local(dir),
        Vec::<std::path::PathBuf>::new(),
        "temp locally"
    );
}

fn kinds(r: &Reply) -> Vec<String> {
    r.of("error")
        .iter()
        .map(|v| v["kind"].as_str().unwrap().to_string())
        .collect()
}

fn per(r: &Reply) -> std::collections::BTreeMap<&'static str, u64> {
    r.stats().per_command.clone()
}

fn map(pairs: &[(&'static str, u64)]) -> std::collections::BTreeMap<&'static str, u64> {
    pairs.iter().copied().collect()
}

const SIZES: [u64; 11] = [
    0, 1, 65535, 65536, 65537, 1048575, 1048576, 1048577, 8388607, 8388608, 8388609,
];

/// F21
#[test]
fn get_put_every_size() {
    let dir = tmp("get_put_every_size");
    let s = Scratch::new(fast());
    let mut c = direct_in(&dir, Some(&s.remote()));
    for n in SIZES {
        let name = format!("{n}.bin");
        let r = c.cmd(&format!("get /sizes/{name}"));
        r.expect_ok(&name);
        assert_eq!(transfers(&r)[0]["bytes"], n, "{name}: bytes of get");
        let (want, got) = (
            sha_of(&format!("/sizes/{name}")),
            local_sha(&dir.join(&name)),
        );
        assert!(
            want == got,
            "sha256 of {name} differs: server {want}, result {got}"
        );
        let r = c.cmd(&format!("put {name}"));
        r.expect_ok(&format!("put {name}"));
        assert_eq!(transfers(&r)[0]["bytes"], n, "{name}: bytes of put");
        let up = server_sha(samba(), &format!("{}/{name}", s.server()));
        assert!(
            up == want,
            "sha256 of {name} differs: server {up}, source {want}"
        );
    }
    no_temps(&s, &dir);
}

/// F22
#[test]
fn threshold_boundary() {
    let dir = tmp("threshold_boundary");
    let s = Scratch::new(fast());
    let mut c = tapped_in(&dir, Some(&s.remote()));
    for (i, n) in [262_143u64, 262_144, 262_145].into_iter().enumerate() {
        let name = format!("t{n}.bin");
        make_file(&dir.join(&name), n, i as u8);
        let r = c.cmd(&format!("put {name}"));
        r.expect_ok(&name);
        let st = r.stats();
        if n <= 262_144 {
            r.expect(&format!("put {name} (the small path)"), 7, 3);
            assert_eq!(count(st, "WRITE"), 1, "{name}\n{}", context(st));
        } else {
            assert_eq!(
                (count(st, "CREATE"), count(st, "QUERY_INFO")),
                (3, 3),
                "put {name}: the stream path\n{}",
                context(st)
            );
        }
        let src = local_sha(&dir.join(&name));
        assert_eq!(
            server_sha(samba(), &format!("{}/{name}", s.server())),
            src,
            "{name}"
        );
        c.cmd(&format!("get {name} back-{name}"))
            .expect_ok("get back");
        assert_eq!(
            local_sha(&dir.join(format!("back-{name}"))),
            src,
            "{name} round trip"
        );
    }
    no_temps(&s, &dir);
}

/// F23
#[test]
fn transfer_costs() {
    let dir = tmp("transfer_costs");
    let s = Scratch::new(fast());
    make_file(&dir.join("small.bin"), 3000, 1);
    make_file(&dir.join("large.bin"), 1_048_577, 2);
    let mut c = tapped_in(&dir, Some(&s.remote()));
    let r = c.cmd("get /sizes/65537.bin");
    r.expect_ok("get 65537");
    r.expect("get /sizes/65537.bin: expected 7 requests (CREATE 1, QUERY_INFO 4, READ 1, CLOSE 1) in 3 flights", 7, 3);
    assert_eq!(
        per(&r),
        map(&[("CREATE", 1), ("QUERY_INFO", 4), ("READ", 1), ("CLOSE", 1)])
    );
    let r = c.cmd("get /sizes/1048577.bin");
    r.expect_ok("get 1048577");
    assert_eq!(count(r.stats(), "READ"), 3, "{}", context(r.stats()));
    assert_eq!(
        r.stats().requests,
        9,
        "get 1048577: expected 9 requests\n{}",
        context(r.stats())
    );
    let r = c.cmd("put small.bin");
    r.expect_ok("put small");
    r.expect("put of 3,000 bytes: expected 7 requests (CREATE 2, WRITE 1, FLUSH 1, CLOSE 2, SET_INFO 1) in 3 flights", 7, 3);
    assert_eq!(
        per(&r),
        map(&[
            ("CREATE", 2),
            ("WRITE", 1),
            ("FLUSH", 1),
            ("CLOSE", 2),
            ("SET_INFO", 1)
        ])
    );
    let large = map(&[
        ("CREATE", 3),
        ("QUERY_INFO", 3),
        ("WRITE", 3),
        ("FLUSH", 1),
        ("SET_INFO", 1),
        ("CLOSE", 3),
    ]);
    for line in ["put large.bin", "put -f large.bin"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        assert_eq!(
            per(&r),
            large,
            "{line}: expected 14 requests\n{}",
            context(r.stats())
        );
    }
    // /sizes/1.bin is one byte that is not UTF-8: with --json cat reads it, then refuses it.
    let r = c.cmd("cat /sizes/1.bin");
    assert_eq!(
        r.stats().requests,
        7,
        "cat /sizes/1.bin\n{}",
        context(r.stats())
    );
    c.cmd("cat /tree/d0/file.txt")
        .expect("cat /tree/d0/file.txt", 7, 3);
    for (line, req, fl) in [
        ("mkdir d", 2, 1),
        ("mkdir -p a/b/c", 6, 3),
        ("rm small.bin", 3, 1),
        ("rmdir a/b/c", 3, 1),
        ("lcd .", 0, 0),
        ("lpwd", 0, 0),
        ("lls", 0, 0),
    ] {
        let r = c.cmd(line);
        r.expect_ok(line);
        r.expect(line, req, fl);
    }
    let r = c.cmd("get /sizes/1.bin small.bin");
    assert_eq!(r.kind().as_deref(), Some("already_exists"));
    r.expect("a get refused because the local file exists", 0, 0);
    let r = c.cmd("put small.bin large.bin");
    assert_eq!(r.kind().as_deref(), Some("already_exists"));
    r.expect(
        "a small put refused: 7, the refused rename's extra CLOSE 1, the delete 3, the stat 2",
        13,
        6,
    );
    // -f onto an existing file replaces it: the same 7 requests in 3 flights as onto a new name
    // (the refused rename's extra CLOSE and the clean-up are what the refusal above costs).
    let r = c.cmd("put -f small.bin large.bin");
    r.expect_ok("put -f small.bin large.bin");
    r.expect("a small put with -f onto an existing file", 7, 3);
    let r = c.cmd("put -f large.bin d");
    assert_eq!(r.kind().as_deref(), Some("is_a_directory"));
    r.expect(
        "a large put onto a directory, with -f: the check only",
        2,
        1,
    );
    assert_eq!(count(r.stats(), "WRITE"), 0);
    no_temps(&s, &dir);
}

fn tree_without(container: &str, path: &str, drop: &[&str]) -> Tree {
    let (mut files, mut dirs) = server_tree(container, path);
    files.retain(|k, _| !drop.iter().any(|d| k.starts_with(d.as_bytes())));
    dirs.retain(|k| !drop.iter().any(|d| k.starts_with(d.as_bytes())));
    (files, dirs)
}

/// F24
#[test]
fn structure_round_trip() {
    let dir = tmp("structure_round_trip");
    let s = Scratch::new(fast());
    let mut c = direct_in(&dir, Some(&s.remote()));
    let r = c.cmd("get -r /tree");
    assert!(!r.ok, "get -r /tree must fail (two items are not readable)");
    let items = item_errors(&r);
    let mut paths: Vec<&str> = items.iter().map(|v| v["path"].as_str().unwrap()).collect();
    paths.sort();
    assert_eq!(
        paths,
        ["/tree/forbidden", "/tree/unreadable.txt"],
        "{:?}",
        r.lines
    );
    assert!(
        items.iter().all(|v| v["kind"] == "access_denied"),
        "{items:?}"
    );
    assert_eq!(r.of("summary")[0]["failed"], 2);
    let want = tree_without(
        samba(),
        "/srv/share/tree",
        &["./forbidden", "./unreadable.txt"],
    );
    assert_eq!(
        local_tree(&dir.join("tree")),
        want,
        "the local tree equals the server's"
    );
    for name in ["deep", "chain"] {
        let r = c.cmd(&format!("get -r /{name}"));
        r.expect_ok(&format!("get -r /{name}"));
        assert_eq!(
            local_tree(&dir.join(name)),
            server_tree(samba(), &format!("/srv/share/{name}"))
        );
    }
    for name in ["tree", "deep", "chain"] {
        let r = c.cmd(&format!("put -r {name}"));
        r.expect_ok(&format!("put -r {name}"));
        assert_eq!(
            server_tree(samba(), &format!("{}/{name}", s.server())),
            local_tree(&dir.join(name)),
            "{name} put back"
        );
    }
    // A tree with an empty directory, up and back.
    let made = dir.join("made");
    std::fs::create_dir_all(made.join("empty")).unwrap();
    std::fs::create_dir_all(made.join("full/deeper")).unwrap();
    make_file(&made.join("full/deeper/x.bin"), 300_000, 3);
    make_file(&made.join("top.txt"), 10, 4);
    c.cmd("put -r made").expect_ok("put -r made");
    std::fs::rename(&made, dir.join("made-orig")).unwrap();
    c.cmd("get -r made").expect_ok("get -r made");
    let back = local_tree(&made);
    assert_eq!(back, local_tree(&dir.join("made-orig")));
    assert!(
        back.1.contains(b"./empty".as_slice()),
        "the empty directory came back"
    );
    assert!(
        server_tree(samba(), &format!("{}/made", s.server()))
            .1
            .contains(b"./empty".as_slice())
    );
    // Again without -f: every file already_exists, nothing changed; with -f: ok.
    let before = server_tree(samba(), &format!("{}/made", s.server()));
    let r = c.cmd("put -r made");
    assert!(!r.ok);
    let items = item_errors(&r);
    assert_eq!(items.len(), 2, "{:?}", r.lines);
    assert!(
        items.iter().all(|v| v["kind"] == "already_exists"),
        "{items:?}"
    );
    assert_eq!(
        server_tree(samba(), &format!("{}/made", s.server())),
        before
    );
    c.cmd("put -r -f made").expect_ok("put -r -f made");
    // Leftovers (finding 1).
    let lo = dir.join("lo");
    std::fs::create_dir_all(&lo).unwrap();
    make_file(&lo.join("a.txt"), 100, 5);
    let leftover = fnv_temp("a.txt");
    make_file(&lo.join(&leftover), 50, 6);
    for line in ["put -r lo", "put -r -f lo"] {
        let r = c.cmd(line);
        assert!(!r.ok, "{line}");
        assert_eq!(kinds(&r)[..1], ["invalid_name"], "{line}: {:?}", r.lines);
        assert_eq!(item_errors(&r).len(), 1, "{line}");
        assert_eq!(
            server_sha(samba(), &format!("{}/lo/a.txt", s.server())),
            local_sha(&lo.join("a.txt"))
        );
        assert!(
            server_names(samba(), &format!("upload/{}/lo", s.name)).len() == 1,
            "{line}"
        );
    }
    s.sh(&format!(
        "mkdir mirror && echo full > mirror/a.txt && echo part > mirror/{leftover}"
    ));
    let r = c.cmd("get -r mirror");
    assert!(!r.ok);
    assert_eq!(item_errors(&r).len(), 1, "{:?}", r.lines);
    assert_eq!(item_errors(&r)[0]["kind"], "invalid_name");
    assert_eq!(std::fs::read(dir.join("mirror/a.txt")).unwrap(), b"full\n");
    assert!(!dir.join("mirror").join(&leftover).exists());
    let mut t = tapped_in(&dir, Some(&s.remote()));
    let r = t.cmd(&format!("get mirror/{leftover}"));
    assert_eq!(r.kind().as_deref(), Some("invalid_name"));
    r.expect("get of a leftover", 0, 0);
    let r = t.cmd(&format!("put lo/{leftover}"));
    assert_eq!(r.kind().as_deref(), Some("invalid_name"));
    r.expect("put of a leftover", 0, 0);
    t.cmd(&format!("cat mirror/{leftover}"))
        .expect_ok("cat a leftover");
    t.cmd(&format!("rm mirror/{leftover}"))
        .expect_ok("rm a leftover");
    no_temps(&s, &dir.join("tree"));
}

/// F25
#[test]
fn names_round_trip() {
    let dir = tmp("names_round_trip");
    let s = Scratch::new(fast());
    let mut c = direct_in(&dir, Some(&s.remote()));
    for name in ["cjk", "special", "longnames"] {
        c.cmd(&format!("get -r /{name}"))
            .expect_ok(&format!("get -r /{name}"));
        assert_eq!(
            local_tree(&dir.join(name)),
            server_tree(samba(), &format!("/srv/share/{name}")),
            "{name}: names and checksums"
        );
    }
    assert_eq!(
        std::fs::read(dir.join("special/Case.txt")).unwrap(),
        b"Case.txt\n"
    );
    assert_eq!(
        std::fs::read(dir.join("special/case.txt")).unwrap(),
        b"case.txt\n"
    );
    for name in ["cjk", "longnames"] {
        c.cmd(&format!("put -r {name}"))
            .expect_ok(&format!("put -r {name}"));
        assert_eq!(
            server_tree(samba(), &format!("{}/{name}", s.server())),
            local_tree(&dir.join(name)),
            "{name} put back"
        );
    }
    let r = c.cmd("put -r special");
    assert!(!r.ok);
    let items = item_errors(&r);
    assert_eq!(items.len(), 2, "{:?}", r.lines);
    assert!(items.iter().all(|v| v["kind"] == "invalid_name"));
    let (files, dirs) = server_tree(samba(), &format!("{}/special", s.server()));
    assert_eq!(
        files.len(),
        20,
        "the other 19 files and the leaf: {:?}",
        files.keys()
    );
    assert!(
        !files.keys().any(|k| k.ends_with(b"ase.txt")),
        "neither case file is on the server"
    );
    assert_eq!(dirs.len(), 5, "the directory chain");
    // rm and rmdir by name: the uploaded cjk tree, emptied.
    let base = dir.join("cjk");
    let (files, dirs) = local_tree(&base);
    for f in files.keys() {
        let p = String::from_utf8(f[2..].to_vec()).unwrap();
        c.cmd(&format!("rm {}", q(&format!("cjk/{p}"))))
            .expect_ok(&format!("rm {p}"));
    }
    let mut ds: Vec<String> = dirs
        .iter()
        .filter(|d| d.len() > 1)
        .map(|d| String::from_utf8(d[2..].to_vec()).unwrap())
        .collect();
    ds.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    assert_eq!(ds.len(), 19, "nine names, nested/ and its nine");
    for d in ds {
        c.cmd(&format!("rmdir {}", q(&format!("cjk/{d}"))))
            .expect_ok(&format!("rmdir {d}"));
    }
    assert!(server_names(samba(), &format!("upload/{}/cjk", s.name)).is_empty());
}

/// F26
#[test]
fn mangled_names() {
    let dir = tmp("mangled_names");
    let mut c = direct_in(&dir, None);
    let r = c.cmd("get -r /mangled");
    assert!(!r.ok);
    let items = item_errors(&r);
    assert_eq!(items.len(), 4, "{:?}", r.lines);
    assert!(
        items.iter().all(|v| v["kind"] == "invalid_name"),
        "{items:?}"
    );
    let names = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". {}/testenv/samba/names.sh; printf '%s\\n' \"$MANGLED_NAMES\"",
            env!("CARGO_MANIFEST_DIR")
        ))
        .output()
        .unwrap()
        .stdout;
    let names: Vec<&[u8]> = names
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(names.len(), 12);
    let got: Vec<_> = std::fs::read_dir(dir.join("mangled"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(got.len(), 5);
    for e in got {
        let text = std::fs::read(e.path()).unwrap();
        assert!(
            names.iter().any(|n| text == [*n, b"\n"].concat()),
            "{text:?}"
        );
    }
    assert!(temps_local(&dir).is_empty());
}

/// F27
#[test]
fn link_loops() {
    let dir = tmp("link_loops");
    let mut c = tapped_in(&dir, None);
    let r = c.cmd("get -r /loops");
    assert!(!r.ok);
    assert_eq!(
        per(&r),
        map(&[
            ("CREATE", 2),
            ("QUERY_DIRECTORY", 2),
            ("READ", 1),
            ("CLOSE", 2)
        ]),
        "expected 1 listing (CREATE 2, QUERY_DIRECTORY 2, READ 1, CLOSE 2: 7 requests)\n{}",
        context(r.stats())
    );
    let (files, dirs) = local_tree(&dir.join("loops"));
    assert_eq!(
        files.keys().cloned().collect::<Vec<_>>(),
        [b"./file.txt".to_vec()]
    );
    assert_eq!(dirs.len(), 1);
    let items = item_errors(&r);
    let paths: Vec<&str> = items.iter().map(|v| v["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["/loops/self", "/loops/up"]);
    assert!(
        items[0]["message"].as_str().unwrap().contains("as /loops "),
        "{:?}",
        items[0]
    );
}

/// F28
#[test]
fn overwrite_rules() {
    let dir = tmp("overwrite_rules");
    let s = Scratch::new(fast());
    let srv = |n: &str| format!("{}/{n}", s.server());
    make_file(&dir.join("small.bin"), 2000, 1);
    make_file(&dir.join("large.bin"), 600_000, 2);
    make_file(&dir.join("other.bin"), 900_000, 3);
    s.sh("mkdir dir && echo old > s.txt && echo old > l.txt");
    let mut c = tapped_in(&dir, Some(&s.remote()));
    // get onto an existing local file
    std::fs::write(dir.join("1.bin"), b"local").unwrap();
    let r = c.cmd("get /sizes/1.bin");
    assert_eq!(r.kind().as_deref(), Some("already_exists"));
    assert_eq!(std::fs::read(dir.join("1.bin")).unwrap(), b"local");
    let o = run_in(
        &dir,
        &[
            &fast().args(None)[..],
            &[
                "-C".into(),
                "use share".into(),
                "-C".into(),
                "get /sizes/1.bin".into(),
            ],
        ]
        .concat()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>(),
        None,
        &[],
    );
    assert_eq!(o.code, 1, "one-shot exit code: {}", o.stderr);
    c.cmd("get -f /sizes/1.bin").expect_ok("get -f");
    assert_eq!(local_sha(&dir.join("1.bin")), sha_of("/sizes/1.bin"));
    // get onto a directory name
    std::fs::create_dir(dir.join("65536.bin")).unwrap();
    for line in ["get /sizes/65536.bin", "get -f /sizes/65536.bin"] {
        assert_eq!(
            c.cmd(line).kind().as_deref(),
            Some("is_a_directory"),
            "{line}"
        );
    }
    assert!(dir.join("65536.bin").is_dir());
    // put onto existing remote files, small and large
    for (src, t) in [("small.bin", "s.txt"), ("large.bin", "l.txt")] {
        let old = server_sha(samba(), &srv(t));
        let r = c.cmd(&format!("put {src} {t}"));
        assert_eq!(r.kind().as_deref(), Some("already_exists"), "put {src} {t}");
        assert_eq!(server_sha(samba(), &srv(t)), old, "{t} unchanged");
        c.cmd(&format!("put -f {src} {t}")).expect_ok("put -f");
        assert_eq!(
            server_sha(samba(), &srv(t)),
            local_sha(&dir.join(src)),
            "{t} replaced"
        );
    }
    // put onto a directory without the trailing /
    for src in ["small.bin", "large.bin"] {
        for f in ["", "-f "] {
            let r = c.cmd(&format!("put {f}{src} dir"));
            assert_eq!(
                r.kind().as_deref(),
                Some("is_a_directory"),
                "put {f}{src} dir"
            );
            let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
            assert!(m.contains("dir/"), "{m}");
            assert!(server_names(samba(), &format!("upload/{}/dir", s.name)).is_empty());
        }
    }
    c.cmd("put other.bin dir/").expect_ok("put into dir/");
    assert_eq!(
        server_sha(samba(), &srv("dir/other.bin")),
        local_sha(&dir.join("other.bin"))
    );
    no_temps(&s, &dir);
}

/// Review finding 2 of Stage 2: a link (or any object) at the temporary name is never written
/// through. The create is create-only; on a collision the object itself is removed (not what a
/// link points to) and the create is repeated.
#[test]
fn link_at_the_temporary_name() {
    let dir = tmp("link_at_the_temporary_name");
    let s = Scratch::new(fast());
    make_file(&dir.join("small.bin"), 3000, 1);
    make_file(&dir.join("large.bin"), 600_000, 2);
    let mut c = tapped_in(&dir, Some(&s.remote()));
    // A link to a file that exists, on the small path and on the stream path; then a plain file
    // (what a killed transfer leaves), which the next transfer removes and replaces.
    for (name, plant, line) in [
        ("small.bin", "ln -s victim.txt", "put small.bin"),
        ("large.bin", "ln -s victim.txt", "put large.bin"),
        ("small.bin", "echo junk >", "put -f small.bin"),
    ] {
        s.sh(&format!(
            "echo victim-original > victim.txt; rm -f {name}; {plant} {}",
            fnv_temp(name)
        ));
        let r = c.cmd(line);
        r.expect_ok(line);
        println!("{line} with an object at the temporary name: {:?}", per(&r));
        assert_eq!(
            s.sh("head -c 15 victim.txt"),
            "victim-original",
            "{line}: the link's target was written"
        );
        assert_eq!(
            s.sh(&format!("test -f {name} && ! test -L {name} && echo ok"))
                .trim(),
            "ok",
            "{line}: the target is not a regular file"
        );
        assert_eq!(
            server_sha(samba(), &format!("{}/{name}", s.server())),
            local_sha(&dir.join(name))
        );
    }
    no_temps(&s, &dir);
}

/// I21
#[test]
fn overwrite_prompt() {
    let dir = tmp("overwrite_prompt");
    let s = Scratch::new(fast());
    s.sh("echo old > r.txt");
    std::fs::write(dir.join("r.txt"), b"new local\n").unwrap();
    std::fs::write(dir.join("1.bin"), b"keep").unwrap();
    let mut args = fast().args(None);
    args.push("-C".into());
    args.push(format!("lcd {}", q(&dir.to_string_lossy())));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = Pty::start(&a, &dir.join("history"));
    let h = fast().host(false);
    let root = format!("//{h}/share/> ");
    p.expect(&format!("//{h}> "), WAIT);
    p.line("use share", &root);
    let here = format!("//{h}/share{}> ", s.remote());
    p.line(&format!("cd {}", s.remote()), &here);
    // get: n, then Ctrl-C, then y
    p.send(b"get /sizes/1.bin\r");
    p.expect("Overwrite? [y/N] ", WAIT);
    p.send(b"n\r");
    p.expect("exists; -f replaces it", WAIT);
    p.expect(&here, WAIT);
    assert_eq!(std::fs::read(dir.join("1.bin")).unwrap(), b"keep");
    p.send(b"get /sizes/1.bin\r");
    p.expect("Overwrite? [y/N] ", WAIT);
    p.send(b"\x03");
    p.expect(&here, WAIT);
    assert_eq!(std::fs::read(dir.join("1.bin")).unwrap(), b"keep");
    p.send(b"\x1b[A");
    let shown = p.expect("get /sizes/1.bin", WAIT);
    assert!(!shown.contains("> y"), "Up recalled the answer: {shown:?}");
    p.send(b"\r");
    p.expect("Overwrite? [y/N] ", WAIT);
    p.send(b"y\r");
    p.expect(&here, WAIT);
    assert_eq!(local_sha(&dir.join("1.bin")), sha_of("/sizes/1.bin"));
    // put: n, then y
    p.send(b"put r.txt\r");
    p.expect("Overwrite? [y/N] ", WAIT);
    p.send(b"N\r");
    p.expect("-f replaces it", WAIT);
    p.expect(&here, WAIT);
    assert_eq!(s.sh("cat r.txt"), "old\n");
    p.send(b"put r.txt\r");
    p.expect("Overwrite? [y/N] ", WAIT);
    p.send(b"y\r");
    p.expect(&here, WAIT);
    assert_eq!(s.sh("cat r.txt"), "new local\n");
    let h = std::fs::read_to_string(dir.join("history")).unwrap_or_default();
    assert!(
        !h.lines().any(|l| l.trim() == "y" || l.trim() == "n"),
        "history: {h:?}"
    );
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

/// F29: the only test that uses `tiny`.
#[test]
fn disk_full_tiny() {
    let dir = tmp("disk_full_tiny");
    struct Empty;
    impl Drop for Empty {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker")
                .args([
                    "exec",
                    samba(),
                    "sh",
                    "-c",
                    "rm -rf /srv/tiny/* /srv/tiny/.[!.]*",
                ])
                .output();
        }
    }
    let _empty = Empty;
    make_file(&dir.join("twelve.bin"), 12 << 20, 1);
    make_file(&dir.join("five.bin"), 5 << 20, 2);
    let mut c = Client::to(fast(), &[]);
    c.cmd("use tiny").expect_ok("use tiny");
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd");
    let r = c.cmd("put twelve.bin");
    assert_eq!(r.kind().as_deref(), Some("disk_full"), "{:?}", r.lines);
    assert_eq!(
        docker_sh(samba(), "ls -A /srv/tiny"),
        "",
        "tiny is empty after the failed put"
    );
    c.cmd("put five.bin").expect_ok("put five.bin");
    assert_eq!(
        server_sha(samba(), "/srv/tiny/five.bin"),
        local_sha(&dir.join("five.bin"))
    );
    let o = run_in(
        &dir,
        &[
            &fast().args(None)[..],
            &[
                "-C".into(),
                "use tiny".into(),
                "-C".into(),
                "put -f twelve.bin five.bin".into(),
            ],
        ]
        .concat()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>(),
        None,
        &[],
    );
    assert_eq!(o.code, 1, "{}", o.stderr);
    assert_eq!(
        server_sha(samba(), "/srv/tiny/five.bin"),
        local_sha(&dir.join("five.bin")),
        "the old file untouched"
    );
    // Review finding 3: after a fatal error the summary's `failed` counts every item that did
    // not arrive, the root directory not being one of them. Two of three files fit.
    docker_sh(samba(), "rm -rf /srv/tiny/* /srv/tiny/.[!.]*");
    std::fs::create_dir(dir.join("three")).unwrap();
    for i in 0..3 {
        make_file(&dir.join(format!("three/f{i}.bin")), 4 << 20, 4 + i);
    }
    let r = c.cmd("put -r three");
    assert_eq!(r.kind().as_deref(), Some("disk_full"), "{:?}", r.lines);
    let sm = r.of("summary")[0];
    assert_eq!(
        (
            sm["files"].as_u64(),
            sm["failed"].as_u64(),
            sm["directories"].as_u64()
        ),
        (Some(2), Some(1), Some(1)),
        "{sm}"
    );
}

/// F30
#[test]
fn read_only_share() {
    let dir = tmp("read_only_share");
    make_file(&dir.join("s.bin"), 100, 1);
    make_file(&dir.join("l.bin"), 700_000, 2);
    std::fs::create_dir_all(dir.join("tree/x")).unwrap();
    make_file(&dir.join("tree/x/f"), 10, 3);
    let mut c = Client::to(fast(), &[]);
    c.cmd("use readonly").expect_ok("use readonly");
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd");
    let before = sha_of("/sizes/1.bin");
    for line in [
        "put s.bin /upload/s.bin",
        "put l.bin /upload/l.bin",
        "put -r tree /upload/",
        "mkdir /upload/newdir",
        "mkdir -p /tree/d0/newdir",
        "rm /sizes/1.bin",
        "rmdir /tree/d0/d0/d0/d0",
    ] {
        let r = c.cmd(line);
        assert_eq!(
            r.kind().as_deref(),
            Some("access_denied"),
            "{line}: {:?}",
            r.lines
        );
    }
    assert_eq!(sha_of("/sizes/1.bin"), before);
    docker_sh(
        samba(),
        "test -d /srv/share/tree/d0/d0/d0/d0 && ! test -e /srv/share/tree/d0/newdir",
    );
    // What this test tried to create is not there (other tests' directories may be).
    let names = server_names(samba(), "upload");
    for n in ["s.bin", "l.bin", "tree", "newdir"] {
        assert!(!names.contains(n.as_bytes()), "{n} was created");
    }
    for n in ["s.bin", "l.bin"] {
        assert!(!names.contains(fnv_temp(n).as_bytes()), "temp of {n} left");
    }
    c.cmd("get /sizes/65536.bin").expect_ok("get on readonly");
    c.cmd("cat /tree/d0/file.txt").expect_ok("cat on readonly");
}

/// I22
#[test]
fn file_ops_and_cache() {
    let dir = tmp("file_ops_and_cache");
    let s = Scratch::new(fast());
    make_file(&dir.join("pfile.bin"), 100, 1);
    std::fs::create_dir_all(dir.join("ptree/inner")).unwrap();
    let (mut p, tap, _) = Pty::tapped(
        fast(),
        &["-C", &format!("lcd {}", q(&dir.to_string_lossy()))],
        &dir.join("history"),
    );
    let here = format!("//{}/share{}> ", fast().host(true), s.remote());
    p.line("use share", &p1());
    p.line(&format!("cd {}", s.remote()), &here);
    p.line("ls", &here);
    take(&tap);
    let tab_twice = |p: &mut Pty, text: &str, shows: &str, what: &str| {
        let st = tab(p, &tap, text, Some(shows));
        expect_stats(
            &st,
            &format!("expected exactly one listing (5 requests) for the Tab after {what}"),
            5,
            1,
        );
        let st = tab(p, &tap, text, Some(shows));
        expect_stats(&st, &format!("second Tab after {what}"), 0, 0);
        p.send(CTRL_U);
    };
    p.line("mkdir newdir", &here);
    take(&tap);
    s.sh("test -d newdir");
    tab_twice(&mut p, "cd n", "cd newdir/", "mkdir");
    p.line("put pfile.bin", &here);
    take(&tap);
    s.sh("test -f pfile.bin");
    tab_twice(&mut p, "ls pf", "ls pfile.bin ", "put");
    p.line("rm pfile.bin", &here);
    take(&tap);
    s.sh("! test -e pfile.bin");
    tab_twice(&mut p, "ls n", "ls newdir/", "rm");
    p.line("rmdir newdir", &here);
    take(&tap);
    s.sh("! test -e newdir");
    p.line("put -r ptree", &here);
    take(&tap);
    s.sh("test -d ptree/inner");
    tab_twice(&mut p, "ls p", "ls ptree/", "rmdir and put -r");
    s.sh("echo x > ptree/inner/f");
    p.send(b"rmdir ptree/inner\r");
    p.expect("directory not empty", WAIT);
    p.expect(&here, WAIT);
    p.send(b"rmdir .\r");
    p.expect("cd out of it first", WAIT);
    p.expect(&here, WAIT);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

/// Names the server sends reach the terminal escaped (final review, finding 2): a control
/// character (the C1 CSI 0x9B starts an escape sequence on some terminals) or a bidi override
/// (U+202E reverses what is shown after it) in text output is shown as `\u{..}`; `--json` carries
/// the exact name, and `get` creates the local file under the exact name.
#[test]
fn hostile_names_are_escaped_in_text() {
    let dir = tmp("hostile_names");
    let s = Scratch::new(fast());
    s.sh(r#"printf x > "$(printf 'a\xc2\x9b31mX')"; printf y > "$(printf 'b\xe2\x80\xaeexe.txt')"; mkdir "$(printf 'd\xc2\x9b2Jdir')""#);
    let (csi, rlo) = ("a\u{9b}31mX", "b\u{202e}exe.txt");
    let (csi_shown, rlo_shown) = (r"a\u{9b}31mX", r"b\u{202e}exe.txt");
    let clean = |what: &str, text: &str| {
        for c in text.chars() {
            assert!(
                c == '\n'
                    || !(c.is_control()
                        || ('\u{202a}'..='\u{202e}').contains(&c)
                        || ('\u{2066}'..='\u{2069}').contains(&c)),
                "{what}: {c:?} reached the terminal: {text:?}"
            );
        }
    };
    let remote = s.remote();
    let at = |n: &str| q(&format!("{remote}/{n}"));
    let cmds = [
        format!("ls {}", q(&remote)),
        format!("find {}", q(&remote)),
        format!("tree {}", q(&remote)),
        format!("get {}", at(rlo)),
        format!("cd {}", at(csi)),
    ];
    let mut args = fast_args(&["--keep-going", "-C", "use share"]);
    for c in &cmds {
        args.extend(["-C".to_string(), c.clone()]);
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = run_in(&dir, &a, None, &[]);
    clean("stdout", &o.stdout);
    clean("stderr", &o.stderr);
    for shown in [csi_shown, rlo_shown, r"d\u{9b}2Jdir"] {
        assert!(o.stdout.contains(shown), "{shown} in {:?}", o.stdout);
    }
    assert!(
        o.stderr.contains(csi_shown),
        "the error names it escaped: {:?}",
        o.stderr
    );
    assert_eq!(
        std::fs::read(dir.join(rlo)).unwrap(),
        b"y",
        "the local file has the exact name"
    );
    // --json: the exact name, and the path of the error.
    let mut args = fast_args(&["--json", "-C", "use share", "-C"]);
    args.push(format!("ls {}", q(&remote)));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let names = run(&a, None)
        .json()
        .iter()
        .filter(|v| v["type"] == "entry")
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&csi.to_string()) && names.contains(&rlo.to_string()),
        "{names:?}"
    );
}

/// `rmdir` refuses the working directory and the ones above it however the path is spelled: the
/// server folds case, so does the guard (final review, finding 4).
#[test]
fn rmdir_guard_folds_case() {
    let dir = tmp("rmdir_guard_folds_case");
    let s = Scratch::new(fast());
    let mut c = direct_in(&dir, Some(&s.remote()));
    c.cmd("mkdir cw").expect_ok("mkdir");
    c.cmd("mkdir cw/in").expect_ok("mkdir cw/in");
    c.cmd("cd cw/in").expect_ok("cd");
    for arg in [
        "../IN",
        ".",
        "../../CW",
        "../../cw",
        &format!("{}/Cw/IN", s.remote().to_uppercase()),
    ] {
        let r = c.cmd(&format!("rmdir {}", q(arg)));
        assert!(!r.ok, "rmdir {arg} succeeded: {:?}", r.lines);
        assert!(
            format!("{:?}", r.lines).contains("cd out of it first"),
            "rmdir {arg}: {:?}",
            r.lines
        );
    }
    s.sh("test -d cw/in");
    c.cmd("cd ../..").expect_ok("cd out");
    c.cmd("rmdir CW/IN").expect_ok("rmdir CW/IN");
    c.cmd("rmdir CW").expect_ok("rmdir CW");
    s.sh("! test -e cw");
}

/// Text-mode `cat` to a file through stdin lines; returns the peak RSS in kB.
fn cat_to_file(remote: &str, file: &std::path::Path, size: u64) -> u64 {
    let mut child = Command::new(BIN)
        .args(fast().args(None))
        .env("RSMB_HISTORY", "")
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(file).unwrap())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    use std::io::Write;
    writeln!(w, "use share\ncat {remote}").unwrap();
    wait_file(file, size, WAIT);
    let s = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
    let kb = s
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap();
    drop(w);
    let st = child.wait().unwrap();
    assert!(st.success());
    kb
}

/// F31
#[test]
fn cat_output() {
    let dir = tmp("cat_output");
    let f = dir.join("medium.out");
    let kb = cat_to_file("/large/medium.bin", &f, 52_432_897);
    assert_eq!(local_sha(&f), sha_of("/large/medium.bin"));
    assert!(
        kb <= 64_000,
        "cat medium.bin: peak RSS {kb} kB, expected at most 64 MB"
    );
    let line = format!(
        "{BIN} {} -C 'use share' -C 'cat /large/big.bin' | head -c 1000 > /dev/null; echo \"code=${{PIPESTATUS[0]}}\"",
        fast().args(None).join(" ")
    );
    let o = run_sh(&line);
    assert!(
        o.took < Duration::from_secs(5),
        "cat | head took {:?}",
        o.took
    );
    assert!(!o.stderr.contains("panicked"), "{}", o.stderr);
    assert!(
        !o.stdout.contains("code=0") && o.stdout.contains("code="),
        "{}",
        o.stdout
    );
    let o = run_fast(
        &[
            "--json",
            "--keep-going",
            "-C",
            "use share",
            "-C",
            "cat /tree/d0/file.txt",
            "-C",
            "cat /sizes/65537.bin",
            "-C",
            "cat /sizes/8388608.bin",
            "-C",
            "cat /tree",
        ],
        None,
    );
    assert_eq!(o.stderr, "", "stderr stays empty");
    let v = o.json();
    let content: Vec<&Value> = v.iter().filter(|x| x["type"] == "content").collect();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["text"], "tree/d0\n");
    assert_eq!(content[0]["path"], "/tree/d0/file.txt");
    let errors: Vec<&Value> = v.iter().filter(|x| x["type"] == "error").collect();
    assert_eq!(errors.len(), 3, "{v:?}");
    for e in &errors[..2] {
        assert_eq!(e["kind"], "other");
        assert!(e["message"].as_str().unwrap().contains("get"), "{e}");
    }
    assert_eq!(errors[2]["kind"], "is_a_directory");
}

/// F32
#[test]
fn local_commands() {
    let dir = tmp("local_commands");
    std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
    std::fs::write(dir.join("sub/a.txt"), b"abc").unwrap();
    std::fs::write(dir.join(fnv_temp("x")), b"planted").unwrap();
    let ds = dir.to_string_lossy().to_string();
    let mut c = Client::tapped(fast());
    let r = c.cmd(&format!("lcd {}", q(&ds)));
    r.expect_ok("lcd before use");
    r.expect("lcd", 0, 0);
    let r = c.cmd("lpwd");
    r.expect("lpwd", 0, 0);
    assert_eq!(r.of("lpwd")[0]["path"], ds.as_str());
    let r = c.cmd("lls");
    r.expect("lls", 0, 0);
    let names: Vec<&str> = r
        .of("local_entry")
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [fnv_temp("x").as_str(), "sub"],
        "sorted, the planted .rsmb-….part shown"
    );
    let sub = r.of("local_entry")[1];
    assert_eq!(
        (sub["is_dir"].as_bool(), sub["path"].as_str()),
        (Some(true), Some(format!("{ds}/sub").as_str()))
    );
    let r = c.cmd("lls sub");
    let e: Vec<&Value> = r.of("local_entry");
    assert_eq!(e.len(), 2);
    assert_eq!(
        (
            e[0]["name"].as_str(),
            e[0]["size"].as_u64(),
            e[0]["is_dir"].as_bool()
        ),
        (Some("a.txt"), Some(3), Some(false))
    );
    assert!(e[0]["mtime"].as_str().unwrap().ends_with('Z'));
    let r = c.cmd("lcd /nonexistent");
    assert_eq!(r.kind().as_deref(), Some("local_io"));
    r.expect("a failed lcd", 0, 0);
    assert_eq!(
        c.cmd("lpwd").of("lpwd")[0]["path"],
        ds.as_str(),
        "unchanged"
    );
    let o = run_env(
        &[
            &fast().args(None)[..],
            &[
                "--json".into(),
                "-C".into(),
                "lcd".into(),
                "-C".into(),
                "lpwd".into(),
            ],
        ]
        .concat()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>(),
        None,
        &[("HOME", &format!("{ds}/sub"))],
    );
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert!(
        o.stdout.contains(&format!("\"path\":\"{ds}/sub\"")),
        "{}",
        o.stdout
    );
    let o = run_in(
        &dir,
        &[&fast().args(None)[..], &["-C".into(), "lls".into()]]
            .concat()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        None,
        &[],
    );
    assert!(
        o.stdout
            .lines()
            .any(|l| l.starts_with('d') && l.ends_with("  sub/")),
        "{}",
        o.stdout
    );
}

/// Review finding 3, the download side: after a fatal error (the connection is cut while a large
/// file is read) every item that did not arrive is `failed`, and the root directory, which the
/// summary counts as a directory, is not one of the items: files + failed = the three files.
#[test]
fn get_r_summary_after_a_fatal_error() {
    let dir = tmp("get_r_summary_after_a_fatal_error");
    let mut c = tapped_in(&dir, None);
    c.send("get -r /large");
    c.wait_requests("READ", 1, WAIT);
    assert_eq!(c.tap().drop_connections(), 1);
    let r = c.wait_done(WAIT);
    assert_eq!(
        r.kind().as_deref(),
        Some("connection_lost"),
        "{:?}",
        r.lines
    );
    let sm = r.of("summary")[0];
    let (files, failed) = (
        sm["files"].as_u64().unwrap(),
        sm["failed"].as_u64().unwrap(),
    );
    assert_eq!(files + failed, 3, "{sm}");
    assert_eq!(sm["directories"], 1, "{sm}");
    assert!(temps_local(&dir).is_empty());
}

/// Review finding 7: a download refused before any data was read (`cat --json` of a file over
/// 1 MiB, `get` into a directory that cannot be written) leaves nothing on the wire, so the next
/// command uses the same connection.
#[test]
fn refusal_before_data_keeps_the_connection() {
    let dir = tmp("refusal_before_data");
    let ro = dir.join("ro");
    std::fs::create_dir(&ro).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut c = tapped_in(&ro, None);
    let r = c.cmd("cat /sizes/1048577.bin");
    assert_eq!(r.kind().as_deref(), Some("other"), "{:?}", r.lines);
    assert_eq!(r.stats().connections, 0);
    let r = c.cmd("get /sizes/65537.bin");
    assert_eq!(r.kind().as_deref(), Some("local_io"), "{:?}", r.lines);
    assert_eq!(r.stats().connections, 0);
    let r = c.cmd("get /sizes/1048577.bin");
    assert_eq!(r.kind().as_deref(), Some("local_io"), "{:?}", r.lines);
    assert_eq!(r.stats().connections, 0);
    let r = c.cmd("ls /sizes");
    r.expect_ok("ls after the refusals");
    assert_eq!(
        r.stats().connections,
        0,
        "the refusals replaced the connection\n{}",
        log(r.stats())
    );
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// F33
#[test]
fn local_failures() {
    let dir = tmp("local_failures");
    let line = format!(
        "cd {} && trap '' XFSZ; ulimit -f 1024; exec {BIN} {} --json -C 'use share' -C 'get /large/medium.bin'",
        dir.display(),
        fast().args(None).join(" ")
    );
    let o = run_sh(&line);
    assert_eq!(o.code, 1, "{}", o.stdout);
    assert!(o.stdout.contains("\"kind\":\"local_io\""), "{}", o.stdout);
    assert!(
        !dir.join("medium.bin").exists(),
        "nothing under the final name"
    );
    assert!(temps_local(&dir).is_empty());
    let ro = dir.join("ro");
    std::fs::create_dir(&ro).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut c = direct_in(&ro, None);
    let r = c.cmd("get /sizes/1.bin");
    assert_eq!(r.kind().as_deref(), Some("local_io"), "{:?}", r.lines);
    assert_eq!(
        std::fs::read_dir(&ro).unwrap().count(),
        0,
        "nothing created"
    );
    c.cmd("ls /sizes").expect_ok("the next command works");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// F34
#[test]
fn recursive_put_refuses_and_goes_on() {
    let dir = tmp("recursive_put_refuses");
    let s = Scratch::new(fast());
    let t = dir.join("src");
    std::fs::create_dir_all(t.join("bad?dir")).unwrap();
    std::fs::create_dir_all(t.join("locked")).unwrap();
    std::fs::create_dir_all(t.join("realdir")).unwrap();
    for f in [
        "g1.txt",
        "g2.txt",
        "realdir/g3.txt",
        "a:b.txt",
        "bad?dir/x",
        "bad?dir/y",
        "locked/inside",
        "nope.txt",
    ] {
        make_file(&t.join(f), 64, 1);
    }
    std::os::unix::fs::symlink(t.join("g1.txt"), t.join("link-file")).unwrap();
    std::os::unix::fs::symlink(t.join("realdir"), t.join("link-dir")).unwrap();
    let fifo = std::ffi::CString::new(t.join("fifo").to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(t.join("nope.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(t.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut c = direct_in(&dir, Some(&s.remote()));
    let r = c.cmd("put -r src");
    std::fs::set_permissions(t.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(t.join("nope.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!r.ok);
    let items = item_errors(&r);
    let mut k: Vec<&str> = items.iter().map(|v| v["kind"].as_str().unwrap()).collect();
    k.sort();
    assert_eq!(
        k,
        [
            "invalid_name",
            "invalid_name",
            "local_io",
            "local_io",
            "other",
            "other",
            "other"
        ],
        "{items:#?}"
    );
    let bad = items
        .iter()
        .find(|v| v["path"].as_str().unwrap().ends_with("bad?dir"))
        .unwrap();
    assert!(
        bad["message"].as_str().unwrap().contains("2 files"),
        "{bad}"
    );
    let (files, dirs) = server_tree(samba(), &format!("{}/src", s.server()));
    let names: Vec<&[u8]> = files.keys().map(Vec::as_slice).collect();
    assert_eq!(names, [&b"./g1.txt"[..], b"./g2.txt", b"./realdir/g3.txt"]);
    for f in ["g1.txt", "g2.txt", "realdir/g3.txt"] {
        assert_eq!(files[format!("./{f}").as_bytes()], local_sha(&t.join(f)));
    }
    assert_eq!(
        dirs.len(),
        2,
        "src and realdir; locked does not exist on the server: {dirs:?}"
    );
    // Review finding 6: a root that cannot be read is an error of the command, and the server
    // gets no directory for it.
    let root = dir.join("rootlocked");
    std::fs::create_dir(&root).unwrap();
    make_file(&root.join("f"), 10, 1);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = c.cmd("put -r rootlocked");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r.kind().as_deref(), Some("local_io"), "{:?}", r.lines);
    assert!(r.of("summary").is_empty(), "{:?}", r.lines);
    assert_eq!(s.sh("test -e rootlocked || echo absent").trim(), "absent");
}

/// F35
#[test]
fn single_transfer_arguments() {
    let dir = tmp("single_transfer_arguments");
    let s = Scratch::new(fast());
    std::fs::create_dir(dir.join("into")).unwrap();
    std::fs::create_dir_all(dir.join("pd/inner")).unwrap();
    make_file(&dir.join("pd/inner/f.bin"), 10, 1);
    s.sh("mkdir sub");
    let mut c = direct_in(&dir, Some(&s.remote()));
    c.cmd("get /sizes/1.bin").expect_ok("get f");
    assert!(dir.join("1.bin").is_file());
    c.cmd("get /sizes/65535.bin into").expect_ok("get f dir");
    assert!(dir.join("into/65535.bin").is_file());
    c.cmd("get /sizes/65536.bin new.bin").expect_ok("get f new");
    assert_eq!(local_sha(&dir.join("new.bin")), sha_of("/sizes/65536.bin"));
    c.cmd("get -r /sizes/65537.bin into")
        .expect_ok("get -r on a file");
    assert_eq!(
        local_sha(&dir.join("into/65537.bin")),
        sha_of("/sizes/65537.bin")
    );
    let r = c.cmd("get /tree");
    assert_eq!(r.kind().as_deref(), Some("is_a_directory"));
    assert!(r.of("error")[0]["message"].as_str().unwrap().contains("-r"));
    assert!(!dir.join("tree").exists(), "no local file of size 0");
    let r = c.cmd("put pd");
    assert_eq!(r.kind().as_deref(), Some("is_a_directory"));
    assert!(r.of("error")[0]["message"].as_str().unwrap().contains("-r"));
    c.cmd("put 1.bin").expect_ok("put f");
    s.sh("test -f 1.bin");
    c.cmd("put new.bin sub/").expect_ok("put f sub/");
    s.sh("test -f sub/new.bin");
    c.cmd("put -r pd sub").expect_ok("put -r d sub");
    s.sh("test -f sub/pd/inner/f.bin");
    let r = c.cmd("get -r /loops missing");
    assert_eq!(r.kind().as_deref(), Some("local_io"));
    // Review finding 4: a local link goes up under its own name, with its target's content.
    std::fs::write(dir.join("real.txt"), b"the target").unwrap();
    std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
    s.sh("echo precious > real.txt");
    c.cmd("put -f link.txt").expect_ok("put -f link.txt");
    assert_eq!(s.sh("cat link.txt"), "the target");
    assert_eq!(s.sh("cat real.txt"), "precious\n", "real.txt was replaced");
    // `.` has no name of its own: the directory's.
    c.cmd("lcd pd").expect_ok("lcd pd");
    c.cmd("put -r -f . sub").expect_ok("put -r -f . sub");
    s.sh("test -f sub/pd/inner/f.bin");
}

/// I23
#[test]
fn progress_on_terminal_only() {
    let dir = tmp("progress_on_terminal_only");
    let s = Scratch::new(fast());
    let mut args = fast().args(None);
    args.extend(["-C", "use share"].map(String::from));
    args.push("-C".into());
    args.push(format!("lcd {}", q(&dir.to_string_lossy())));
    args.push("-C".into());
    args.push(format!("cd {}", s.remote()));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = Pty::start(&a, &dir.join("history"));
    p.expect("> ", WAIT);
    p.send(b"get /large/medium.bin\r");
    let out = p.expect("52432897 bytes", WAIT);
    assert!(out.contains("100%") && out.contains("MiB/s"), "{out:?}");
    p.send(b"put medium.bin\r");
    let out = p.expect("52432897 bytes", WAIT);
    assert!(out.contains("100%") && out.contains("MiB/s"), "{out:?}");
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
    std::fs::remove_file(dir.join("medium.bin")).unwrap();
    let mut cmd = fast().args(None);
    cmd.extend(["-C", "use share", "-C", "get /large/medium.bin"].map(String::from));
    let a: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let o = run_in(&dir, &a, None, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stderr, "", "piped: stderr empty");
    assert!(o.stdout.contains("52432897 bytes"), "{}", o.stdout);
    std::fs::remove_file(dir.join("medium.bin")).unwrap();
    let mut j = vec!["--json"];
    j.extend(&a);
    let o = run_in(&dir, &j, None, &[]);
    let t: Vec<Value> = o
        .json()
        .into_iter()
        .filter(|v| v["type"] == "transfer")
        .collect();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0]["bytes"], 52432897);
    assert_eq!(o.stderr, "");
}

/// F36
#[test]
fn mkdir_rules() {
    let dir = tmp("mkdir_rules");
    let s = Scratch::new(fast());
    s.sh("echo file > f");
    let mut c = tapped_in(&dir, Some(&s.remote()));
    c.cmd("mkdir d").expect_ok("mkdir d");
    assert_eq!(c.cmd("mkdir d").kind().as_deref(), Some("already_exists"));
    assert_eq!(c.cmd("mkdir f").kind().as_deref(), Some("already_exists"));
    let r = c.cmd("mkdir x/y");
    assert_eq!(r.kind().as_deref(), Some("not_found"));
    assert!(r.of("error")[0]["message"].as_str().unwrap().contains("-p"));
    let r = c.cmd("mkdir -p x/y/z");
    r.expect_ok("mkdir -p");
    r.expect("mkdir -p x/y/z", 6, 3);
    s.sh("test -d x/y/z");
    c.cmd("mkdir -p x/y/z").expect_ok("mkdir -p again");
    for line in ["mkdir -p f", "mkdir -p f/sub"] {
        assert_eq!(
            c.cmd(line).kind().as_deref(),
            Some("not_a_directory"),
            "{line}"
        );
    }
    assert_eq!(s.sh("cat f"), "file\n");
    let t = dir.join("up");
    std::fs::create_dir_all(t.join("f/deep")).unwrap();
    make_file(&t.join("f/deep/a"), 5, 1);
    make_file(&t.join("f/b"), 5, 2);
    make_file(&t.join("ok.txt"), 5, 3);
    c.cmd("mkdir up").expect_ok("mkdir up");
    s.sh("echo file > up/f");
    let r = c.cmd("put -r up");
    let items = item_errors(&r);
    assert_eq!(items.len(), 1, "{:?}", r.lines);
    assert_eq!(items[0]["kind"], "not_a_directory");
    assert!(
        items[0]["message"]
            .as_str()
            .unwrap()
            .contains("2 files and 1 directories"),
        "{}",
        items[0]
    );
    s.sh("test -f up/ok.txt");
    assert_eq!(r.of("summary")[0]["failed"], 4);
}

/// F37
#[test]
fn get_r_keeps_inside_its_destination() {
    let dir = tmp("get_r_keeps_inside");
    let dest = dir.join("dest");
    let outside = dir.join("outside");
    std::fs::create_dir_all(dest.join("tree")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("file.txt"), b"mine\n").unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("tree/d0")).unwrap();
    let before = local_sha(&outside.join("file.txt"));
    let mut c = direct_in(&dir, None);
    for line in ["get -r /tree dest", "get -r -f /tree dest"] {
        let r = c.cmd(line);
        assert!(!r.ok);
        let items = item_errors(&r);
        let d0: Vec<&&Value> = items.iter().filter(|v| v["path"] == "/tree/d0").collect();
        assert_eq!(d0.len(), 1, "{line}: {items:?}");
        assert!(
            d0[0]["message"]
                .as_str()
                .unwrap()
                .contains("85 files and 84 directories"),
            "{}",
            d0[0]
        );
        assert_eq!(local_sha(&outside.join("file.txt")), before, "{line}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1, "{line}");
    }
    let want = tree_without(
        samba(),
        "/srv/share/tree",
        &["./forbidden", "./unreadable.txt", "./d0"],
    );
    let (files, mut dirs) = local_tree(&dest.join("tree"));
    dirs.remove(b"./d0".as_slice());
    assert_eq!((files, dirs), want, "everything else of tree arrived");
}

// ---------------------------------------------------------------------------------------------
// Stage 3: tree and find (docs/plans/stage-3.md 6.4)

/// A direct client at the share root.
fn at_root() -> Client {
    let mut c = Client::to(fast(), &[]);
    c.cmd("use share").expect_ok("use share");
    c
}

/// Components below `/tree` of an entry's path.
fn below(path: &str, base: &str) -> usize {
    path.strip_prefix(base)
        .unwrap()
        .split('/')
        .filter(|s| !s.is_empty())
        .count()
}

/// F41
#[test]
fn walk_depth() {
    let mut c = at_root();
    for (n, want_len) in [(Some(1), 6), (Some(2), 26), (Some(3), 106), (None, 682)] {
        let depth: Vec<String> = n
            .map(|n| vec!["-maxdepth".to_string(), n.to_string()])
            .unwrap_or_default();
        let d: Vec<&str> = depth.iter().map(String::as_str).collect();
        let want = server_find(samba(), "tree", &d);
        assert_eq!(want.len(), want_len, "the server's find -maxdepth {n:?}");
        let lines = match n {
            Some(n) => [
                format!("find /tree -maxdepth {n}"),
                format!("tree -L {n} /tree"),
            ],
            None => ["find /tree".to_string(), "tree /tree".to_string()],
        };
        for line in lines {
            let r = c.cmd(&line);
            r.expect_ok(&line);
            same_paths(&line, &r.paths(), &want);
            for e in r.of("entry") {
                let p = e["path"].as_str().unwrap();
                assert_eq!(e["depth"], below(p, "/tree"), "{line}: depth of {p}");
            }
        }
    }
    // Depth 0 is a usage error and sends nothing.
    let mut t = Client::tapped(fast());
    t.cmd("use share").expect_ok("use share");
    for line in ["find /tree -maxdepth 0", "tree -L 0 /tree"] {
        let r = t.cmd(line);
        assert_eq!(r.kind().as_deref(), Some("usage"), "{line}");
        r.expect(line, 0, 0);
        let o = run_fast(&["--json", "-C", "use share", "-C", line], None);
        assert_eq!(o.code, 2, "{line}: {}", o.stdout);
    }
    // Relative to the working directory, printed from the share root.
    c.cmd("cd /tree/d0").expect_ok("cd");
    let want = server_find(samba(), "tree/d0", &[]);
    for line in ["find", "find ."] {
        let r = c.cmd(line);
        r.expect_ok(line);
        same_paths(line, &r.paths(), &want);
        assert!(
            r.of("entry")
                .iter()
                .all(|e| e["path"].as_str().unwrap().starts_with("/tree/d0/")),
            "{line}"
        );
    }
    // Text mode.
    let o = run_fast(&["-C", "use share", "-C", "tree /tree/d0/d0/d0"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let want = "\
/tree/d0/d0/d0
├── d0/
│   └── file.txt
├── d1/
│   └── file.txt
├── d2/
│   └── file.txt
├── d3/
│   └── file.txt
└── file.txt

4 directories, 5 files
";
    assert_eq!(o.stdout, want);
    let o = run_fast(&["-C", "use share", "-C", "tree /tree"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(
        lines.len(),
        682 + 3,
        "the start, 682 entries, an empty line, the count"
    );
    assert_eq!(lines[0], "/tree");
    assert_eq!(*lines.last().unwrap(), "341 directories, 341 files");
    let o = run_fast(&["-C", "use share", "-C", "find /tree"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(lines.len(), 682);
    let want = server_find(samba(), "tree", &[]);
    let dirs = server_find(samba(), "tree", &["-type", "d"]);
    for l in &lines {
        let p = l.strip_prefix('/').unwrap();
        let (p, slash) = match p.strip_suffix('/') {
            Some(d) => (d, true),
            None => (p, false),
        };
        assert!(want.contains(p.as_bytes()), "{l}");
        assert_eq!(
            slash,
            dirs.contains(p.as_bytes()),
            "{l}: / after a directory only"
        );
    }
}

/// `+1M` → `+1048576c`: the server's `-size` in bytes (BusyBox has no M, and c does not round).
fn server_size(s: &str) -> String {
    let (sign, rest) = match s.chars().next() {
        Some(c @ ('+' | '-')) => (c.to_string(), &s[1..]),
        _ => (String::new(), s),
    };
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let n: u64 = rest[..end].parse().unwrap();
    let mult: u64 = match &rest[end..] {
        "" | "B" | "c" => 1,
        "K" | "k" => 1 << 10,
        "M" | "m" => 1 << 20,
        "G" | "g" => 1 << 30,
        "T" | "t" => 1 << 40,
        "P" | "p" => 1 << 50,
        u => panic!("unit {u}"),
    };
    format!("{sign}{}c", n * mult)
}

/// The 60 patterns of docs/plans/stage-3.md 2.7: (directory, flag, pattern).
fn patterns() -> Vec<(&'static str, &'static str, String)> {
    let mut v = Vec::new();
    for p in [
        "*.txt",
        "[[]abc[]].txt",
        "\\[abc\\].txt",
        "{a,b}.txt",
        "-*",
        "--help*",
        "*space*",
        "?ase.txt",
        "[Cc]ase.txt",
        "[!C]ase.txt",
        "caf?.txt",
        "caf??.txt",
        "*😀*",
        "? smile.txt",
        "Gr??e.txt",
        "it's.txt",
        "*%*",
        "*[;&]*",
        "#*",
        "$*",
        "*`*",
        "  *",
        "*",
        "[",
        "*[*",
        "[a-c]*",
        "leaf.tx[s-u]",
        "*.TXT",
    ] {
        v.push(("special", "-name", p.to_string()));
    }
    for p in [
        "CASE.TXT",
        "GRÜSSE.TXT",
        "GRÜßE.TXT",
        "привет.txt",
        "CAF\u{c9}.TXT",
        "*.TXT",
        "[A-C]ASE.txt",
        "DIR WITH SPACE",
        "😀*",
    ] {
        v.push(("special", "-iname", p.to_string()));
    }
    for p in [
        "简体中文*",
        "*中文",
        "\u{d55c}\u{ae00}",
        "?\u{ae00}*",
        "??",
        "*_0999.txt",
        "*_00[0-4]?.txt",
        "ＦＵＬＬ*",
        "東京\u{3000}大阪*",
        "𠮷*",
        "?野家*",
        "[简繁]*",
        "[一-龥]*",
        "*.txt",
        "leaf.txt",
        "\u{30d1}\u{30f3}*",
        "???",
        "か*",
        "\u{304c}*",
    ] {
        v.push(("cjk", "-name", p.to_string()));
    }
    for p in ["ｆｕｌｌ*", "简体中文*", "LEAF.TXT", "[ｆ]*"] {
        v.push(("cjk", "-iname", p.to_string()));
    }
    assert_eq!(v.len(), 60);
    v
}

/// `find /<dir> <args>`: the set of paths equals the server's `find <dir> <server>`; its size.
fn check_find(c: &mut Client, dir: &str, args: &[&str], server: &[String]) -> usize {
    let line = format!(
        "find /{dir} {}",
        args.iter().map(|a| q(a)).collect::<Vec<_>>().join(" ")
    );
    let r = c.cmd(&line);
    r.expect_ok(&line);
    let s: Vec<&str> = server.iter().map(String::as_str).collect();
    let want = server_find(samba(), dir, &s);
    same_paths(&line, &r.paths(), &want);
    want.len()
}

/// F42
#[test]
fn find_filters() {
    let mut c = at_root();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        check_find(&mut c, "tree", &["-type", "f"], &s(&["-type", "f"])),
        341
    );
    assert_eq!(
        check_find(&mut c, "tree", &["-type", "d"], &s(&["-type", "d"])),
        341
    );
    for (dir, size, n) in [
        ("sizes", "+65536", 7),
        ("sizes", "-65536", 3),
        ("sizes", "65536", 1),
        ("sizes", "+64K", 7),
        ("sizes", "-64K", 3),
        ("sizes", "64K", 1),
        ("sizes", "+1M", 4),
        ("sizes", "-1M", 6),
        ("sizes", "1M", 1),
        ("sizes", "+0", 10),
        ("sizes", "0", 1),
        ("sizes", "1024k", 1),
        ("sizes", "1T", 0),
        ("sizes", "1P", 0),
        ("sizes", "65536c", 1),
        ("sizes", "+65536B", 7),
        ("large", "+1G", 2),
        ("large", "-1G", 1),
        ("large", "+4G", 1),
    ] {
        let got = check_find(
            &mut c,
            dir,
            &["-size", size],
            &s(&["-type", "f", "-size", &server_size(size)]),
        );
        assert_eq!(got, n, "find /{dir} -size {size}");
    }
    let r = c.cmd("find /sizes -size 10b");
    assert_eq!(r.kind().as_deref(), Some("usage"));
    assert!(
        r.of("error")[0]["message"]
            .as_str()
            .unwrap()
            .contains("512")
    );
    let mut t = Client::tapped(fast());
    t.cmd("use share").expect_ok("use share");
    let r = t.cmd("find /tree -name '[[:digit:]]*'");
    assert_eq!(r.kind().as_deref(), Some("usage"));
    r.expect("a refused pattern sends nothing", 0, 0);
    let r = c.cmd("find /tree -size +0 -type d");
    r.expect_ok("-size with -type d");
    assert_eq!(r.of("entry").len(), 0, "a directory never matches -size");
    let n = check_find(
        &mut c,
        "tree",
        &["-name", "file.txt", "-type", "f", "-maxdepth", "3"],
        &s(&["-name", "file.txt", "-type", "f", "-maxdepth", "3"]),
    );
    assert_eq!(n, 20);
    check_find(
        &mut c,
        "special",
        &["-iname", "*.TXT", "-size", "+0", "-type", "f"],
        &s(&["-iname", "*.TXT", "-size", "+0c", "-type", "f"]),
    );
    for (dir, flag, p) in patterns() {
        check_find(&mut c, dir, &[flag, &p], &[flag.to_string(), p.clone()]);
    }
}

/// F43
#[test]
fn walk_counts_fast() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    let r = c.cmd("find /fanout");
    r.expect_ok("find /fanout");
    let s = r.stats();
    assert_eq!(
        s.requests, 8_004,
        "find /fanout: expected 8,004 requests (2,001 listings × 4)\n{:?}",
        s.per_command
    );
    same_paths(
        "find /fanout",
        &r.paths(),
        &server_find(samba(), "fanout", &[]),
    );
    assert_eq!(r.of("entry").len(), 4_000);
    assert!(
        s.peak_outstanding <= 64 && s.peak_outstanding > 4,
        "find /fanout: peak outstanding {}, expected 5 to 64",
        s.peak_outstanding
    );
    assert_eq!(s.connections, 0, "find /fanout: 0 new connections");
    let r = c.cmd("find /chain");
    r.expect_ok("find /chain");
    r.expect("find /chain: 61 listings one after another", 244, 61);
    let entries = r.of("entry");
    let bottom: Vec<&&Value> = entries
        .iter()
        .filter(|e| e["name"] == "bottom.txt")
        .collect();
    assert_eq!(bottom.len(), 1);
    assert_eq!(bottom[0]["depth"], 61);
    let r = c.cmd("find /many -name 'f0999?'");
    r.expect_ok("find /many -name");
    assert_eq!(r.of("entry").len(), 10);
    assert_eq!(
        r.stats().requests,
        9,
        "find /many -name: a filter costs no request\n{}",
        context(r.stats())
    );
    for line in ["find /huge", "tree /huge"] {
        let mut c = Client::tapped(fast());
        c.cmd("use share").expect_ok("use share");
        let r = c.cmd(line);
        r.expect_ok(line);
        assert_eq!(r.of("entry").len(), 100_000, "{line}");
        assert_eq!(
            r.stats().requests,
            19,
            "{line}\n{:?}",
            r.stats().per_command
        );
        let kb = c.peak_rss_kb();
        println!("{line}: peak RSS {kb} kB");
        assert!(
            kb <= 64_000,
            "{line}: peak RSS {kb} kB, expected at most 64 MB"
        );
    }
}

/// F44
#[test]
fn walk_past_denied() {
    let mut c = at_root();
    let want = server_find(samba(), "tree", &[]);
    for line in ["find /tree", "tree /tree"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        let d = denied(&r);
        assert_eq!(d.len(), 1, "{line}: {d:?}");
        assert_eq!(d[0]["path"], "/tree/forbidden");
        let sum = walk_summary(&r).unwrap();
        assert_eq!(
            (sum["denied"].as_u64(), sum["directories"].as_u64()),
            (Some(1), Some(341))
        );
        same_paths(line, &r.paths(), &want);
        assert!(r.paths().contains(b"tree/forbidden".as_slice()));
        let o = run_fast(&["--json", "-C", "use share", "-C", line], None);
        assert_eq!(o.code, 0, "{line}: exit code");
    }
    for (line, kind) in [
        ("find /forbidden", "access_denied"),
        ("find /traverse-only", "access_denied"),
        ("tree /forbidden", "access_denied"),
        ("find /nope", "not_found"),
        ("find /sizes/1.bin", "not_a_directory"),
        ("tree /sizes/1.bin", "not_a_directory"),
    ] {
        let r = c.cmd(line);
        assert_eq!(r.kind().as_deref(), Some(kind), "{line}: {:?}", r.lines);
        assert!(walk_summary(&r).is_none(), "{line}");
    }
    let o = run_fast(
        &["--json", "-C", "use share", "-C", "find /forbidden"],
        None,
    );
    assert_eq!(o.code, 1);
    let o = run_fast(&["-C", "use share", "-C", "find /tree"], None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stderr.contains("rsmb: /tree/forbidden: access denied"),
        "{}",
        o.stderr
    );
    assert!(
        o.stdout.lines().all(|l| l.starts_with("/tree/")),
        "only paths on stdout"
    );
    let o = run_fast(&["-C", "use share", "-C", "tree /tree"], None);
    assert!(
        o.stdout.contains("forbidden/ [access denied]"),
        "{}",
        o.stdout
    );
    let o = run_fast(&["--json", "-C", "find /tree"], None);
    assert_eq!(o.code, 2, "find before use: {}", o.stdout);
    assert_eq!(error_kind(&o), "usage");
}

/// F45
#[test]
fn walk_link_loops() {
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    for line in ["find /loops", "tree /loops"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        r.expect(line, 4, 1);
        let names: Vec<&str> = r
            .of("entry")
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, ["file.txt", "self", "up"], "{line}");
        let sk = skipped(&r);
        let got: Vec<(&str, &str, Option<&str>)> = sk
            .iter()
            .map(|v| {
                (
                    v["path"].as_str().unwrap(),
                    v["reason"].as_str().unwrap(),
                    v["same_as"].as_str(),
                )
            })
            .collect();
        // `up` leads to the share root, which a walk of /loops does not list: no path is named.
        assert_eq!(
            got,
            [
                ("/loops/self", "already_listed", Some("/loops")),
                ("/loops/up", "outside", None)
            ],
            "{line}"
        );
        let sum = walk_summary(&r).unwrap();
        assert_eq!(
            (sum["directories"].as_u64(), sum["skipped"].as_u64()),
            (Some(1), Some(2))
        );
        let o = run_fast(&["--json", "-C", "use share", "-C", line], None);
        assert_eq!(o.code, 0, "{line}: exit code");
    }
    let r = c.cmd("find /loops -maxdepth 1");
    r.expect_ok("-maxdepth 1");
    assert_eq!(r.of("entry").len(), 3);
    assert!(skipped(&r).is_empty(), "{:?}", r.lines);
    for line in ["find /loops", "tree /loops"] {
        let o = run_fast(&["-C", "use share", "-C", line], None);
        assert_eq!(o.code, 0);
        assert!(
            o.stderr.contains("already listed as /loops\n"),
            "{}",
            o.stderr
        );
        assert!(
            o.stderr
                .contains("/loops/up: not entered: leads above the start, to a directory this walk does not list\n"),
            "{}",
            o.stderr
        );
        assert!(!o.stderr.contains("already listed as /\n"), "{}", o.stderr);
        assert!(!o.stderr.contains("link"), "{}", o.stderr);
        if line.starts_with("tree") {
            assert!(
                o.stdout.contains("self/ [already listed as /loops]"),
                "{}",
                o.stdout
            );
            assert!(
                o.stdout.contains("up/ [leads above the start"),
                "{}",
                o.stdout
            );
        }
    }
    // Started at the link: `self` is the start itself, `up` is above the real /loops, which is
    // not /loops/self's textual parent.
    for line in ["find /loops/self", "tree /loops/self"] {
        let r = c.cmd(line);
        r.expect_ok(line);
        let got: Vec<(&str, &str, Option<&str>)> = skipped(&r)
            .iter()
            .map(|v| {
                (
                    v["path"].as_str().unwrap(),
                    v["reason"].as_str().unwrap(),
                    v["same_as"].as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("/loops/self/self", "already_listed", Some("/loops/self")),
                ("/loops/self/up", "outside", None)
            ],
            "{line}"
        );
    }
}

/// F46
#[test]
fn walk_objects() {
    let mut c = at_root();
    let rank = |t: &str| match t {
        "entry" => 0,
        "denied" => 1,
        "skipped" => 2,
        "walk_summary" => 3,
        t => panic!("unexpected object {t}"),
    };
    for dir in ["/tree", "/loops", "/special"] {
        for cmd in ["find", "tree"] {
            let line = format!("{cmd} {dir}");
            let r = c.cmd(&line);
            r.expect_ok(&line);
            let ranks: Vec<i32> = r
                .lines
                .iter()
                .map(|v| rank(v["type"].as_str().unwrap()))
                .collect();
            assert!(
                ranks.is_sorted(),
                "{line}: entries, denied, skipped, summary: {ranks:?}"
            );
            for e in r.of("entry") {
                assert!(e["name"].is_string() && e["path"].is_string(), "{e}");
                assert!(
                    e["is_dir"].is_boolean() && e["size"].is_u64() && e["depth"].is_u64(),
                    "{e}"
                );
                assert!(e["mtime"].is_string() || e["mtime"].is_null(), "{e}");
                assert!(
                    e["file_id"]
                        .as_str()
                        .unwrap()
                        .bytes()
                        .all(|b| b.is_ascii_digit()),
                    "{e}"
                );
            }
            for d in denied(&r) {
                assert!(d["path"].is_string(), "{d}");
            }
            for s in skipped(&r) {
                assert!(s["path"].is_string() && s["reason"].is_string(), "{s}");
                assert!(s["same_as"].is_string() || s["same_as"].is_null(), "{s}");
            }
            let sum = walk_summary(&r).unwrap();
            assert_eq!(sum["command"], cmd);
            assert_eq!(sum["path"], dir);
            for f in [
                "directories",
                "entries",
                "matched",
                "denied",
                "skipped",
                "failed",
            ] {
                assert!(sum[f].is_u64(), "{f}: {sum}");
            }
            assert!(sum["seconds"].is_f64() || sum["seconds"].is_u64(), "{sum}");
            assert_eq!(
                sum["matched"].as_u64().unwrap() as usize,
                r.of("entry").len()
            );
            assert!(sum["entries"].as_u64() >= sum["matched"].as_u64());
        }
    }
    let r = c.cmd("find /tree -name nomatch");
    r.expect_ok("nomatch");
    let sum = walk_summary(&r).unwrap();
    assert_eq!(
        (sum["matched"].as_u64(), sum["entries"].as_u64()),
        (Some(0), Some(682))
    );
}

/// F47
#[test]
fn names_in_patterns() {
    let mut c = at_root();
    let n = |c: &mut Client, line: &str| {
        let r = c.cmd(line);
        r.expect_ok(line);
        r.paths()
    };
    // 112 at depth 1 (the directory and 111 files); 114 in all of cjk/ (docs/plans/stage-3.md,
    // deviations).
    let want = server_find(
        samba(),
        "cjk",
        &["-maxdepth", "1", "-name", "東京\u{3000}大阪*"],
    );
    assert_eq!(want.len(), 112);
    for line in [
        "find /cjk -maxdepth 1 -name '東京\u{3000}大阪*'",
        "find /cjk -maxdepth 1 -name 東京\u{3000}大阪*",
    ] {
        same_paths(
            &format!("{line}: U+3000 does not split"),
            &n(&mut c, line),
            &want,
        );
    }
    let one = |c: &mut Client, line: &str, want: &str| {
        let got = n(c, line);
        assert_eq!(got.len(), 1, "{line}: {got:?}");
        assert!(got.contains(want.as_bytes()), "{line}: {got:?}");
    };
    one(
        &mut c,
        "find /special -name \"it's.txt\"",
        "special/it's.txt",
    );
    for line in [
        "find /special -name '\\[abc\\].txt'",
        "find /special -name \\\\[abc\\\\].txt",
        "find /special -name '[[]abc[]].txt'",
    ] {
        one(&mut c, line, "special/[abc].txt");
    }
    one(&mut c, "find /special -name -rf", "special/-rf");
    one(
        &mut c,
        "find /special -name '--help*'",
        "special/--help.txt",
    );
    let leaf = n(&mut c, "find \"/special/dir with space\" -name leaf.txt");
    assert_eq!(leaf.len(), 1, "{leaf:?}");
    let (nfc, nfd) = (fixture("KO_NFC"), fixture("KO_NFD"));
    for (mine, other) in [(&nfc, &nfd), (&nfd, &nfc)] {
        let got = n(&mut c, &format!("find /cjk -maxdepth 1 -name {}", q(mine)));
        assert!(got.contains(format!("cjk/{mine}").as_bytes()), "{got:?}");
        assert!(!got.contains(format!("cjk/{other}").as_bytes()), "{got:?}");
    }
    let r = c.cmd("find /special -name 'a/b'");
    assert_eq!(r.kind().as_deref(), Some("usage"));
}

/// F50: the same output on every run (queue order).
#[test]
fn walk_is_the_same_every_run() {
    let mut c = at_root();
    let want = [
        "/links/a",
        "/links/b",
        "/links/a/to-c",
        "/links/b/c",
        "/links/a/to-c/file.txt",
    ];
    for cmd in ["find", "tree"] {
        let mut first: Option<Vec<String>> = None;
        for run in 0..10 {
            let line = format!("{cmd} /links");
            let r = c.cmd(&line);
            r.expect_ok(&line);
            let paths: Vec<String> = r
                .of("entry")
                .iter()
                .map(|e| e["path"].as_str().unwrap().to_string())
                .collect();
            let mut set = paths.clone();
            set.sort();
            let mut w: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            w.sort();
            assert_eq!(set, w, "{line} run {run}");
            if cmd == "find" {
                assert_eq!(paths, want, "{line} run {run}: breadth first");
            }
            match &first {
                None => first = Some(paths),
                Some(f) => assert_eq!(&paths, f, "{line} run {run}: the same as run 0"),
            }
            let sk = skipped(&r);
            assert_eq!(sk.len(), 1, "{line} run {run}: {sk:?}");
            assert_eq!(
                (
                    sk[0]["path"].as_str(),
                    sk[0]["reason"].as_str(),
                    sk[0]["same_as"].as_str()
                ),
                (
                    Some("/links/b/c"),
                    Some("already_listed"),
                    Some("/links/a/to-c")
                ),
                "{line} run {run}"
            );
            assert_eq!(walk_summary(&r).unwrap()["directories"], 4);
        }
    }
    let lines = |c: &mut Client| -> Vec<String> {
        let r = c.cmd("find /fanout");
        r.expect_ok("find /fanout");
        r.of("entry")
            .iter()
            .map(|e| e["path"].as_str().unwrap().to_string())
            .collect()
    };
    let (a, b) = (lines(&mut c), lines(&mut c));
    assert_eq!(a.len(), 4_000);
    assert_eq!(a, b, "find /fanout twice: the same lines in the same order");
    let dirs: Vec<String> = (1..=2000).map(|i| format!("/fanout/d{i:04}")).collect();
    let files: Vec<String> = dirs.iter().map(|d| format!("{d}/file.txt")).collect();
    assert_eq!(
        a,
        [dirs, files].concat(),
        "the directories by name, then each one's file.txt"
    );
}

/// F49
#[test]
fn walk_encrypted() {
    let mut c = Client::to(fast(), &[]);
    c.cmd("use encrypted").expect_ok("use encrypted");
    let r = c.cmd("find /tree");
    r.expect_ok("find /tree on encrypted");
    same_paths(
        "find /tree on encrypted",
        &r.paths(),
        &server_find(samba(), "tree", &[]),
    );
    let r = c.cmd("tree -L 1 /tree");
    r.expect_ok("tree -L 1 /tree on encrypted");
    same_paths(
        "tree -L 1 on encrypted",
        &r.paths(),
        &server_find(samba(), "tree", &["-maxdepth", "1"]),
    );
}

/// I41: what a walk lists feeds the cache that Tab reads.
#[test]
fn walk_feeds_the_cache() {
    let dir = tmp("walk_feeds_the_cache");
    let s = Scratch::new(fast());
    s.sh("mkdir -p cs/Foo cs/foo && echo x > cs/Foo/x && echo y > cs/foo/y");
    let (mut p, tap, _) = Pty::tapped(fast(), &[], &dir.join("history"));
    p.line("use share", &p1());
    p.line("tree -L 2 /tree", &p1());
    take(&tap);
    let st = tab(&mut p, &tap, "cd /tree/d0/", None);
    expect_stats(&st, "Tab below a walked directory", 0, 0);
    p.send(b"\t");
    p.expect("d3/", WAIT);
    expect_stats(&take(&tap), "the second Tab there", 0, 0);
    let st = tab(&mut p, &tap, "cd /tree/d0/d0/", None);
    expect_stats(&st, "Tab at depth 2 (drawn, not listed): one listing", 5, 1);
    p.send(CTRL_U);
    p.line("find /cjk -maxdepth 1 -name nomatch", &p1());
    take(&tap);
    expect_stats(&tab(&mut p, &tap, "cd /cjk/", None), "Tab in /cjk/", 0, 0);
    expect_stats(
        &tab(&mut p, &tap, "find -name ", None),
        "Tab after a flag that takes a value",
        0,
        0,
    );
    p.send(CTRL_U);
    let here = format!("//{}/share{}> ", fast().host(true), s.remote());
    p.line(&format!("cd {}", s.remote()), &here);
    p.line("find .", &here);
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "cd c", Some("cd cs/")),
        "Tab in the walked scratch directory",
        0,
        0,
    );
    // cs/Foo and cs/foo are two directories with one folded cache key: the walk stores neither.
    expect_stats(
        &tab(&mut p, &tap, "ls cs/Foo/", Some("ls cs/Foo/x ")),
        "Tab in cs/Foo/: not stored by the walk",
        5,
        1,
    );
    p.send(CTRL_U);
    p.line("find .", &here);
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "ls cs/foo/", Some("ls cs/foo/y ")),
        "Tab in cs/foo/: not stored by the walk",
        5,
        1,
    );
    p.send(CTRL_U);
    p.line("mkdir x", &here);
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "cd x", Some("cd x/")),
        "Tab after mkdir: the walk's listing was dropped",
        5,
        1,
    );
    p.send(CTRL_U);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

// ---------------------------------------------------------------------------------------------
// Stage 4: the proxy (docs/plans/stage-4.md 6.4). Run twice like everything here; a test that
// passes its own --proxy builds its whole command line and never uses the route (finding 4).

const PROXY: &str = "10.213.45.20";

/// `--proxy <url> -H <host>`, the login, `-N`, `extra`, `--json` if asked; nothing from `route()`.
fn with_proxy(url: &str, host: &str, json: bool, extra: &[&str]) -> Output {
    let mut a = vec!["--proxy", url, "-H", host];
    a.extend(LOGIN);
    a.push("-N");
    a.extend(extra);
    if json {
        a.push("--json");
    }
    run(&a, None)
}

/// F61
#[test]
fn proxy_urls_at_startup() {
    let user256 = format!("socks5://{}:pw@{PROXY}:1080", "u".repeat(256));
    let refused = [
        "socks4://10.213.45.20:1080",
        "socks4a://10.213.45.20:1080",
        "socks://10.213.45.20:1080",
        "http://10.213.45.20:1080",
        "https://10.213.45.20:1080",
        "SOCKS5://10.213.45.20:1080",
        "10.213.45.20:1080",
        "socks5://10.213.45.20",
        "socks5://10.213.45.20:99999",
        "socks5://10.213.45.20:1080/x",
        "",
        "socks5://:pw@10.213.45.20:1080",
        "socks5://user:@10.213.45.20:1080",
        &user256,
        "socks5://user@10.213.45.20:1080",
        "socks5://tester:pa%ss@10.213.45.20:1081",
        "socks5://user1:secretpw@10.213.45.20",
        "socks5://alice:hunter2",
    ];
    for url in refused {
        for json in [false, true] {
            let o = with_proxy(url, PRIVATE, json, &["-C", "pwd"]);
            assert_eq!(o.code, 2, "{url:?} json {json}: {} {}", o.stdout, o.stderr);
            if json {
                assert_eq!(o.stderr, "", "{url:?}: stderr stays empty");
                let v = o.json();
                assert_eq!(v.len(), 1, "{url:?}: {}", o.stdout);
                assert_eq!(v[0]["kind"], "usage", "{url:?}");
                let m = v[0]["message"].as_str().unwrap();
                assert!(m.contains("expected socks5://"), "{url:?}: {m}");
            } else {
                assert!(
                    o.stderr.contains("expected socks5://"),
                    "{url:?}: {}",
                    o.stderr
                );
            }
            for secret in ["user1", "secretpw", "alice", "hunter2"] {
                assert!(
                    !o.stdout.contains(secret) && !o.stderr.contains(secret),
                    "{url:?}: the output holds {secret}"
                );
            }
        }
    }
    let url = format!("socks5://{PROXY}:1080");
    let o = with_proxy(&url, &"a".repeat(256), true, &["-C", "pwd"]);
    assert_eq!(o.code, 2, "-H of 256 bytes: {}", o.stdout);
    assert_eq!(error_kind(&o), "usage");
    assert!(o.stdout.contains("255"), "{}", o.stdout);
    let o = with_proxy(&url, &"a".repeat(255), true, &["-C", "pwd"]);
    assert_eq!(
        o.code, 3,
        "-H of 255 bytes: the proxy is asked: {}",
        o.stdout
    );
    assert_eq!(error_kind(&o), "connect");
    let message = o.json()[0]["message"].as_str().unwrap().to_string();
    assert!(
        message.starts_with("proxy "),
        "-H of 255 bytes: the proxy was asked and the message says so: {message}"
    );
    for url in [
        format!("socks5://{PROXY}:1080"),
        format!("socks5h://{PROXY}:1080"),
        format!("socks5://{PROXY}:1080/"),
        format!("socks5://tester:testpass@{PROXY}:1081"),
        format!("socks5://tester:testpass@{PROXY}:1080"),
    ] {
        let o = with_proxy(&url, PRIVATE, false, &["-C", "use share", "-C", "pwd"]);
        assert_eq!(o.code, 0, "{url}: {}", o.stderr);
        assert_eq!(o.stdout, "//fileserver.rsmb.test/share/\n", "{url}");
    }
    let o = run(&["--help-all"], None);
    assert!(
        o.stdout.contains("\nPROXY\n"),
        "help all has the topic PROXY"
    );
    // the manual is wrapped to 80 columns: compare with runs of white space as one space
    let squashed = o.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(squashed.contains(
        "rsmb --proxy socks5://127.0.0.1:1080 -H fileserver.example -u alice -d CORP -N -C 'use \
         docs' -C 'ls'"
    ));
}

/// F62: no proxy is taken from the environment.
#[test]
fn environment_is_not_read() {
    let bad = "socks5://127.0.0.1:1";
    let env = [
        ("ALL_PROXY", bad),
        ("all_proxy", bad),
        ("SOCKS_PROXY", bad),
        ("RSMB_PROXY", bad),
    ];
    let args = fast_args(&["-C", "use share", "-C", "ls /sizes"]);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = run_env(&a, None, &env);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stdout.lines().count(), 11, "{}", o.stdout);
}

/// The lines of a pty transcript that start with `{`, parsed; panics on one that does not parse.
fn json_lines(t: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(t)
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON: {l:?}: {e}")))
        .collect()
}

/// I61: --json never starts the line editor; typed lines follow the non-interactive rules.
#[test]
fn json_never_starts_the_shell() {
    let dir = tmp("json_never_starts_the_shell");
    let hist = dir.join("history");
    let route = fast().args(None);
    let start = |extra: &[&str]| {
        let mut a: Vec<&str> = route.iter().map(String::as_str).collect();
        a.push("--json");
        a.extend(extra);
        Pty::start(&a, &hist)
    };
    // -C: the commands, then exit, as with -N.
    let (code, t) = start(&["-C", "pwd"]).finish(WAIT);
    assert_eq!(code, 0, "{}", t.escape_ascii());
    assert!(!t.contains(&0x1b), "an ESC byte: {}", t.escape_ascii());
    assert!(
        !t.windows(2).any(|w| w == b"> "),
        "a prompt: {}",
        t.escape_ascii()
    );
    let v = json_lines(&t);
    let types: Vec<&str> = v.iter().map(|o| o["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["pwd", "done"], "{}", t.escape_ascii());
    // Lines typed at the terminal, then Ctrl-D.
    let mut p = start(&[]);
    p.send(b"pwd\r");
    p.expect("\"type\":\"done\"", WAIT);
    p.send(b"\x04");
    let (code, t) = p.finish(WAIT);
    assert_eq!(code, 0, "Ctrl-D: {}", t.escape_ascii());
    assert!(!t.contains(&0x1b), "an ESC byte: {}", t.escape_ascii());
    let s = String::from_utf8_lossy(&t);
    assert!(!s.contains("logged in as"), "a banner: {s}");
    let types: Vec<String> = json_lines(&t)
        .iter()
        .map(|o| o["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(types, ["pwd", "done"], "{s}");
    // The first failing typed line ends the run, as from a pipe; with --keep-going both run.
    let mut p = start(&[]);
    p.send(b"use share\r");
    p.expect("\"type\":\"done\"", WAIT);
    p.send(b"cd /nope\rpwd\r");
    let (code, t) = p.finish(WAIT);
    assert_eq!(code, 1, "{}", t.escape_ascii());
    let v = json_lines(&t);
    assert!(v.iter().all(|o| o["type"] != "pwd"), "pwd ran: {v:?}");
    assert!(v.iter().any(|o| o["kind"] == "not_found"), "{v:?}");
    let mut p = start(&["--keep-going"]);
    p.send(b"use share\r");
    p.expect("\"type\":\"done\"", WAIT);
    p.send(b"cd /nope\rpwd\r");
    p.expect("\"type\":\"pwd\"", WAIT);
    p.send(b"\x04");
    let (code, t) = p.finish(WAIT);
    assert_eq!(
        code,
        1,
        "--keep-going: the first failure's code: {}",
        t.escape_ascii()
    );
    assert!(json_lines(&t).iter().any(|o| o["type"] == "pwd"));
    // Nothing asks to overwrite.
    std::fs::write(dir.join("1.bin"), b"keep").unwrap();
    let lcd = format!("lcd {}", q(&dir.to_string_lossy()));
    let (code, t) = start(&["-C", &lcd, "-C", "use share", "-C", "get /sizes/1.bin"]).finish(WAIT);
    assert_eq!(code, 1, "{}", t.escape_ascii());
    let s = String::from_utf8_lossy(&t);
    assert!(!s.contains("Overwrite?"), "{s}");
    assert!(
        json_lines(&t).iter().any(|o| o["kind"] == "already_exists"),
        "{s}"
    );
    assert_eq!(std::fs::read(dir.join("1.bin")).unwrap(), b"keep");
}

// ---------------------------------------------------------------------------------------------
// Stage 5: Kerberos (docs/plans/stage-5.md 7.4). `dc()` is the DC by address direct, and the DC
// by name through the proxy in the proxied run.

/// A krb5.conf that names the DC's KDC by address, which the host reaches (a direct TGT-only run
/// needs it: without it rsmb asks `rsmb.test`, which only the proxy resolves).
fn dc_conf(dir: &std::path::Path) -> String {
    krb5_conf(dir, DC).to_str().unwrap().to_string()
}

/// One-shot `-k` run to `dc()` with KRB5CCNAME=`cache`, KRB5_CONFIG=`conf`, then `extra`.
fn run_dc(route: &[String], cache: &str, conf: &str, extra: &[&str]) -> Output {
    let mut a: Vec<&str> = route.iter().map(String::as_str).collect();
    a.push("-k");
    a.extend(extra);
    run_env(&a, None, &[("KRB5CCNAME", cache), ("KRB5_CONFIG", conf)])
}

/// The one `error` object of a failed start-up: exit 3, `command` null, no `done`.
fn startup_error(o: &Output, what: &str) -> Value {
    assert_eq!(o.code, 3, "{what}: {}", o.stdout);
    let v = o.json();
    assert_eq!(v.len(), 1, "{what}: {}", o.stdout);
    assert_eq!(v[0]["type"], "error", "{what}");
    assert_eq!(v[0]["command"], Value::Null, "{what}");
    v[0].clone()
}

/// F71: a Kerberos session with the service ticket in the cache: 2 requests at start-up, a
/// listing, a read and a signed write with checksums, the encrypted share. Direct, also a
/// TGT-only start-up in the tap (the KDC's connection goes beside it).
#[test]
fn kerberos_login() {
    let ep = dc();
    let dir = tmp("kerberos_login");
    let cache = krb_cache(&dir, "svc.cc", "-f", "tester", &[ep.spn(true)]);
    let s = Scratch::new(ep);
    let mut c = Client::kerberos(ep, &cache, &[], &[]);
    let st = c.startup.stats();
    c.startup
        .expect("Kerberos start-up: NEGOTIATE 1, SESSION_SETUP 1", 2, 2);
    assert_eq!(count(st, "NEGOTIATE"), 1);
    assert_eq!(count(st, "SESSION_SETUP"), 1);
    assert_eq!(st.connections, 1, "{}", context(st));
    let r = c.cmd("use share");
    r.expect_ok("use share");
    r.expect("use share", 1, 1);
    let r = c.cmd("ls /");
    r.expect_ok("ls /");
    let names: BTreeSet<Vec<u8>> = ["hello.txt", "sub", "upload"]
        .iter()
        .map(|n| n.as_bytes().to_vec())
        .collect();
    assert_eq!(r.names(), names);
    c.cmd(&format!("lcd {}", q(&dir.to_string_lossy())))
        .expect_ok("lcd");
    c.cmd("get /sub/1m.bin").expect_ok("get");
    assert_eq!(
        local_sha(&dir.join("1m.bin")),
        server_sha(SAMBA_DC, "/srv/share/sub/1m.bin")
    );
    make_file(&dir.join("up.bin"), 300_000, 7);
    c.cmd(&format!("put up.bin {}/up.bin", s.remote()))
        .expect_ok("put");
    assert_eq!(
        server_sha(SAMBA_DC, &format!("{}/up.bin", s.server())),
        local_sha(&dir.join("up.bin"))
    );
    c.cmd("use encrypted").expect_ok("use encrypted");
    let r = c.cmd("cat /hello.txt");
    r.expect_ok("cat on encrypted");
    assert_eq!(r.of("content")[0]["text"], "kerberos works\n");
    let (code, err) = c.finish();
    assert_eq!((code, err.as_str()), (0, ""));
    if !ep.proxied() {
        // TGT-only, tapped as 127.0.0.1: the KDC is asked beside the tap (K15, finding 15).
        let tgt = krb_cache(&dir, "tgt.cc", "-f", "tester", &[]);
        let conf = dc_conf(&dir);
        let mut c = Client::kerberos(ep, &tgt, &[], &[("KRB5_CONFIG", &conf)]);
        c.startup.expect(
            "TGT-only start-up in the tap: NEGOTIATE 1, SESSION_SETUP 1",
            2,
            2,
        );
        assert_eq!(c.startup.stats().connections, 1);
        c.cmd("use share").expect_ok("use share");
        c.cmd("ls /").expect_ok("ls / with a TGT-only login");
    }
}

/// F72: TGT-only: the KDC is asked (direct by the address a krb5.conf names; through the proxy by
/// the realm's name, which only the proxy resolves). A TGT that is not forwardable is refused by
/// this KDC, with a message that says what to do.
#[test]
fn kerberos_tgt_only() {
    let ep = dc();
    let dir = tmp("kerberos_tgt_only");
    let route = ep.route(None);
    let conf = if ep.proxied() {
        "/dev/null".to_string()
    } else {
        dc_conf(&dir)
    };
    let f = krb_cache(&dir, "f.cc", "-f", "tester", &[]);
    let o = run_dc(
        &route,
        f.to_str().unwrap(),
        &conf,
        &["--json", "-N", "-C", "use share", "-C", "ls /"],
    );
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(o.json().iter().filter(|v| v["type"] == "entry").count(), 3);
    let nf = krb_cache(&dir, "nf.cc", "-F", "tester", &[]);
    let o = run_dc(
        &route,
        nf.to_str().unwrap(),
        &conf,
        &["--json", "-N", "-C", "ls"],
    );
    let e = startup_error(&o, "not forwardable");
    assert_eq!(e["kind"], "auth");
    let m = e["message"].as_str().unwrap();
    for w in ["KDC_ERR_BADOPTION", "kinit -f", "kvno cifs/"] {
        assert!(m.contains(w), "{w}: {m}");
    }
}

/// F73: what is wrong with the cache is found before anything is dialled: kind auth, exit 3, a
/// message that starts `Kerberos: `, and no connection in the tap.
#[test]
fn kerberos_cache_errors() {
    use std::os::unix::fs::MetadataExt;
    let ep = dc();
    let dir = tmp("kerberos_cache_errors");
    let tap = ep.tap();
    let route = ep.route(Some(&tap));
    let good = krb_cache(&dir, "good.cc", "-f", "tester", &[ep.spn(true)]);
    let good = good.to_str().unwrap().to_string();
    let absent = dir.join("absent").to_str().unwrap().to_string();
    let text = dir.join("text.cc");
    std::fs::write(&text, "not a cache\n").unwrap();
    let text = text.to_str().unwrap().to_string();
    let expired = expired_copy(std::path::Path::new(&good));
    let expired = expired.to_str().unwrap().to_string();
    let me = std::fs::metadata(&dir).unwrap().uid();
    let mut cases: Vec<(String, Vec<&str>, Vec<String>)> = vec![
        (
            absent.clone(),
            vec![],
            vec![absent.clone(), "No such file".into()],
        ),
        ("KCM:".into(), vec![], vec!["'KCM'".into()]),
        ("DIR:/x".into(), vec![], vec!["'DIR'".into()]),
        (text.clone(), vec![], vec!["is not a ticket cache".into()]),
        (expired, vec![], vec!["expired at".into()]),
        (
            good.clone(),
            vec!["-u", "nobody"],
            vec!["tester@RSMB.TEST".into(), "nobody".into()],
        ),
        (
            good.clone(),
            vec!["-d", "OTHER.TEST"],
            vec!["RSMB.TEST".into(), "OTHER.TEST".into()],
        ),
    ];
    // A file of another owner (finding 13).
    match std::fs::metadata("/etc/hostname").map(|m| m.uid()) {
        Ok(owner) if owner != me && me != 0 => cases.push((
            "/etc/hostname".into(),
            vec![],
            vec![format!("belongs to uid {owner}"), "own user only".into()],
        )),
        _ => println!("skipped: /etc/hostname is missing or belongs to the user running the test"),
    }
    for (cache, extra, words) in &cases {
        for json in [true, false] {
            let mut x = extra.clone();
            x.extend(["-N", "-C", "pwd"]);
            if json {
                x.push("--json");
            }
            let o = run_dc(&route, cache, "/dev/null", &x);
            let what = format!("{cache} {extra:?} json {json}");
            let m = if json {
                let e = startup_error(&o, &what);
                assert_eq!(e["kind"], "auth", "{what}");
                e["message"].as_str().unwrap().to_string()
            } else {
                assert_eq!(o.code, 3, "{what}: {}", o.stderr);
                o.stderr
                    .strip_prefix("rsmb: ")
                    .unwrap_or(&o.stderr)
                    .to_string()
            };
            assert!(m.starts_with("Kerberos: "), "{what}: {m}");
            for w in words {
                assert!(m.contains(w.as_str()), "{what}: {w:?} not in {m}");
            }
            assert!(!m.contains("KRB5CCNAME is not set"), "{what}: {m}");
        }
    }
    assert_eq!(take(&tap).connections, 0, "nothing was dialled");
    let o = run_dc(
        &route,
        &good,
        "/dev/null",
        &["-u", "TESTER", "-d", "rsmb.test", "-N", "-C", "pwd"],
    );
    assert_eq!(o.code, 0, "-u and -d in another case: {}", o.stderr);
}

/// F73b: a cache whose name is not UTF-8 is the one KRB5CCNAME names (it was taken as unset, and
/// the default file used); a cache that is not a regular file (a FIFO, a directory) is refused at
/// once, where `open` on a FIFO waited for good and ignored SIGINT.
#[test]
fn kerberos_cache_odd_files() {
    use std::os::unix::ffi::OsStrExt;
    let ep = dc();
    let dir = tmp("kerberos_cache_odd_files");
    let route = ep.route(None);
    let good = krb_cache(&dir, "good.cc", "-f", "tester", &[ep.spn(false)]);
    let odd = dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9.cc"));
    std::fs::copy(&good, &odd).unwrap();
    let o = Command::new(BIN)
        .args(&route)
        .args(["-k", "--json", "-N", "-C", "use share", "-C", "ls /"])
        .env("KRB5CCNAME", &odd)
        .env("KRB5_CONFIG", "/dev/null")
        .env("RSMB_HISTORY", "")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(out.matches("\"type\":\"entry\"").count(), 3, "{out}");

    let fifo = dir.join("fifo");
    let made = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo");
    for cache in [fifo, dir.clone()] {
        let shown = cache.to_str().unwrap();
        let o = run_dc(&route, shown, "/dev/null", &["--json", "-N", "-C", "pwd"]);
        assert!(o.took < Duration::from_secs(5), "{shown}: {:?}", o.took);
        let e = startup_error(&o, shown);
        assert_eq!(e["kind"], "auth", "{shown}");
        let m = e["message"].as_str().unwrap();
        assert!(
            m.starts_with("Kerberos: ticket cache ")
                && m.contains(shown)
                && m.contains("not a regular file"),
            "{shown}: {m}"
        );
    }
}

/// F74: `-k` excludes -p and --no-pass, asks for no password, reads the cache it is told to before
/// it dials; the manual has the topic and the flags.
#[test]
fn kerberos_usage() {
    for pw in [&["-p", "x"][..], &["--no-pass"][..]] {
        let mut a = vec!["-H", "h", "-k"];
        a.extend(pw);
        let o = run(&a, None);
        assert_eq!(o.code, 2, "{pw:?}: {}", o.stderr);
        a.push("--json");
        let o = run(&a, None);
        assert_eq!(o.code, 2, "{pw:?}");
        assert_eq!(error_kind(&o), "usage", "{pw:?}");
    }
    // The harness's KRB5CCNAME: a path the test controls, read before port 1 is dialled.
    let o = run(
        &[
            "-H",
            "127.0.0.1",
            "-P",
            "1",
            "-k",
            "--json",
            "-N",
            "-C",
            "pwd",
        ],
        None,
    );
    let e = startup_error(&o, "the harness's cache");
    assert_eq!(e["kind"], "auth");
    let m = e["message"].as_str().unwrap();
    assert!(
        m.contains(&format!("ticket cache {NO_CACHE}")) && m.contains("No such file"),
        "{m}"
    );
    let manual = run(&["--help-all"], None).stdout;
    assert!(manual.contains("\nKERBEROS\n"), "the topic");
    assert!(manual.contains("-k, --kerberos"), "the flag");
    let opts = manual_json();
    let keep = opts
        .iter()
        .filter(|v| v["type"] == "option")
        .find(|v| v["flag"].as_str().unwrap().contains("--keep-going"))
        .unwrap();
    assert_eq!(keep["flag"], "--keep-going", "no short form");
}

/// F75: a reconnect reads the cache again: gone, back, another principal.
#[test]
fn kerberos_reconnect_reads_the_cache_again() {
    let ep = dc();
    let dir = tmp("kerberos_reconnect_reads_the_cache_again");
    let cache = krb_cache(&dir, "svc.cc", "-f", "tester", &[ep.spn(true)]);
    let other = krb_cache(&dir, "other.cc", "-f", "kother", &[ep.spn(true)]);
    let away = dir.join("away.cc");
    let mut c = Client::kerberos(ep, &cache, &[], &[]);
    c.cmd("use share").expect_ok("use share");
    c.cmd("ls /").expect_ok("ls /");
    let cut = |c: &mut Client| {
        assert_eq!(c.tap().drop_connections(), 1);
        let deadline = std::time::Instant::now() + WAIT;
        while c.tap().live_connections() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the cut connection stays live"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        take(c.tap());
    };
    std::fs::rename(&cache, &away).unwrap();
    cut(&mut c);
    let r = c.cmd("ls /");
    assert_eq!(r.kind().as_deref(), Some("auth"), "{:?}", r.lines);
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    let path = cache.to_str().unwrap();
    assert!(
        m.contains("reconnect failed: Kerberos: ticket cache") && m.contains(path),
        "{m}"
    );
    assert_eq!(
        r.stats().connections,
        0,
        "a cache that cannot be used costs no connection"
    );
    c.cmd("pwd").expect_ok("pwd");
    std::fs::rename(&away, &cache).unwrap();
    let r = c.cmd("ls /");
    r.expect_ok("ls / with the cache back");
    let st = r.stats();
    assert_eq!(
        st.connections,
        1,
        "exactly 1 new connection\n{}",
        context(st)
    );
    assert_eq!(
        st.requests,
        8,
        "NEGOTIATE, SESSION_SETUP, TREE_CONNECT and the 5 of a listing\n{}",
        context(st)
    );
    assert_eq!(count(st, "SESSION_SETUP"), 1, "{}", context(st));
    std::fs::rename(&other, &cache).unwrap();
    cut(&mut c);
    let r = c.cmd("ls /");
    assert_eq!(r.kind().as_deref(), Some("auth"), "{:?}", r.lines);
    let m = r.of("error")[0]["message"].as_str().unwrap();
    assert!(
        m.contains("kother@RSMB.TEST") && m.contains("'tester'"),
        "{m}"
    );
}

/// F76: the banner says Kerberos (and the proxy through it); nothing asks for a password.
#[test]
fn kerberos_banner() {
    let ep = dc();
    let dir = tmp("kerberos_banner");
    let cache = krb_cache(&dir, "svc.cc", "-f", "tester", &[ep.spn(true)]);
    let tap = ep.tap();
    let args = ep.args(Some(&tap));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut p = Pty::command(
        BIN,
        &args,
        &dir.join("history"),
        &[("KRB5CCNAME", cache.to_str().unwrap())],
    );
    p.expect(&format!("//{}> ", ep.host(true)), WAIT);
    let t = p.transcript();
    assert!(
        // The transcript is escaped: ' and \ come as \' and \\.
        t.contains("logged in as \\'RSMB.TEST\\\\tester\\' (Kerberos) on"),
        "{t}"
    );
    assert!(!t.contains("Password"), "{t}");
    assert_eq!(t.contains("through proxy"), ep.proxied(), "{t}");
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

// ---------------------------------------------------------------------------------------------
// Stage 6: access, and the check of a large put (docs/plans/stage-6.md 7.4)

/// The local files the Stage 6 tests put: `notes.txt` (6 bytes) and `big.bin` (300,000 bytes, so
/// the stream path with its check).
fn stage6_local(dir: &std::path::Path) {
    std::fs::write(dir.join("notes.txt"), b"notes\n").unwrap();
    make_file(&dir.join("big.bin"), 300_000, 6);
}

/// F87: the check before a large put asks for FILE_READ_ATTRIBUTES only (A14): every effect of
/// docs/plans/stage-6.md 2.13 on a file and a directory the user may not read.
#[test]
fn put_over_a_file_without_synchronize() {
    let dir = tmp("put_over_a_file_without_synchronize");
    stage6_local(&dir);
    let s = Scratch::new(fast());
    posix_fixture(&s);
    let srv = |n: &str| format!("{}/{n}", s.server());
    let mut c = tapped_in(&dir, Some(&s.remote()));
    // root's 0600 file in tester's directory: replaceable, not readable (mask RA DE)
    let r = c.cmd("put -f big.bin big.bin");
    r.expect_ok("put -f big.bin big.bin (before A14: access_denied by the check)");
    assert_eq!(
        server_sha(samba(), &srv("big.bin")),
        local_sha(&dir.join("big.bin"))
    );
    assert_eq!(
        (count(r.stats(), "CREATE"), count(r.stats(), "CLOSE")),
        (3, 3),
        "the check (CREATE + CLOSE), the writer, the rename, as transfer_costs counts\n{}",
        context(r.stats())
    );
    // no -f over root's 0600 file: it exists (before A14: access_denied)
    let old = server_sha(samba(), &srv("secret.txt"));
    let r = c.cmd("put big.bin secret.txt");
    assert_eq!(r.kind().as_deref(), Some("already_exists"), "{:?}", r.lines);
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    assert!(m.contains("-f replaces it"), "{m}");
    assert_eq!(server_sha(samba(), &srv("secret.txt")), old);
    // root's 0700 directory: found to be a directory on both paths (before A14: already_exists
    // on the small path, access_denied on the large one)
    for src in ["notes.txt", "big.bin"] {
        let r = c.cmd(&format!("put {src} rootdir700"));
        assert_eq!(
            r.kind().as_deref(),
            Some("is_a_directory"),
            "put {src} rootdir700: {:?}",
            r.lines
        );
        let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
        assert!(m.contains("write '") && m.contains("rootdir700/'"), "{m}");
    }
    no_temps(&s, &dir);
}

/// F84: what `access` costs: the path and its directory in one compound, the cascade and its
/// retries, `-e` as a listing plus 16 paths per compound; `ls` and `cd` cost what they did.
#[test]
fn access_costs() {
    let s = Scratch::new(fast());
    posix_fixture(&s);
    let mut c = Client::tapped(fast());
    c.cmd("use share").expect_ok("use share");
    let sr = s.remote();
    let r = c.cmd("access");
    r.expect_ok("access at the root");
    r.expect("access at the root: CREATE + CLOSE", 2, 1);
    let r = c.cmd("access upload");
    r.expect_ok("access upload");
    r.expect("access upload: the root and upload in one compound", 4, 1);
    let r = c.cmd("access nosuch");
    assert_eq!(r.kind().as_deref(), Some("not_found"), "{:?}", r.lines);
    r.expect("access nosuch", 4, 1);
    let r = c.cmd("access nosuch/x");
    assert_eq!(r.kind().as_deref(), Some("not_found"), "{:?}", r.lines);
    r.expect(
        "access nosuch/x: x asked again after its parent failed",
        6,
        2,
    );
    // Samba lists nothing in a directory it may read but not traverse (it cannot look at the
    // entries): the listing and the directory's own probe (deviation 1 of the Stage 6 plan).
    let r = c.cmd(&format!("access -e {sr}/listonly"));
    r.expect_ok("access -e listonly");
    r.expect("access -e listonly: 5 + 2, nothing listed", 7, 2);
    assert!(r.of("access").is_empty(), "{:?}", r.lines);
    let r = c.cmd(&format!("access {sr}/listonly/a.txt"));
    assert_eq!(r.kind().as_deref(), Some("access_denied"), "{:?}", r.lines);
    r.expect(
        "access listonly/a.txt: the second CREATE of the chain refused",
        4,
        1,
    );
    let r = c.cmd(&format!("access -e {sr}/mixed"));
    r.expect_ok("access -e mixed");
    r.expect(
        "access -e mixed: 5 + 10 + 4 (c.txt and d asked again alone)",
        19,
        3,
    );
    for i in 0..2 {
        let r = c.cmd("access -e /tree");
        r.expect_ok("access -e /tree");
        r.expect(&format!("access -e /tree ({i}): 5 + 14, no cache"), 19, 2);
        assert_eq!(r.of("access").len(), 6);
    }
    let r = c.cmd("access -e /");
    r.expect_ok("access -e /");
    let st = r.stats();
    assert!(
        st.requests == 43 && (2..=3).contains(&st.flights),
        "access -e /: 5 + 2 x 19 in 2 or 3 flights\n{}",
        context(st)
    );
    let r = c.cmd("access -e /tree/d0/file.txt");
    assert_eq!(r.kind().as_deref(), Some("not_a_directory"));
    r.expect("access -e of a file: the listing only", 5, 1);
    let r = c.cmd("ls /tree");
    r.expect_ok("ls /tree");
    r.expect("ls /tree as before this stage", 5, 1);
    let r = c.cmd("cd /tree/d0");
    r.expect_ok("cd /tree/d0");
    r.expect("cd /tree/d0 as before this stage", 2, 1);
    let (code, _) = c.finish();
    assert_eq!(code, 1, "the failed commands");
}

/// F85: the text lines byte for byte, the JSON objects' keys, the working directory, usage.
#[test]
fn access_output() {
    let s = Scratch::new(fast());
    posix_fixture(&s);
    let sr = s.remote();
    let o = run_fast(
        &[
            "-C",
            "use share",
            "-C",
            &format!("access {sr}/mine.txt"),
            "-C",
            &format!("access {sr}/rodir"),
            "-C",
            &format!("access -e {sr}/mixed"),
            "-C",
            "cd /tree",
            "-C",
            "access",
        ],
        None,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        o.stdout,
        format!(
            "-  get put       rm     {sr}/mine.txt\n\
             d  ls  -   -     rmdir  {sr}/rodir/\n\
             -  get put       rm     a.txt\n\
             -  ?   ?         ?      b.fifo  (other)\n\
             -  get put       rm     c.txt\n\
             d  ls  put mkdir rmdir  d/\n\
             d  ls  -   -     -      /tree/\n"
        )
    );
    let mut c = Client::to(fast(), &[]);
    let r = c.cmd("access");
    assert_eq!(
        r.kind().as_deref(),
        Some("usage"),
        "before use: {:?}",
        r.lines
    );
    c.cmd("use share").expect_ok("use share");
    let r = c.cmd(&format!("access -e {sr}"));
    r.expect_ok("access -e S");
    let names: Vec<&str> = r
        .of("access")
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "in the listing's order");
    let by = access_by_name(&r);
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    let common = [
        "type",
        "name",
        "path",
        "is_dir",
        "mask",
        "attributes",
        "error",
    ];
    let want = |extra: &[&str]| -> BTreeSet<String> {
        common.iter().chain(extra).map(|k| k.to_string()).collect()
    };
    let dir = &by["rodir"];
    assert_eq!(keys(dir), want(&["ls", "put", "mkdir", "rmdir"]), "{dir}");
    assert_eq!(dir["is_dir"], true);
    let file = &by["mine.txt"];
    assert_eq!(keys(file), want(&["get", "replace", "rm"]), "{file}");
    for v in [dir, file] {
        assert!(v["mask"].is_u64() && v["attributes"].is_u64(), "{v}");
        assert_eq!(v["error"], Value::Null, "{v}");
    }
    assert_eq!(file["path"], format!("{sr}/mine.txt"));
    assert_eq!(dir["mask"], 0x0013_00a9, "{dir}");
    assert_eq!(file["mask"], 0x0017_019f, "{file}");
    let r = c.cmd("access -e a b");
    assert_eq!(r.kind().as_deref(), Some("usage"), "{:?}", r.lines);
    let r = c.cmd("access -- -e");
    assert_eq!(r.kind().as_deref(), Some("not_found"), "{:?}", r.lines);
    let m = r.of("error")[0]["message"].as_str().unwrap().to_string();
    assert!(m.contains("/-e"), "{m}");
}

/// F86: `access -e` lists like `ls`, so Tab below it costs nothing.
#[test]
fn access_refreshes_the_cache() {
    let dir = tmp("access_refreshes_the_cache");
    let (mut p, tap, _) = Pty::tapped(fast(), &[], &dir.join("history"));
    p.line("use share", &p1());
    p.line("access -e /tree", &p1());
    take(&tap);
    expect_stats(
        &tab(&mut p, &tap, "cd /tree/d", None),
        "Tab after access -e",
        0,
        0,
    );
    p.send(b"\t");
    let shown = p.expect("d3/", WAIT);
    for d in ["d0/", "d1/", "d2/"] {
        assert!(shown.contains(d), "{shown}");
    }
    expect_stats(&take(&tap), "the second Tab", 0, 0);
    p.send(CTRL_U);
    p.send(b"exit\r");
    assert_eq!(p.exit_code(WAIT), 0);
}

/// How a row of a prediction test must come out (docs/plans/stage-6.md 7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// The column says what the command did: true and it succeeded, false and it failed.
    Agree,
    /// A known disagreement of the plan's 2.7 (K1, K2, K3, K4, K7): the column says the opposite.
    K(u8),
    /// The column is null (`?`); the command did what the plan measured.
    Unknown,
}

/// (path below the scratch directory, JSON key, the command line run in the scratch directory,
/// expectation, the outcome measured while planning: None succeeded, else the error kind).
type Row = (
    &'static str,
    &'static str,
    &'static str,
    Expect,
    Option<&'static str>,
);

/// The `access` objects of `lines` (run in a session standing in the scratch directory `sr`), by
/// path below `sr` ("" for `sr` itself). Every `access` line must succeed.
fn access_objects(c: &mut Client, sr: &str, lines: &[String]) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for l in lines {
        let r = c.cmd(l);
        r.expect_ok(l);
        for v in r.of("access") {
            let p = v["path"].as_str().unwrap();
            let rel = p.strip_prefix(sr).unwrap().trim_start_matches('/');
            m.insert(rel.to_string(), v.clone());
        }
    }
    m
}

/// Every mask equals the measured one (a Samba upgrade that changes an answer fails here).
fn check_masks(what: &str, objs: &BTreeMap<String, Value>, masks: &[(&str, u64)]) {
    let mut bad = Vec::new();
    for (path, mask) in masks {
        match objs.get(*path) {
            None => bad.push(format!("{path}: no access object")),
            Some(v) if v["mask"] != *mask => bad.push(format!(
                "{path}: mask {} (measured {mask:#010x}): {v}",
                v["mask"]
            )),
            _ => {}
        }
    }
    assert!(
        bad.is_empty(),
        "{what}: masks differ from docs/plans/stage-6.md 2.4, 2.6, 2.13:\n{}",
        bad.join("\n")
    );
}

/// Runs the rows' lines in order and compares each prediction with what the command did.
fn check_rows(what: &str, c: &mut Client, objs: &BTreeMap<String, Value>, rows: &[Row]) {
    let lines: Vec<String> = rows.iter().map(|r| r.2.to_string()).collect();
    let got = outcomes(c, &lines);
    let mut bad = Vec::new();
    for ((path, key, line, expect, real), out) in rows.iter().zip(&got) {
        let pred = objs
            .get(*path)
            .unwrap_or_else(|| panic!("{what}: no access object for {path}"))[*key]
            .as_bool();
        let fits = match expect {
            Expect::Agree => pred == Some(real.is_none()),
            Expect::K(_) => pred == Some(real.is_some()),
            Expect::Unknown => pred.is_none(),
        };
        if !fits || out.as_deref() != *real {
            bad.push(format!(
                "{line}: {path} {key} = {pred:?}, the command {out:?}; the plan (2.7, 2.13) says \
                 {expect:?} with {real:?}"
            ));
        }
    }
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

const POSIX_MASKS: &[(&str, u64)] = &[
    ("", 0x001f_01ff),
    ("mine.txt", 0x0017_019f),
    ("rm-mine.txt", 0x0017_019f),
    ("rootfile.txt", 0x0013_0089),
    ("rm-rootfile.txt", 0x0013_0089),
    ("secret.txt", 0x0001_0080),
    ("rm-secret.txt", 0x0001_0080),
    ("big.bin", 0x0001_0080),
    ("dosro.txt", 0x0004_0089),
    ("rm-dosro.txt", 0x0004_0089),
    ("rodir", 0x0013_00a9),
    ("rodir/f.txt", 0x0012_0089),
    ("rodir/rm-f.txt", 0x0012_0089),
    ("rodir/ww.txt", 0x0012_019f),
    ("rodir/sub", 0x0012_00a9),
    ("rodir2", 0x0013_00a9),
    ("rodir2/minefile.txt", 0x0016_019f),
    ("rodir2/rm-minefile.txt", 0x0016_019f),
    ("rodir2/minedir", 0x001f_01ff),
    ("sticky", 0x001f_01ff),
    ("sticky/rootfile.txt", 0x0012_0089),
    ("sticky/rm-rootfile.txt", 0x0012_0089),
    ("sticky/mine.txt", 0x0017_019f),
    ("sticky/rm-mine.txt", 0x0017_019f),
    ("sticky/rootsub", 0x0012_00a9),
    ("sticky/minesub", 0x001f_01ff),
    ("sticky/rwxsub", 0x001f_01ff),
    ("emptyroot", 0x0013_00a9),
    ("emptymine", 0x001f_01ff),
    ("listonly", 0x0013_0089),
    ("writeonly", 0x0013_01f6),
    ("rodos", 0x000c_00e9),
    ("rodos/f.txt", 0x0017_019f),
    ("rodos/sub", 0x001f_01ff),
    ("mixed", 0x001f_01ff),
    ("mixed/a.txt", 0x0017_019f),
    ("mixed/c.txt", 0x0017_019f),
    ("mixed/d", 0x001f_01ff),
];

const POSIX_ROWS: &[Row] = &[
    ("", "ls", "ls .", Expect::Agree, None),
    ("", "put", "put notes.txt ./", Expect::Agree, None),
    ("", "mkdir", "mkdir new", Expect::Agree, None),
    ("mine.txt", "get", "get mine.txt dl/", Expect::Agree, None),
    (
        "mine.txt",
        "replace",
        "put -f notes.txt mine.txt",
        Expect::Agree,
        None,
    ),
    ("rm-mine.txt", "rm", "rm rm-mine.txt", Expect::Agree, None),
    (
        "rootfile.txt",
        "get",
        "get rootfile.txt dl/",
        Expect::Agree,
        None,
    ),
    (
        "rootfile.txt",
        "replace",
        "put -f notes.txt rootfile.txt",
        Expect::Agree,
        None,
    ),
    (
        "rm-rootfile.txt",
        "rm",
        "rm rm-rootfile.txt",
        Expect::Agree,
        None,
    ),
    (
        "secret.txt",
        "get",
        "get secret.txt dl/",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "secret.txt",
        "replace",
        "put -f notes.txt secret.txt",
        Expect::Agree,
        None,
    ),
    (
        "rm-secret.txt",
        "rm",
        "rm rm-secret.txt",
        Expect::Agree,
        None,
    ),
    // K6, fixed by A14: the check of a large put opens for the attributes only
    (
        "big.bin",
        "replace",
        "put -f big.bin big.bin",
        Expect::Agree,
        None,
    ),
    ("dosro.txt", "get", "get dosro.txt dl/", Expect::Agree, None),
    (
        "dosro.txt",
        "replace",
        "put -f notes.txt dosro.txt",
        Expect::K(1),
        None,
    ),
    (
        "rm-dosro.txt",
        "rm",
        "rm rm-dosro.txt",
        Expect::Agree,
        Some("other"),
    ),
    ("rodir", "ls", "ls rodir", Expect::Agree, None),
    (
        "rodir",
        "put",
        "put notes.txt rodir/",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir",
        "mkdir",
        "mkdir rodir/new",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir/f.txt",
        "get",
        "get rodir/f.txt dl/",
        Expect::Agree,
        None,
    ),
    (
        "rodir/f.txt",
        "replace",
        "put -f notes.txt rodir/f.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir/rm-f.txt",
        "rm",
        "rm rodir/rm-f.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir/ww.txt",
        "replace",
        "put -f notes.txt rodir/ww.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir/sub",
        "rmdir",
        "rmdir rodir/sub",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir2/minefile.txt",
        "replace",
        "put -f notes.txt rodir2/minefile.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir2/rm-minefile.txt",
        "rm",
        "rm rodir2/rm-minefile.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "rodir2/minedir",
        "rmdir",
        "rmdir rodir2/minedir",
        Expect::Unknown,
        Some("access_denied"),
    ),
    ("sticky", "ls", "ls sticky", Expect::Agree, None),
    (
        "sticky",
        "put",
        "put notes.txt sticky/",
        Expect::Agree,
        None,
    ),
    ("sticky", "mkdir", "mkdir sticky/new", Expect::Agree, None),
    (
        "sticky/rootfile.txt",
        "replace",
        "put -f notes.txt sticky/rootfile.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "sticky/rm-rootfile.txt",
        "rm",
        "rm sticky/rm-rootfile.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "sticky/mine.txt",
        "replace",
        "put -f notes.txt sticky/mine.txt",
        Expect::Agree,
        None,
    ),
    (
        "sticky/rm-mine.txt",
        "rm",
        "rm sticky/rm-mine.txt",
        Expect::Agree,
        None,
    ),
    (
        "sticky/rootsub",
        "rmdir",
        "rmdir sticky/rootsub",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "sticky/minesub",
        "rmdir",
        "rmdir sticky/minesub",
        Expect::Agree,
        None,
    ),
    (
        "sticky/rwxsub",
        "rmdir",
        "rmdir sticky/rwxsub",
        Expect::K(3),
        Some("access_denied"),
    ),
    ("emptyroot", "ls", "ls emptyroot", Expect::Agree, None),
    (
        "emptyroot",
        "put",
        "put notes.txt emptyroot/",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "emptyroot",
        "mkdir",
        "mkdir emptyroot/new",
        Expect::Agree,
        Some("access_denied"),
    ),
    ("emptyroot", "rmdir", "rmdir emptyroot", Expect::Agree, None),
    ("emptymine", "rmdir", "rmdir emptymine", Expect::Agree, None),
    ("listonly", "ls", "ls listonly", Expect::Agree, None),
    (
        "writeonly",
        "ls",
        "ls writeonly",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "writeonly",
        "put",
        "put notes.txt writeonly/",
        Expect::Agree,
        None,
    ),
    (
        "writeonly",
        "mkdir",
        "mkdir writeonly/new",
        Expect::Agree,
        None,
    ),
    // 2.13: a directory's DOS read-only attribute hides the rights and is not enforced
    (
        "rodos",
        "put",
        "put notes.txt rodos/",
        Expect::Unknown,
        None,
    ),
    ("rodos", "mkdir", "mkdir rodos/new", Expect::Unknown, None),
    (
        "rodos/f.txt",
        "replace",
        "put -f notes.txt rodos/f.txt",
        Expect::Unknown,
        None,
    ),
    ("rodos/sub", "rmdir", "rmdir rodos/sub", Expect::Agree, None),
    (
        "rodos2",
        "rmdir",
        "rmdir rodos2",
        Expect::Agree,
        Some("other"),
    ),
    (
        "mixed/c.txt",
        "get",
        "get mixed/c.txt dl/",
        Expect::Agree,
        None,
    ),
];

/// F81: the prediction against the real command on POSIX permissions (`share` of `fast()`), row
/// by row of docs/plans/stage-6.md 2.7 and 2.13, the known disagreements pinned.
#[test]
fn access_predicts_posix() {
    let dir = tmp("access_predicts_posix");
    stage6_local(&dir);
    std::fs::create_dir(dir.join("dl")).unwrap();
    let s = Scratch::new(fast());
    posix_fixture(&s);
    let sr = s.remote();
    let mut c = direct_in(&dir, Some(&sr));
    let mut lines = vec![format!("access {sr}"), format!("access -e {sr}")];
    for d in ["rodir", "rodir2", "sticky", "listonly", "rodos", "mixed"] {
        lines.push(format!("access -e {sr}/{d}"));
    }
    let objs = access_objects(&mut c, &sr, &lines);
    check_masks("POSIX", &objs, POSIX_MASKS);
    assert_eq!(objs["rodos"]["attributes"].as_u64().unwrap() & 0x11, 0x11);
    // the FIFO cuts the chain of -e mixed; c.txt and d were asked again and landed in their places
    let fifo = &objs["mixed/b.fifo"];
    assert_eq!(
        (&fifo["mask"], &fifo["error"]),
        (&Value::Null, &Value::from("other")),
        "{fifo}"
    );
    for n in ["mixed/a.txt", "mixed/c.txt", "mixed/d"] {
        assert_eq!(
            objs[n]["name"],
            n.rsplit('/').next().unwrap(),
            "{}",
            objs[n]
        );
        assert_eq!(objs[n]["error"], Value::Null, "{}", objs[n]);
    }
    // Samba lists nothing in listonly (0744: readable, not traversable), so -e shows no entry;
    // asked alone, an entry is the command's error (deviation 1 of the Stage 6 plan).
    assert!(!objs.contains_key("listonly/a.txt"));
    let r = c.cmd(&format!("access {sr}/listonly/a.txt"));
    assert_eq!(r.kind().as_deref(), Some("access_denied"), "{:?}", r.lines);
    check_rows("POSIX", &mut c, &objs, POSIX_ROWS);
    let r = c.cmd("get listonly/a.txt dl/");
    assert_eq!(r.kind().as_deref(), Some("access_denied"), "{:?}", r.lines);
}

const NT_MASKS: &[(&str, u64)] = &[
    ("A", 0x0012_00ab),
    ("C", 0x0012_00ad),
    ("D", 0x0012_00e9),
    ("D/file.txt", 0x0013_0089),
    ("D/rm-file.txt", 0x0013_0089),
    ("D/denydel.txt", 0x0013_0089),
    ("E", 0x0012_00af),
    ("E/file.txt", 0x0013_0089),
    ("E/rm-file.txt", 0x0013_0089),
    ("E/empty", 0x0012_00a9),
    ("F", 0x0012_00ab),
    ("F/file.txt", 0x0012_0089),
    ("F/rm-file.txt", 0x0012_0089),
    ("G", 0x0012_00ab),
    ("H", 0x0012_00a9),
    ("H/nosync.txt", 0x0000_0081),
    ("H/nosyncdir", 0x0000_0081),
    ("I", 0x0012_00a9),
    ("I/sub", 0x001f_01ff),
    ("I/own.txt", 0x001f_01ff),
    ("I/rm-own.txt", 0x001f_01ff),
    ("J", 0x0012_00ab),
];

const NT_ROWS: &[Row] = &[
    ("A", "put", "put notes.txt A/", Expect::Agree, None),
    (
        "A",
        "mkdir",
        "mkdir A/new",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "C",
        "put",
        "put notes.txt C/",
        Expect::Agree,
        Some("access_denied"),
    ),
    ("C", "mkdir", "mkdir C/new", Expect::Agree, None),
    (
        "D/file.txt",
        "get",
        "get D/file.txt dl/",
        Expect::Agree,
        None,
    ),
    (
        "D/rm-file.txt",
        "rm",
        "rm D/rm-file.txt",
        Expect::Agree,
        None,
    ),
    (
        "D/denydel.txt",
        "rm",
        "rm D/denydel.txt",
        Expect::Agree,
        None,
    ),
    (
        "D/file.txt",
        "replace",
        "put -f notes.txt D/file.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "E/file.txt",
        "replace",
        "put -f notes.txt E/file.txt",
        Expect::Agree,
        None,
    ),
    (
        "E/rm-file.txt",
        "rm",
        "rm E/rm-file.txt",
        Expect::Agree,
        None,
    ),
    (
        "E/empty",
        "rmdir",
        "rmdir E/empty",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "F/file.txt",
        "replace",
        "put -f notes.txt F/file.txt",
        Expect::K(2),
        None,
    ),
    (
        "F/rm-file.txt",
        "rm",
        "rm F/rm-file.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
    ("G", "put", "put notes.txt G/", Expect::Agree, None),
    (
        "G",
        "mkdir",
        "mkdir G/new",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "J",
        "put",
        "put notes.txt J/",
        Expect::K(7),
        Some("access_denied"),
    ),
    (
        "J",
        "mkdir",
        "mkdir J/new",
        Expect::Agree,
        Some("access_denied"),
    ),
    (
        "H/nosync.txt",
        "get",
        "get H/nosync.txt dl/",
        Expect::K(4),
        Some("access_denied"),
    ),
    (
        "H/nosyncdir",
        "ls",
        "ls H/nosyncdir",
        Expect::K(4),
        Some("access_denied"),
    ),
    ("I/sub", "rmdir", "rmdir I/sub", Expect::Unknown, None),
    ("I/rm-own.txt", "rm", "rm I/rm-own.txt", Expect::Agree, None),
    (
        "I/own.txt",
        "replace",
        "put -f notes.txt I/own.txt",
        Expect::Agree,
        Some("access_denied"),
    ),
];

/// F82: the same on NT ACLs (the DC, Kerberos), the drop box of K7 included.
#[test]
fn access_predicts_nt_acls() {
    let ep = dc();
    let dir = tmp("access_predicts_nt_acls");
    stage6_local(&dir);
    std::fs::create_dir(dir.join("dl")).unwrap();
    let cache = krb_cache(&dir, "k.cc", "-f", "tester", &[ep.spn(true)]);
    let s = Scratch::new(ep);
    nt_fixture(&s);
    let sr = s.remote();
    let c = Client::kerberos(ep, &cache, &[], &[]);
    let mut c = ready(c, &dir, Some(&sr));
    // No stale DOS attribute on the scratch directory (finding 1 of the plan's review).
    let objs = access_objects(&mut c, &sr, &[format!("access {sr}")]);
    let me = &objs[""];
    assert_eq!(
        (me["mask"].as_u64(), me["attributes"].as_u64()),
        (Some(0x001f_01ff), Some(0x10)),
        "the scratch directory on the DC: {me}"
    );
    let mut lines = vec![format!("access -e {sr}")];
    for d in ["D", "E", "F", "H", "I"] {
        lines.push(format!("access -e {sr}/{d}"));
    }
    let objs = access_objects(&mut c, &sr, &lines);
    check_masks("NT ACLs", &objs, NT_MASKS);
    check_rows("NT ACLs", &mut c, &objs, NT_ROWS);
    // K7: the drop box took the temporary file and refused its rename; it is left behind
    let left = temps_on_server(SAMBA_DC, &format!("{}/J", s.server()));
    assert_eq!(left.len(), 1, "the part file of put notes.txt J/: {left:?}");
}

/// F83: the share's `read only`, encryption and the guest logins are in the server's answer.
#[test]
fn access_by_share_and_login() {
    let dir = tmp("access_by_share_and_login");
    stage6_local(&dir);
    let s = Scratch::new(fast());
    posix_fixture(&s);
    let sr = s.remote();
    let by_path = |r: &Reply| -> BTreeMap<String, Value> {
        r.of("access")
            .into_iter()
            .map(|v| (v["path"].as_str().unwrap().to_string(), v.clone()))
            .collect()
    };
    let every = |c: &mut Client, share: &str| {
        c.cmd(&format!("use {share}")).expect_ok(share);
        let r = c.cmd(&format!("access -e {sr}"));
        r.expect_ok(&format!("access -e on {share}"));
        by_path(&r)
    };
    let mut c = Client::to(fast(), &[]);
    let share = every(&mut c, "share");
    // readonly: every change refused, reading as on share
    let mut c = ready(Client::to(fast(), &[]), &dir, None);
    let ro = every(&mut c, "readonly");
    assert_eq!(ro.len(), share.len());
    for (p, v) in &ro {
        let on_share = &share[p];
        let rodos = p.ends_with("/rodos") || p.ends_with("/rodos2");
        if v["is_dir"] == true {
            assert_eq!(v["ls"], on_share["ls"], "{p} ls");
            // a directory with the DOS read-only attribute: put and mkdir are unknown (A7)
            let refused = if rodos {
                Value::Null
            } else {
                Value::Bool(false)
            };
            for k in ["put", "mkdir"] {
                assert_eq!(v[k], refused, "{p} {k} on readonly: {v}");
            }
            assert_eq!(v["rmdir"], false, "{p} rmdir on readonly: {v}");
        } else {
            assert_eq!(v["get"], on_share["get"], "{p} get");
            for k in ["replace", "rm"] {
                assert_eq!(v[k], false, "{p} {k} on readonly: {v}");
            }
        }
    }
    assert_eq!(ro[&format!("{sr}/secret.txt")]["get"], false);
    assert_eq!(ro[&format!("{sr}/mine.txt")]["mask"], 0x0004_0089);
    assert_eq!(ro[&format!("{sr}/rootfile.txt")]["mask"], 0x0000_0089);
    let r = c.cmd(&format!("access {sr}"));
    assert_eq!(r.of("access")[0]["mask"], 0x000c_00a9, "{:?}", r.lines);
    for line in [
        format!("put notes.txt {sr}/"),
        format!("rm {sr}/rm-mine.txt"),
        format!("mkdir {sr}/x"),
    ] {
        let r = c.cmd(&line);
        assert_eq!(
            r.kind().as_deref(),
            Some("access_denied"),
            "{line}: {:?}",
            r.lines
        );
    }
    // encrypted: the same masks as on share
    let mut c = Client::to(fast(), &[]);
    let enc = every(&mut c, "encrypted");
    let masks = |m: &BTreeMap<String, Value>| -> BTreeMap<String, Value> {
        m.iter()
            .map(|(p, v)| (p.clone(), v["mask"].clone()))
            .collect()
    };
    assert_eq!(masks(&enc), masks(&share), "encrypted against share");
    // guest, anonymous and as Guest; and tester on share, in text
    let route = fast().route(None);
    let text = |login: &[&str], share: &str, line: &str| -> Output {
        let mut a: Vec<&str> = route.iter().map(String::as_str).collect();
        a.extend(login);
        let use_share = format!("use {share}");
        a.extend(["-C", &use_share, "-C", line]);
        run(&a, None)
    };
    let root = "d  ls  -   -     -      /\n";
    for login in [&[][..], &["-u", "Guest", "--no-pass"][..]] {
        let o = text(login, "guest", "access /");
        assert_eq!(
            (o.code, o.stdout.as_str()),
            (0, root),
            "{login:?}: {}",
            o.stderr
        );
        let o = text(login, "guest", "access hello.txt");
        assert_eq!(
            (o.code, o.stdout.as_str()),
            (0, "-  get -         -      /hello.txt\n"),
            "{login:?}: {}",
            o.stderr
        );
        let o = run_in(
            &dir,
            &[
                &route.iter().map(String::as_str).collect::<Vec<_>>()[..],
                login,
                &["-C", "use guest", "-C", "put notes.txt /"],
            ]
            .concat(),
            None,
            &[],
        );
        assert_eq!(
            o.code, 1,
            "{login:?}: put into the guest share: {}",
            o.stderr
        );
    }
    let o = text(&LOGIN, "share", "access /");
    assert_eq!((o.code, o.stdout.as_str()), (0, root), "{}", o.stderr);
}
