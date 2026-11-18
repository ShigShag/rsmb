//! smbtap command line: forwards one listening port to an SMB server and counts the traffic.
//! The library (src/lib.rs) does the work; docs/measuring.md describes both.

use std::io::{self, BufRead, Write};
use std::process::exit;
use std::time::Duration;

const USAGE: &str = "usage: smbtap <upstream-host:port> [--listen <addr:port>]

Forwards every connection to <upstream> (default listen address 127.0.0.1:0, the port is chosen by
the system) and counts the SMB traffic. The first line on stdout is {\"listen\":\"<addr:port>\"}.
Commands on stdin, one per line, each answered with one JSON line on stdout:
  stats                          counters and per-command request counts
  log                            the same plus every request and response
  reset                          zero the counters and logs
  drop                           close every live connection (both sides)
  settle <quiet-ms> [<max-ms>]   wait until nothing is outstanding and the client has been quiet
                                 for <quiet-ms> (default max 30000); answers {\"settled\":true|false}
  quit                           print the final stats and exit (end of input does the same)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (upstream, listen) = match args.as_slice() {
        [u] if !u.starts_with('-') => (u.as_str(), "127.0.0.1:0"),
        [u, flag, l] if flag == "--listen" => (u.as_str(), l.as_str()),
        _ => {
            eprintln!("{USAGE}");
            exit(2);
        }
    };
    let tap = smbtap::Tap::start_on(listen, upstream).unwrap_or_else(|e| {
        eprintln!("smbtap: {e}");
        exit(1);
    });
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{{\"listen\":\"{}\"}}", tap.addr());
    let _ = out.flush();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let words: Vec<&str> = line.split_whitespace().collect();
        let ms = |s: &str| s.parse().ok().map(Duration::from_millis);
        let answer = match words.as_slice() {
            [] => continue,
            ["quit"] => break,
            ["stats"] => tap.stats().to_json(false),
            ["log"] => tap.stats().to_json(true),
            ["reset"] => {
                tap.reset();
                "{\"reset\":true}".to_string()
            }
            ["drop"] => format!("{{\"dropped\":{}}}", tap.drop_connections()),
            ["settle", quiet, rest @ ..] if rest.len() <= 1 => {
                match (
                    ms(quiet),
                    rest.first()
                        .map_or(Some(Duration::from_secs(30)), |m| ms(m)),
                ) {
                    (Some(q), Some(max)) => format!("{{\"settled\":{}}}", tap.settle(q, max)),
                    _ => "{\"error\":\"settle takes milliseconds\"}".to_string(),
                }
            }
            _ => "{\"error\":\"unknown command; see smbtap --help\"}".to_string(),
        };
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
    let _ = writeln!(out, "{}", tap.stats().to_json(false));
}
