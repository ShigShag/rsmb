# Stage 4 plan: SOCKS5 proxy, and closing the project

> Written on 2026-11-18 by a planning agent that read the repository and nothing else, before any
> Stage 4 code exists. It follows [`master-plan.md`](master-plan.md) (sections 3–6, 10, 11–14);
> where it departs from the master plan, section 4 says where and why. It is written for one
> implementing agent who was not present: everything needed to build Stage 4 in one pass is here
> or in a document this one names. **Revised on 2026-11-18 after the adversary's review:**
> section 11 lists the eleven findings, the evidence for each and what changed; the text above it
> is the revised plan, and 2.15 holds what was measured for the revision. Section 10 lists what
> could not be verified.
>
> Words are used as in `docs/measuring.md` (request, flight, outstanding) and in the master plan
> (fast, slow, edge, wan-bad). "The crate" is `smb2` 0.43.0. **The proxy** is `socks`
> (microsocks 1.0.5) at 10.213.45.20: port 1080 without a login, port 1081 with
> `tester`/`testpass`. **The private server** is `samba-private`, `fileserver.rsmb.test` =
> 10.213.47.10 on the network `priv`, which the host cannot reach. **Direct** means without
> `--proxy`.

## 1. Scope, what is not in it, exit criteria

**In scope** (brief, Stage 4; master plan section 10):

- `--proxy socks5://[user:pass@]host:port`: all traffic to the server goes through the proxy,
  the server's name included; everything of Stages 1 to 3 works through it. The dialer exists
  since Stage 1 (2.1); this stage proves the claim, repairs what the measurements of section 2
  found (messages, URL edge cases, a password in a usage message), and tests the failures.
- The end of the project: the final measured comparison with smbclient-ng in
  `docs/performance.md` and the README, the complete list of known limitations, every document
  and `help all` complete, and the cleanup of section 7 before the review of the whole codebase.

**Not in Stage 4:** SOCKS4, SOCKS4a, HTTP CONNECT, proxy chains, UDP, GSSAPI login at the proxy,
a proxy taken from an environment variable (P8), a prompt or a file for the proxy's password
(P7), a flag of its own for the dial limit (it follows `--timeout`, P9), a bench mode and a tap
command-line flag for the proxy (P15), a second proxy listener with odd credentials in the test
environment (P6), an IPv6 server in the test environment, latency between the client and the
proxy in the test environment (2.10), repeating `tests/slow.rs` through the proxy (S4-2), a
write that pipelines the SOCKS5 handshake.

**Exit criteria** (master plan section 10): every row of section 6.4 passes in `./test.sh`; the
full run is within 10 minutes of counted time (600 s) and `./test.sh fast` within 2.5 (150 s);
the benchmark runs of 6.6 are done and `docs/performance.md` and the README carry the final
table, the README with the key Stage 0 numbers the brief asks for; the cleanup of section 7 is done; `docs/known-limitations.md` is complete by the
procedure of section 9; every document in the `AGENTS.md` index matches the code; the review's
confirmed findings are fixed and their tests are in the suite.

## 2. What was measured while planning

All on 2026-11-18, Samba 4.23.8, microsocks 1.0.5, the release build of commit `93712f1`, with
throwaway scripts under `spikes/smb2/target/scratch-stage4/` (git-ignored, not committed). Runs
that pause a container, use the blackhole, read the server-side connection count or assert a
time held the exclusive lock; the others the shared one. Every pause and the blackhole were
undone; afterwards all six containers were healthy, `ctl.sh drops` read 0 and `upload/` was
empty on all three servers. Server writes went only below `upload/rsmb-plan4*`.

### 2.1 What exists today

| What | Where | State |
|---|---|---|
| The SOCKS5 client | `tokio-socks` 0.5.3 (a dependency since Stage 1) | `Socks5Stream::connect_with_socket` and `connect_with_password_and_socket` on a `TcpStream` rsmb opened itself, so `TCP_NODELAY` is set on it |
| The dialer | `open_tcp`, `dial`, `login` in `src/session.rs` | direct, or TCP to the proxy and a CONNECT with the target as `(&str, u16)`: a host name goes to the proxy as a name. One 10 s timeout around the whole dial. Used for start-up and for every reconnect |
| Where rsmb opens a socket | `grep TcpStream::connect src/` | two lines, both in `open_tcp`; with a proxy only the one to the proxy runs. Nothing else dials: the crate gets the transport halves and has no reviver |
| The flag | `--proxy <URL>` in `src/main.rs`, `parse_proxy` in `src/session.rs` | parsed by hand before anything touches the network; a bad URL is `usage`, exit 2. Credentials percent-decoded, IPv6 proxy in brackets (Stage 1 review, finding 12) |
| Environment | `grep env::var src/` | rsmb reads `RSMB_HISTORY`, `XDG_STATE_HOME` and `HOME`; no proxy variable. `tokio-socks` reads none |
| Unit tests | `proxy_urls`, `dial_sets_nodelay` | the parser (with and without credentials, encoded credentials, IPv6, bad forms); `TCP_NODELAY` through a local fake proxy of 15 lines that grants every CONNECT |
| Integration tests | `socks5_smoke` (`tests/slow.rs`), one line of `startup_failures` | `use share` and `ls /sizes` through port 1080 and one more proxy log line; `--proxy http://x` exits 2. **Nothing else**: no login at the proxy, no transfer, no walk, no failure, no reconnect, no count |
| `check.sh` | section "SOCKS5 and the private server" | with python: both listeners reach SMB, wrong password and missing credentials refused, the proxy log and the server's connection count rise by one per connection, the name does not resolve on the host, 10.213.47.10 is unreachable |
| The tap | `smbtap` | forwards to an SMB server; a connection that starts with a SOCKS5 handshake would be cut into frames from its first byte (master plan S4-1) |
| The private server | `samba-private` | the same image as `samba`: every fixture, `upload/`, `tiny`, `readonly`, `encrypted`, `guest`; `./test.sh` already removes stale test directories there |
| Documents | `docs/commands.md` (flag row), `docs/architecture.md` (dialer), `docs/known-limitations.md` (one line) | no section on the proxy, no failure messages, nothing in `docs/scripting.md` or `docs/performance.md` |

### 2.2 Every command through both listeners

One non-interactive session per listener against `fileserver.rsmb.test`, 40 lines, `--json`:
`shares`, `use` (`share`, `encrypted`, `readonly`), `pwd`, `ls` (`sizes/`, `cjk/`, `many/`,
`huge/`), `cd`, `lcd`, `lpwd`, `lls`, `get` (1 MiB + 1 byte, 50 MiB), `get -r /tree/d0`, `cat`,
`mkdir -p`, `put` (300 kB, 2 MB), `put -r`, `rm`, `rmdir`, `tree -L 2 /tree`, `find` (`/tree`,
`/fanout`, `/loops`), `refresh`, `help`.

| Listener | Result |
|---|---|
| 1080 (no login) | every line as direct: 6 shares; 11, 1,010, 10,000 and 100,000 entries; `cd nope` `not_found`, `ls /forbidden` `access_denied`, `mkdir` on `readonly` `access_denied`; sha256 of both downloads equal to the server's (`7ce25137…`, `50556313…`); the 3 files of `put -r` equal on the server; `get -r /tree/d0` 85 files; `find /tree -name file.txt` 340 entries and one `denied`; `find /loops` 3 entries and 2 `skipped`; stderr empty; the proxy log gained exactly 1 `connected to fileserver.rsmb.test:445` line |
| 1081 (`tester:testpass@`) | the same, line for line; 1 log line |

**Everything works through the proxy today.** What is missing is the proof in the gate and the
points of 2.5, 2.8 and 2.9.

### 2.3 Requests and flights through the proxy

Where the tap can sit, both tried:

| Place | Tried | Result |
|---|---|---|
| **In front of the proxy**: rsmb → tap → proxy → private server, the tap relaying the SOCKS5 handshake before it starts framing | a copy of `smbtap` with 45 more lines (`socks5_preamble`, 5.7) | works on both listeners; the counts below. The first version relayed the CONNECT request in three writes and microsocks answered `General SOCKS server failure`: it parses one `recv` per message. **Each handshake message must be forwarded in one write** |
| Behind the proxy: rsmb → proxy → the unchanged tap listening on the host's bridge address 10.213.45.1 → `samba` | `smbtap 10.213.45.10:445 --listen 10.213.45.1:0`, `--proxy socks5://10.213.45.20:1080 -H 10.213.45.1 -P <port>` | works here (9 requests for `use share` and `ls /sizes`; the proxy logged `connected to 10.213.45.1:39493`). Not chosen (S4-1): the server is then `samba`, which the host reaches without the proxy, so a session that lost its `--proxy` would still pass; and a container connecting to the host is a new demand on the host's firewall |

Counts per command, one session each: direct through the tap to `samba`; through the tap and
port 1080; through the tap and port 1081. Requests + opaque frames / flights, peak outstanding.

| Command | Direct | Port 1080 | Port 1081 |
|---|---|---|---|
| start-up | 3 / 3, 1 connection | the same | the same |
| `use share` | 1 / 1 | = | = |
| `cd tree/d0`, `cd d1` | 2 / 1 each | = | = |
| `cd /`, `pwd`, `lcd` | 0 | = | = |
| `ls /sizes`, `ls /many`, `ls /huge` | 5 / 1 each | = | = |
| `shares`, again | 7 / 7, 6 / 6 | = | = |
| `get /sizes/1.bin`, `cat` of a small file | 7 / 3 | = | = |
| `get /sizes/1048577.bin` | 9 / 4 | = | = |
| `put` of 4,000 bytes | 7 / 3 | = | = |
| `put` of 1 MiB + 1 byte | 14 / 7 | = | = |
| `put` refused, target exists | 13 / 6 | = | = |
| `mkdir`; `rmdir`, `rm` | 2 / 1; 3 / 1 | = | = |
| `get -r /smallfiles/d01` | 304, peak 48 (14 flights) | 304, peak 48 (15) | 304, peak 48 (14) |
| `put -r d01` | 702, peak 48 (207) | 702, peak 48 (169) | 702, peak 48 (180) |
| `find /tree`; `tree /tree` | 1,368, peak 64 (316; 302) | 1,368, peak 64 (317; 269) | 1,368, peak 64 (307; 305) |
| `find /chain` | 244 / 61, peak 4 | = | = |
| `tree /loops` | 4 / 1 | = | = |
| `find /fanout` | 8,004, peak 64 (1,982) | 8,004, peak 64 (1,966) | 8,004, peak 64 (1,972) |
| `find /many -name 'f0999?'` | 9 / 5 | = | = |
| `use encrypted`; `ls /sizes` there | 1 / 1; 0 + 1 opaque / 1 | = | = |

**The request count is the same on all 33 rows, and so is the flight count wherever requests do
not overlap.** Where 16 are in flight the flight count varies from run to run, as it does direct
(Stage 2 I5). The numbers are the ones `docs/testing.md` lists for the direct tests.

An idle session through port 1081 sent 0 requests in 12 s and its next `ls` used the same
connection.

### 2.4 Cut, reconnect, interrupt, the large file

Through the prototype tap in front of port 1080, one session, the server-side count reset first
(`ctl.sh conns-reset samba-private`, exclusive lock):

| Step | Measured |
|---|---|
| start-up, `use share`, `cd /tree/d0`, `ls /sizes` | 11 requests, 1 connection in the tap; proxy log +1; `ctl.sh conns samba-private` 1 |
| the tap drops the connection while idle; `pwd` | 0 requests |
| `ls` after the cut | succeeds: 9 requests (3 login, TREE_CONNECT, 5), 1 new connection; log +2, server 2 |
| the tap drops at the first QUERY_DIRECTORY of `ls /huge` | the command is run again and succeeds with 100,000 entries: 14 requests, 1 new connection; log +3, server 3 |
| the tap drops at the 100th READ of `get /large/big.bin` | `connection_lost`, "was not run again and may or may not have taken effect"; no local file, no temporary file |
| `get /large/big.bin` again | 1.35 s with its reconnect, sha256 `ad2f1026…` (the fixture's); 2,263 requests, at most 8 outstanding; log +4, server 4 |
| `put big.bin` into a scratch directory | 1.23 s, sha256 on the server equal; 2,266 requests, at most 8 outstanding |
| the tap drops at the 100th WRITE of `put -f big.bin` | `connection_lost`; the cleanup ran on 1 new connection; the directory holds only the earlier `big.bin`, unchanged, and no temporary file; log +5, server 5 |
| SIGINT at the 100th READ of a third `get /large/big.bin` | `interrupted`, exit 130, 0.02 s after the signal; no local file |

**Every connection rsmb opened is one line in the proxy log and one connection on the private
server** (5 and 5). A reconnect goes through the proxy with the name, like the first dial.

### 2.5 Proxy failures: time, kind, exit code, message

`rsmb … --json -N -C 'use share' -C 'ls /sizes'`, one run each. All are start-up failures: one
`error` object with `"command":null`, no `done`.

| Case | Exit | Time | Kind | Message today |
|---|---|---|---|---|
| port 1081, wrong password; wrong user | 3 | 2 ms | `connect` | `proxy 10.213.45.20:1081: Password auth failure, code: 2` |
| port 1081, no credentials | 3 | 2 ms | `connect` | `proxy 10.213.45.20:1081: No acceptable auth methods` |
| port 1080, credentials given | 0 | 7 ms | | works: rsmb offers both methods, microsocks takes "no login" |
| proxy port closed (1082) | 3 | 2 ms | `connect` | `proxy 10.213.45.20:1082: Connection refused (os error 111)` |
| proxy address absent (10.213.45.99) | 3 | 3.11 s | `connect` | `proxy 10.213.45.99:1080: No route to host (os error 113)` |
| proxy name unknown on the host | 3 | 49 ms | `connect` | `proxy noproxy.rsmb.test:1080: failed to lookup address information: …` |
| the "proxy" is an SMB server (10.213.45.10:445) | 3 | 10.00 s | `connect` | `proxy 10.213.45.10:445: no answer within 10 s` |
| target address absent (10.213.47.99) | 3 | 3.06 s | `connect` | `proxy 10.213.45.20:1080: Host unreachable` |
| target port closed (446) | 3 | 2 ms | `connect` | `proxy 10.213.45.20:1080: Connection refused` |
| target name unknown to the proxy | 3 | 92 ms | `connect` | `proxy 10.213.45.20:1080: General SOCKS server failure` |
| a wrong SMB password through port 1080 | 3 | 5 ms | `auth` | `login as 'TESTDOM\tester': login refused (… STATUS_LOGON_FAILURE …)`: the proxy passed it on, the server refused |
| direct to 10.213.47.10 (no `--proxy`) | 3 | 10.00 s | `connect` | `connect to 10.213.47.10:445: no answer within 10 s` |
| direct to `fileserver.rsmb.test` | 3 | 5 ms | `connect` | `connect to fileserver.rsmb.test:445: failed to lookup address information: Name or service not known` |
| a malformed URL (2.8) | 2 | 2 ms | `usage` | `--proxy '<the whole URL>': expected socks5://…` |

Found: the kinds, exit codes and bounds are right and every message names the proxy. **Three
messages mislead**: `Connection refused` for a closed *target* port reads as if the proxy had
refused; `General SOCKS server failure` is all microsocks says about an unknown name;
`Password auth failure, code: 2` and `No acceptable auth methods` are the library's words, not
a hint at what to do. None names the target.

### 2.6 Silent proxy, silent server, blackholed target

Exclusive lock; `ctl.sh pause socks`, `pause samba-private`, `blackhole on`, each undone.

| Case | Measured |
|---|---|
| proxy paused at start-up | exit 3 after 10.00 s, `connect`, `proxy 10.213.45.20:1080: no answer within 10 s` |
| private server paused at start-up | the CONNECT succeeds (the kernel accepts), NEGOTIATE is not answered: exit 3 after 30.03 s with the default `--timeout`, 5.01 s with `--timeout 5`, kind `connection_lost` |
| target SYN dropped behind the proxy (`-H 10.213.45.30 -P 4450`, blackhole on) | exit 3 after 10.00 s, `connect`, `proxy 10.213.45.20:1080: no answer within 10 s`. **Misleading**: the proxy answered, the target did not |
| mid-session, proxy paused, `--timeout 5`: `ls` | fails after 15.02 s, `connection_lost` ("no answer from the server for 15 s") |
| then `pwd` | at once |
| then `ls` again, proxy still paused | fails after 10.0 s, `connect`, `reconnect failed: proxy 10.213.45.20:1080: no answer within 10 s` (one dial, not two) |
| proxy unpaused, `ls` | works |
| mid-session, private server paused: `ls`; `ls` again | 15.02 s `connection_lost`; then 5.01 s, `reconnect failed: connection lost: no complete reply within the response deadline…` (the proxy connected, the server did not answer); after the unpause `ls` works |
| mid-session, blackhole between proxy and server (`-H 10.213.45.30 -P 4450`): `ls`; again; blackhole off | 15.02 s `connection_lost`; 10.0 s `connect`; then 1.21 s (reconnect and `ls`) |

So a dead link behind or at the proxy behaves as a dead direct link does (Stage 1 rows 1.10).
One thing to know for the tests: a connect the proxy began while the blackhole was on completed
seconds after it was lifted (a retransmitted SYN) and added a proxy log line then. A test counts
log lines for its own target only, and not across a blackhole.

### 2.7 Who resolves the name

| Probe | Result |
|---|---|
| `getent hosts fileserver.rsmb.test` on the host | nothing, exit 2 |
| the proxy's log | `connected to fileserver.rsmb.test:445`: the name as rsmb sent it. With `-H FileServer.RSMB.Test` the line reads `connected to FileServer.RSMB.Test:445` and the session works (Docker's DNS ignores case); with `-H samba-private` likewise; with `-H 10.213.47.10` it reads `connected to 10.213.47.10:445` |
| what rsmb puts into the CONNECT request (a fake proxy in python that prints what it reads) | a name: address type 3 and the name's bytes; an IPv4 literal: type 1; an IPv6 literal (`-H ::1`): type 4 |
| **the loader's own log**: `LD_DEBUG=files rsmb …`, which prints every library the process loads; a name lookup on this host loads `libnss_mymachines`, `libnss_resolve` and `libnss_myhostname` (`hosts:` line of `nsswitch.conf`) | proxied with a name: **no `libnss_` module loaded**, exit 0. Proxied with an address: none. Direct to an address: none. Controls: direct to a name: the three modules; `--proxy socks5://localhost:9`: the three modules (the proxy's own name is resolved on the host, as it must be) |
| **a capture on every host interface** (`dumpcap -i any`, ports 53, 5353, 5355, 853) during a run | proxied with the name: 12 packets, **none naming `rsmb.test`**. Control, direct to the name: 8 packets, all 8 naming it (A and AAAA queries and their answers) |

rsmb does not resolve the server's name: no call into the resolver, no packet. Both probes can
see a lookup (the controls). The loader probe does not depend on a resolver cache; it depends on
the host loading a name-service module at run time (10, assumption 8).

### 2.8 Targets and URLs

Against the proxy, and against the fake proxy on `[::1]` that demands the user `us@er` and the
password `p:ss/%wörd`:

| Input | Today |
|---|---|
| `--proxy 'socks5://us%40er:p%3Ass%2F%25w%C3%B6rd@[::1]:<port>'` | **works end to end**: the proxy read `us@er` and `p:ss/%wörd` (UTF-8), methods offered `[0, 2]`, 3 client messages before the SMB stream (2 without credentials) |
| the same with a bare `%` in the password | `usage`, exit 2 |
| `--proxy socks5://localhost:<port>` | works (the proxy's name is resolved on the host) |
| `-H 10.213.47.10`, `-H 10.213.45.10` through port 1080 | work |
| `-H ::1` through a proxy | sent as an IPv6 address (type 4); `-H '[::1]'` is sent as the *name* `[::1]` and fails. Direct, `-H ::1` is dialled (`connect to ::1:445: Connection refused`). No IPv6 server exists here |
| a target name of 256 bytes | `connect`, exit 3, `Target address is invalid: overlong domain` |
| `socks5h://…`, `socks5://host:1080/`, `SOCKS5://…`, `socks4://…`, `http://…`, no scheme, no port, port 99999, the empty string | `usage`, exit 2 |
| `socks5://:pw@…`, `socks5://user:@…`, a user of 256 bytes | **exit 3**, kind `connect`, `Invalid auth values: username length should between 1 to 255`: a usage mistake reported as a failed connection |
| `socks5://user@…` (no password) | `usage`, exit 2 |

### 2.9 Where the password shows

| Place | Today |
|---|---|
| the process list | `--proxy socks5://user:pass@…` is an argument, readable by every local user, as `-p` is |
| **a usage error** | `rsmb: --proxy 'socks5://user:secretpw@10.213.45.20': expected …`: **the whole URL with the password**, on stderr and in the `message` of the `--json` error; the same for a URL without `@` (`socks5://alice:hunter2`, 2.15) |
| dial and login failures, the banner, `pwd`, any other `--json` object | the proxy's host and port at most; no user, no password |

### 2.10 What the proxy costs in time

Direct beside proxied, one session each, medians of 20 (`cd`, small `ls`) or 5 (`ls /many`);
"slow via the proxy" is `--proxy socks5://10.213.45.20:1080 -H 10.213.45.30`: the proxy sits on
`pub` and reaches `wan`, so the 100 ms each way lie between the proxy and the server.

| Route | Process with `pwd` | `use` | `cd` | `ls` small | `ls /many` |
|---|---|---|---|---|---|
| fast, direct | 4.0 ms | 0.5 | 0.21 | 0.35 | 114 |
| fast, port 1080, private server | 4.6 | 0.5 | 0.23 | 0.33 | 103 |
| fast, port 1081, private server | 4.3 | 0.5 | 0.21 | 0.34 | 101 |
| slow, direct | 806.2, 806.1 | 200.9 | 200.67, 200.63 | 200.99, 200.88 | 313.8 |
| slow, port 1080 | 806.2, 806.3 | 201.2 | 200.75, 200.80 | 200.80, 200.93 | 315.7 |
| slow, port 1081 | 806.4, 806.5 | 200.9 | 200.89, 200.93 | 200.88, 200.95 | 315.7 |

**Start-up, `cd` and `ls` cost the same through the proxy** when the proxy is near the client.
The handshake is 2 round trips to the proxy after the TCP connect, 3 with a login (2.8); here
they are local. With the latency between client and proxy instead, a connection would cost
1 + 2 (or 3) round trips more than the direct 4; this environment cannot produce that (`wan`
forwards only to `samba`).

What overlaps is slower through this proxy. At the slow endpoint, direct and through port 1080,
wall time of the command:

| Command | Direct | Through the proxy | Ratio |
|---|---|---|---|
| `find /tree/d0` (85 listings) | 1.609 s | 2.006 s | 1.25 |
| `find /tree` (342) | 4.619 | 5.615 | 1.22 |
| `find /fanout` (2,001) | 25.308 | 28.301 | 1.12 |
| `get -r /smallfiles/d01` (100 files) | 1.699 | 2.028 | 1.19 |
| `put -r d01` | 4.513 | 5.814 | 1.29 |
| `get /large/medium.bin` (50 MiB) | 4.250 | 6.650 | 1.56 |
| `put medium.bin` | 5.053 | 6.055 | 1.20 |
| `cd` | 0.201 | 0.201 | 1.00 |

The cause is the proxy, seen in a capture of the proxy's side towards `wan` during
`find /tree/d0`: a burst of requests leaves as full segments at once and its last, partial
segment one round trip later (1,332 and 1,448 bytes at 1.207 s, the remaining 360 at 1.407 s;
four full segments at 1.409 s, the remaining 988 at 1.608 s). That is Nagle's algorithm on the
proxy's socket to the server: microsocks does not set `TCP_NODELAY`. rsmb sets it on its own
socket; nothing a client does changes the proxy's.

At the fast endpoint the same shows as the 40 ms of a delayed ACK:

| Command, fast | Direct | Port 1080 |
|---|---|---|
| `find /tree`; `find /fanout`; `find /chain` | 86; 472; 14 ms | 117; 476; 16 ms |
| `ls /huge`; `tree /huge` | 998; 1,100 | 999; 1,386 |
| `get -r /smallfiles/d01`; `put -r d01` | 94; 99 | 86; 396 |
| `get -r /smallfiles` (2,000 files); `put -r smallfiles` | 1,507; 1,769 | 1,486; 5,284 |
| `get /large/medium.bin`; `put medium.bin` | 40; 41 | 106; 130 |
| `get -r /tree`; `get -r /cjk` | 352; 830 | 461; 940 |
| `big.bin` (1.1 GiB) down; up (2.4) | 1.8 s; 2.1 s in the gate | 1.35 s; 1.23 s |

### 2.11 Many sessions at once

64 rsmb processes side by side, 32 per listener, each `use share`, `ls /many`, `find /tree`:
all 64 printed their 10,682 entries; 864 ms for the lot. The proxy does not limit what a
parallel test run needs.

### 2.12 `--json` on a terminal

`rsmb … --json -C 'use share'` on a pseudo-terminal (`testenv/ptyrun.py`), then `pwd` and
`exit` typed: after the `done` of `use share` the line editor starts, and stdout carries
`ESC[?2004h…//10.213.45.10/share/> ` and the echo of each typed line between the JSON lines; the
banner `logged in as …` is there too. The Stage 1 review recorded this as documented, not fixed
(finding 6).

### 2.13 smbclient-ng

- `smbclientng --help` has no proxy option: there is no proxied column for it.
- Transfers were never measured for it. One run each at the slow endpoint, time with the command
  minus time without (4,994 ms): **download of `large/medium.bin` (50 MiB) 18.3 s** (its own
  display: 4.9 MB/s; sha256 equal to the fixture's; the file lands in `./large/`), **upload of
  50 MiB 68.5 s** (778.9 kB/s; sha256 equal on the server). rsmb at the same endpoint: 4.25 s
  and 5.05 s (2.10). At the fast endpoint its download took 375 ms above the empty run.
- The navigation baseline (`testenv/baseline-smbclientng.txt`) is from 2026-11-17, before the
  image was rebuilt for `links/`; it was not re-run for Stages 1 to 3 (`docs/performance.md`,
  "Not done").

### 2.14 Stale statements and leftovers found by reading

| Where | What |
|---|---|
| `README.md` | "Status: Stage 3 … The full SOCKS5 test matrix (Stage 4) follows"; the table "Key numbers of the Stage 0 spike" stands where the final numbers belong (the brief wants the Stage 0 measurements in the README: it stays, shorter, below the final table); `./test.sh` "about 6.5 min", `fast` "about 45 s" |
| `docs/architecture.md` | "as of Stage 3"; the section "What Stage 4 adds where" |
| `docs/known-limitations.md` | "Known as of Stage 3 … Later stages add to it"; organised by stage; "showing one file is not in Stage 1"; the `--proxy` line of 2.8 (now measured end to end) |
| `docs/testing.md` | two paragraphs that both begin "The longest tests", the second ("None takes more than 30 s") true for Stage 1 only; three sets of step times; the budget sentence |
| `docs/performance.md` | "Not done": the baseline "was not re-run", "not in Stage 2", "Not built in Stage 3"; no row through a proxy; smbclient-ng's transfers "not benchmarked" |
| `docs/scripting.md`, the JSON part of `help all` | "in a terminal session the line editor draws its prompt and echo on stdout" |
| `docs/development.md`, `docs/test-environment.md`, `AGENTS.md` | budgets 120 s and 540 s, "about 45 s", "about 6.5 min", "about 5 min" |
| `docs/plans/master-plan.md` 4.2 | lists `indicatif`; it was never added (`docs/development.md` has the real list). A plan is a record and is not edited; `docs/development.md` is checked against `Cargo.toml` |
| `tests/slow.rs` `socks5_smoke` | counts "one more proxy log line" beside other tests; right only while it is the one test that uses the proxy |
| `src/` | no dead code the compiler can see (`-D warnings` on a binary crate); the one `#[allow(dead_code)]` (`Alive`) holds a channel open on purpose. Dependencies: every one in `Cargo.toml` is used |

### 2.15 Measured after the review

| What | Measured |
|---|---|
| one direct run of `tests/fast.rs` under the exclusive lock, `ctl.sh conns-reset samba` before it | 66 passed in 12.66 s; **149 TCP connections on `samba`**; the proxy's log gained 0 lines for the private server. A proxied run makes the same connections, each one line in the proxy's log (2.4: 5 and 5) |
| a session through the prototype tap and port 1080 **to `samba`** (`-H 10.213.45.10`, which the host also reaches without the proxy), then the tap closed so that the proxy port rsmb was given is gone | before: `use share` and `ls /sizes` work, 9 requests, 1 connection. After: `ls /sizes` fails at once, kind `connect`, `reconnect failed: proxy 127.0.0.1:<port>: Connection refused (os error 111)`; a second `ls` the same; `pwd` works. **rsmb does not fall back to a direct connection today**; no test would notice if it did (finding 1) |
| `--proxy socks5://10.213.45.20:1082 -H 10.213.45.10` (closed proxy port, reachable server) | exit 3 after 2 ms, kind `connect`, `proxy 10.213.45.20:1082: Connection refused (os error 111)` |
| `--proxy socks5://alice:hunter2` | exit 2; the message prints `'socks5://alice:hunter2'`: a URL without `@` would pass the first version's redaction whole (finding 5) |
| a second `--proxy` on the command line (the orchestrator's run) | clap: `the argument '--proxy <URL>' cannot be used multiple times`, exit 2: the exit code of a bad URL, for another reason (finding 4) |
| `-H` of 256 bytes through port 1080 | exit 3, kind `connect`, `Target address is invalid: overlong domain` (2.8) |
| which existing tests wait for a dial that gets no answer | `silent_link_blackhole`, `silent_link_mid_put`, `leftover_temp_is_reported_once`: all three pass `--timeout 5` (read in `tests/serial.rs`); no test and no bench row does so with the default `--timeout` |

## 3. Decisions

On the open questions of master plan section 14:

| # | Question | Decision | Reason |
|---|---|---|---|
| S4-1 | Request counts through the proxy | **Asserted, the same numbers as direct.** `smbtap` learns to relay a SOCKS5 handshake (5.7) and sits between rsmb and the proxy; the server is the private one | Measured (2.3): 33 of 33 rows equal. The alternative the master plan names, at 45 lines in the tap. It makes row 4.1 literal (the same tests with the same assertions), puts the tap where row 4.7 wants to cut, and keeps the proof by construction: the private server cannot be reached without the proxy. The tap behind the proxy also works but loses that proof |
| S4-2 | How much of `tests/slow.rs` is repeated through the proxy | **None** (the default). The proxied slow path exists (the proxy reaches `wan`, 2.10); its measurement from planning goes into `docs/performance.md` with date and method, and is not asserted | Measured (2.10): start-up, `cd` and `ls` are equal to the millisecond; what differs comes from the proxy's socket options, which rsmb cannot change and a test of rsmb should not assert |

On points the master plan left to this plan:

| # | Point | Decision |
|---|---|---|
| P1 | How "everything from Stages 1 to 3 works through the proxy" is proven | **`tests/fast.rs` runs a second time, whole, through port 1080 against the private server**, chosen by one environment variable (6.1), in both modes of `./test.sh`. Every assertion stays: outputs, exit codes, checksums, request and flight counts, the pty tests. `./test.sh` itself checks that the step went through the proxy (6.2, finding 2). On top: every command in one session through port 1081 (S61), `big.bin` both ways (S62), cuts and signals (S65, S66, X63), one connection on the server and one log line (X61), and two cases in which a direct connection would work and must not be used (S63, S65, finding 1) |
| P2 | Kind and exit code of a proxy failure | Kind `connect` for every failure of the dial, the proxy's refusal of a login included; exit 3 at start-up, 1 when a reconnect fails. No new kind: 15 as before. A script tells the cases apart by the message, which starts `proxy <host>:<port>: ` (5.3) |
| P3 | The messages | rsmb's own words for each `tokio-socks` error, naming the proxy and, when the proxy answered for the target, the target (5.3). A timeout says whether the TCP connection to the proxy stood, and no more: once it stands, rsmb cannot tell a silent proxy from one that is not a SOCKS5 proxy or one that cannot reach the target, and the message names all three (finding 3) |
| P4 | `socks5h://` | **Accepted, the same as `socks5://`.** rsmb always lets the proxy resolve the name, which is what `socks5h` means elsewhere; someone who writes it asks for exactly that and should not get a usage error. One trailing `/` is accepted too (a URL copied from elsewhere often ends in one) |
| P5 | Other schemes | `socks4://`, `socks4a://`, `http://`, `https://`, `socks://`, an upper-case scheme, no scheme: `usage`, exit 2, and the message says SOCKS5 only |
| P6 | Credentials | Percent-decoded as now; after decoding each of user and password must be 1 to 255 bytes (RFC 1929), else `usage` (2.8: today exit 3). Proven end to end once by hand (2.8) and in the gate by a unit test that reads the exact bytes at a fake proxy (`socks5_handshake_bytes`). No third proxy listener with odd credentials: it would prove what the bytes already prove |
| P7 | The password in the process list, in messages, in `--json` | **Process list: documented, not changed** (as `-p`; the brief puts the credentials into the URL). **Messages and `--json`: never.** The usage message for a bad URL does not repeat the URL at all (5.2): any rule for cutting the credentials out fails on some malformed URL, and `socks5://alice:hunter2` has no `@` to cut at (finding 5). S63 and F61 assert that no output holds the password |
| P8 | Environment variables | **None.** `ALL_PROXY`, `all_proxy`, `SOCKS_PROXY`, `RSMB_PROXY` are not read (master plan: not in Stage 4). A route taken from the environment is a route the caller did not ask for on the command line, and the third use wants runs that depend on their arguments only. F62 fixes it |
| P9 | The dial limit | **max(10 s, `--timeout`)** for the whole dial, direct and through a proxy; no flag of its own (finding 8). Someone who raises `--timeout` for a slow link gets a longer dial with it; a smaller `--timeout` leaves the 10 s. **At the default `--timeout 30` a dial that gets no answer now fails after 30 s, not 10** (the rows of 2.5 and 2.6 that read 10.00 s were taken with today's fixed limit). Measured (2.10): the handshake is 2 or 3 round trips |
| P10 | An IPv6 proxy, an IPv6 server, a long name | The proxy in brackets as now (`socks5://[::1]:1080`), tested with the fake proxy. A server address is given without brackets (`-H ::1`) and goes to the proxy as an address; brackets around `-H` are not stripped. Untested beyond the dial: a known limitation. With `--proxy`, a `-H` longer than 255 bytes is `usage`, exit 2: a SOCKS5 request cannot carry it (2.8: today exit 3) |
| P11 | The banner of the interactive shell | Names the proxy's host and port, never its user: `logged in as … on fileserver.rsmb.test:445 through proxy 10.213.45.20:1080 (SMB 3.1.1)` |
| P12 | `--json` on a terminal | **Fixed: `--json` never starts the line editor** (5.5). One condition in `main.rs`. The third use must be able to rely on "every stdout line is one JSON object" whatever stdin is. Lines typed at the terminal then follow the rules of a non-interactive run, which the manual and `docs/scripting.md` say (finding 11) |
| P13 | The proof that the name is not resolved on the host | In the gate: the precondition (the name does not resolve on the host) and a session that works (row 4.4); the bytes of the CONNECT request (unit); the name in the proxy's log in the spelling rsmb was given (X61); and the loader probe of 2.7 with its control (S64), which is skipped with a printed line on a host where the control sees nothing. The packet capture of 2.7 is repeated once at the end of the stage and recorded, not in the gate (a resolver cache or encrypted DNS would blind it) |
| P14 | Where the exact proxy-log and server-side counts are asserted | In `tests/serial.rs` (X61). Log lines and `ctl.sh conns` are shared by everything running; parallel tests count connections in their own tap |
| P15 | The final comparison | One table from one final run of every bench mode on the final commit, with a fresh smbclient-ng baseline and one row that did not exist: a 50 MiB transfer each way with smbclient-ng (6.6). The proxied figures are the planning measurements of 2.3 and 2.10, written into `docs/performance.md` with their date and method: **no `proxy` mode in `bench-rsmb.sh` and no `--socks5` flag in the `smbtap` command line** (findings 7 and 10): smbclient-ng has no proxied column to stand beside, and what the rows would show is microsocks |
| P16 | Where the code goes | `src/session.rs` (URL, errors, the dial limit, banner), `src/main.rs` (one condition, one usage check, the flag's help), `src/commands.rs` (the manual), `smbtap/src/lib.rs` (the relay). No new source file |

## 4. Departures from the master plan

| # | Master plan | Here | Why |
|---|---|---|---|
| D1 | S4-1 default: proxied tests "do not repeat request and flight counts" | they do, through a tap that relays the handshake | S4-1 above; measured equal on 33 rows |
| D2 | Row 4.1: "the tests of `tests/fast.rs` pass again" (full run) | the whole binary a second time, also in `./test.sh fast` | about 19 s (6.2); the proxy path is then checked after every feature, not only before a commit |
| D3 | Rows 4.3 and 4.7: one proxy log line per session, asserted "both ports"; the cut test counts log lines | the exact log-line and server-side counts in one serial test (X61) for both ports; the parallel cut test (S65) counts connections in its tap; `./test.sh` asserts a floor of 100 log lines for the proxied step | P14; 2.6 showed a log line arriving late; finding 2 |
| D4 | Row 4.6: wrong password, no credentials, port closed, malformed URL | those, and: target unreachable, refused, unknown to the proxy; a proxy that is not one; a silent proxy; empty or over-long credentials; a proxy that is gone while the server is reachable directly | 2.5, 2.6, 2.8 measured them; three of the messages misled; finding 1 |
| D5 | Row 1.14 `socks5_smoke` in `tests/slow.rs` | removed; X61 and S61 assert all of it and more | its "one more log line" is wrong beside other proxied tests |
| D6 | Section 6: `fast` means "fast endpoint only" | `fast` = everything without injected latency: `tests/fast` direct and through the proxy | D2 |
| D7 | Stage 1 S1-7: "the shell starts only if stdin is a terminal and `-N` is absent" | and `--json` is absent | P12 |
| D8 | Section 10 names the scheme `socks5://` only | `socks5h://` and one trailing `/` are accepted | P4 |
| D9 | Section 5: the comparison is `cd` and `ls`, with walks added in Stage 3 | plus a 50 MiB transfer each way for both clients, and rsmb's proxied figures from the planning measurement | P15; 2.13 |
| D10 | Section 4.4: the dialer was to need "tests and documentation, not design" | small changes in `session.rs` (5.2 to 5.4) and one check in `main.rs` | found by measuring (2.5, 2.8, 2.9, 2.15); "whatever the tests show is missing" in section 10 covers them |
| D11 | Section 4.4: "Connect timeout 10 s" | max(10 s, `--timeout`) | P9, finding 8 |

Nothing else departs: one connection, no new dependency, no new error kind, the exit codes, all
use of `smb2` in `src/session.rs`, no second timer on requests, the budget of section 6.

## 5. Design

### 5.1 Files, dependencies

No new dependency: `tokio-socks` 0.5.3 does the SOCKS5 handshake; `Cargo.toml` is unchanged.

| File | Stage 4 changes |
|---|---|
| `src/session.rs` | `parse_proxy` (5.2), `hostport`, `proxy_error`, `dial_limit`, `open_tcp` with the limit and the stage (5.3), `Session::describe` (5.4); unit tests |
| `src/main.rs` | `interactive` (5.5); the length check of `-H` with a proxy (5.2); the help text of `--proxy` and of `--timeout` |
| `src/commands.rs` | the topic "proxy", one line in the examples topic, the JSON paragraph, the timeouts topic (the dial limit), the exit-code text (5.6) |
| `smbtap/src/lib.rs` | `Tap::start_socks5`, `socks5_preamble` (5.7). `smbtap/src/main.rs` is unchanged |
| `tests/common/mod.rs`, `tests/fast.rs`, `tests/slow.rs`, `tests/serial.rs`, `test.sh` | section 6 |
| `testenv/bench-rsmb.sh` | the smbclient-ng transfer rows in its `transfers` mode (6.6) |

### 5.2 The URL

```rust
/// `socks5://[user:pass@]host:port`; `socks5h://` means the same; one trailing `/` is dropped.
pub fn parse_proxy(url: &str) -> Result<Proxy>
```

| Part | Rule |
|---|---|
| scheme | `socks5://` or `socks5h://`, lower case; anything else is `usage` |
| end | one `/` after the port is dropped; anything else after the port is `usage` |
| credentials | the part before the **last** `@`, split at its **first** `:`; both halves percent-decoded (a bad escape or bytes that are not UTF-8: `usage`); each 1 to 255 bytes after decoding, else `usage`; a user without `:password` is `usage` |
| host | a name or an IPv4 address as written; an IPv6 address in brackets; empty is `usage` |
| port | 1 to 65535 |
| the message | always the same, and **without the URL**: `--proxy: expected socks5://[user:pass@]host:port (SOCKS5 only; percent-encode special characters in user and password, put an IPv6 host in brackets)` |

The message does not repeat what was given (finding 5): the caller has the URL, and no rule for
blanking the credentials holds for every malformed one (`socks5://alice:hunter2` has no `@`).
The first version's `redact` is gone. `Proxy` keeps its three fields.

One check in `main.rs`, with the other usage checks and before anything touches the network:
with `--proxy`, a `-H` longer than 255 bytes is `usage`, exit 2: `-H: with --proxy the server
name can be at most 255 bytes (a SOCKS5 request carries no longer one)`. Without a proxy
nothing changes.

### 5.3 The dial: how far it got, and what the proxy said

```rust
/// `host:port`, an IPv6 address in brackets.
fn hostport(host: &str, port: u16) -> String
/// max(10 s, `--timeout`): how long a dial may take, direct or through the proxy.
fn dial_limit(timeout: Duration) -> Duration
/// The TCP stream to the server, direct or through the proxy, within `limit`. A timeout says
/// whether the TCP connection to the proxy stood.
async fn open_tcp(t: &Target, limit: Duration) -> Result<TcpStream>
/// A failed SOCKS5 step in our words; always `Kind::Connect`, never a credential.
fn proxy_error(p: &Proxy, target: &str, e: &tokio_socks::Error) -> Error
```

`dial` calls `open_tcp(t, dial_limit(t.timeout))` and no longer has a timeout of its own (the
rest of `dial` does not wait). `DIAL_TIMEOUT` stays 10 s and is the floor: `--timeout 5` gives
10 s, the default `--timeout 30` gives 30 s, `--timeout 120` gives 120 s (P9, finding 8). The
limit covers the whole dial: the TCP connect and, with a proxy, the handshake and the proxy's
own connect to the server. `open_tcp` keeps an `AtomicBool` that the proxy branch sets once the
TCP connection to the proxy stands, runs its work under `tokio::time::timeout(limit, …)`, and on
a timeout says (N = `limit.as_secs()`):

| State | Message (`Kind::Connect`) |
|---|---|
| direct | `connect to <target>: no answer within N s` (as now, with the limit) |
| no TCP connection to the proxy | `proxy <proxy>: no answer within N s` (as now, with the limit) |
| the TCP connection to the proxy stood | `proxy <proxy>: connected to the proxy, but the SOCKS5 handshake did not finish within N s (the proxy is silent, is not a SOCKS5 proxy, or cannot reach <target>)` |

`<proxy>` and `<target>` are `hostport(…)`. The third line does not say which of its three
causes it was, because rsmb cannot know (finding 3): a paused proxy accepts the TCP connection
in its kernel and never answers the greeting, an SMB server taken for a proxy does the same,
and a proxy whose own connect to the server hangs has answered the greeting but not the request
(2.5, 2.6). A flag for "the greeting was answered" would need the handshake split in two calls
that `tokio-socks` does not offer; the wording is the smaller change.

`proxy_error`: the message is `proxy <proxy>: ` followed by

| `tokio_socks::Error` | Text |
|---|---|
| `Io(e)`, end of file | `closed the connection during the SOCKS5 handshake (is it a SOCKS5 proxy?)` |
| `Io(e)`, other | the error's text, as now (`Connection refused (os error 111)`) |
| `NoAcceptableAuthMethods`, no credentials in the URL | `wants a user name and password: --proxy socks5://user:pass@<proxy>` |
| `NoAcceptableAuthMethods`, credentials given | `accepts neither a session without login nor a user name and password` |
| `PasswordAuthFailure(_)` | `user name or password refused` |
| `InvalidResponseVersion`, `UnknownAuthMethod`, `InvalidReservedByte`, `UnknownAddressType` | `not a SOCKS5 answer (is it a SOCKS5 proxy?)` |
| `HostUnreachable` / `NetworkUnreachable` / `ConnectionRefused` | `cannot reach <target>: host unreachable` / `network unreachable` / `connection refused` |
| `GeneralSocksServerFailure` | `cannot reach <target>: the proxy reports a general failure (some proxies answer an unknown host name this way)` |
| `TtlExpired`, `ConnectionNotAllowedByRuleset`, `CommandNotSupported`, `AddressTypeNotSupported` | `cannot reach <target>: ` + `timed out on the way`, `not allowed by the proxy's rules`, `the proxy does not support CONNECT`, `the proxy does not support this kind of address` |
| anything else | the library's text |

The TCP connect to the proxy keeps its message (`proxy <proxy>: <io error>`). A reconnect that
fails keeps the prefix `reconnect failed: ` (Stage 1). **There is no fallback**: when a proxy was
given, `open_tcp` never dials the server itself, at start-up or on a reconnect; a proxy that
cannot be reached is the command's error although the server could be (2.15; S63, S65).

### 5.4 The banner

`Session::describe`: with a proxy, ` through proxy <hostport>` after the server, before the
dialect (P11). Printed by the interactive shell only, on stderr, as now.

### 5.5 `--json` never starts the line editor

`main.rs`: `let interactive = stdin_tty && !args.not_interactive && !args.json;`. Nothing else
changes; what follows from it:

| stdin | Flags | Before | After |
|---|---|---|---|
| a terminal | `--json`, no `-C`, no `-S` | the line editor; prompt and echo on stdout | lines are read from the terminal as from a pipe: no prompt, no history, no completion; one JSON object per stdout line; Ctrl-D ends, exit 0. **The rules of a non-interactive run hold for the typed lines** (finding 11): the first failing command ends the run with its exit code unless `-k` was given; Ctrl-C during a command ends the run with 130 after the cleanup; Ctrl-C between commands exits 130 |
| a terminal | `--json -C …` or `-S` | the commands, then the line editor | the commands, then exit, as with `-N` |
| a terminal | `--json`, `get` onto an existing file | `Overwrite? [y/N]` | `already_exists`, exit 1: nothing is asked |
| a terminal | `--json`, no `-p` | the password prompt | the same: it is written to the terminal device, not to stdout |
| a terminal | `--json -N`, nothing to run | `usage` | the same |
| not a terminal | any | | unchanged |

The banner is not printed (`if interactive`). The rows above are what `main.rs` does today for
stdin lines (`exit(c)` unless `--keep-going`, `Flow::Failed(130) => exit(130)`); nothing is
added for a terminal, and the manual and `docs/scripting.md` say so in these words.

### 5.6 The manual

- `--proxy`'s help (clap, so also `help all`): `Connect through a SOCKS5 proxy:
  socks5://[user:pass@]host:port (the proxy resolves the server's name; percent-encode special
  characters in user and password; IPv6 proxy in brackets; visible in the process list)`.
- A topic `proxy` in `topics()`, after "timeouts and reconnect" (8 topics; `help_all` prints
  `topics[3..]` as now): the URL forms and what is refused; the proxy resolves the server's name
  and the host never does, the proxy's own name is resolved on the host; every connection and
  reconnect goes through the proxy, nothing else is dialled; no environment variable is read;
  credentials are visible in the process list and never printed; a failure is kind `connect`,
  exit 3 at start-up, with a message that starts `proxy <host>:<port>: `, and the three shapes
  of it (the proxy cannot be reached; the proxy refuses the login; the proxy cannot reach the
  server); a proxy that cannot be reached is an error, never a reason to connect directly; the
  dial has max(10 s, `--timeout`); the cost (the TCP connect and 2 round trips to the proxy, 3 with a
  login, once per connection; nothing per command); what a proxy that holds back small segments
  does to walks and small files.
- The examples topic gains one line: `rsmb --proxy socks5://127.0.0.1:1080 -H
  fileserver.example -u alice -d CORP -N -C 'use docs' -C 'ls'`.
- The JSON paragraph: "`--json` never starts the line editor: with a terminal on stdin lines
  are read from it one by one, under the rules of a non-interactive run (the first failing
  command ends the run unless -k; Ctrl-C during a command ends it with 130)" in place of the
  sentence about the prompt.
- The timeouts topic and the help of `--timeout`: "Connecting has max(10 s, --timeout)" in place
  of "its own 10 s limit".
- `EXIT_CODES`, 3: "connect or login failed at start-up, a proxy failure included".

### 5.7 `smbtap`: a SOCKS5 proxy as upstream

```rust
impl Tap {
    /// Like `start`, with a SOCKS5 proxy as upstream: the handshake in front of the SMB stream
    /// is relayed, each message in one write, and not counted.
    pub fn start_socks5(proxy: impl ToSocketAddrs) -> io::Result<Tap>
}
/// RFC 1928 and 1929: greeting, method, user and password if the proxy chose them, request,
/// reply. Reads each message whole, then writes it once.
fn socks5_preamble(client: &mut TcpStream, proxy: &mut TcpStream) -> io::Result<()>
```

- `start` and `start_on` keep their signatures; all three go through one private constructor
  with a `socks5: bool` that `serve` reads.
- `serve`: connect upstream, set `TCP_NODELAY` on both, put the two sockets into `live` (so
  `drop_connections` and `Drop` reach a connection that is still in its handshake), then, in
  SOCKS5 mode, `socks5_preamble`; if it fails (the proxy refused and closed, or the bytes are not
  SOCKS5), both sockets are shut and the connection is closed in the engine. Then the two pumps
  as now. The engine sees no handshake byte, so "the first send on a connection starts flight 1"
  holds for the NEGOTIATE.
- Message lengths: greeting `VER NMETHODS` + methods; method `VER METHOD`; if the method is 2:
  `VER ULEN` + user + `PLEN` + password, answer `VER STATUS`; request and reply `VER x RSV ATYP`
  + 4, 16 or (1 + n) address bytes + 2. **One write per message**: microsocks parses one `recv`
  per message and answers a split request with `General SOCKS server failure` (2.3).
- `connections` counts a connection whose handshake the proxy refused (it was accepted by the
  tap); it has no frames.
- The command line of `smbtap` is unchanged: only tests use the mode, through the library
  (finding 10).
- `validate-smbtap.sh` is unchanged (the counting code is). The cross-check of the new mode is
  the table of 2.3: the same 33 commands, the same counts as the direct tap.

### 5.8 What does not change

`Target`, `Proxy`, `login`, `ensure_connected`, the reconnect and rerun rules, `abandon`, the
15 kinds, `Error::exit_code`, the command table and its 19 commands, the cache, the walk,
transfers. Completion, the prompt and `pwd` show the server as given with `-H`.

## 6. Tests

### 6.1 The harness, `tests/common/mod.rs`

```rust
pub enum Ep { Fast, Slow, Edge, Bad, Proxied, ProxiedLogin }
impl Ep {
    /// What a tap in front of the endpoint forwards to: the server, or for the proxied two the
    /// proxy's listener (10.213.45.20:1080, :1081).
    pub fn addr(self) -> &'static str
    /// The proxied two: "rsmb-samba-private".
    pub fn container(self) -> &'static str
    pub fn proxied(self) -> bool
    /// A new tap in front of the endpoint: `Tap::start`, or `Tap::start_socks5` for the proxied two.
    pub fn tap(self) -> Tap
    /// The flags that reach the endpoint, without the login. Direct: `-H <host> -P <port>` of the
    /// server, or of `tap`. Proxied: `--proxy socks5://[tester:testpass@]<proxy or tap> -H
    /// fileserver.rsmb.test -P 445`.
    pub fn route(self, tap: Option<&Tap>) -> Vec<String>
    /// The host as the prompt and `pwd` show it: the tap's 127.0.0.1 or the server's address for
    /// a direct endpoint, `fileserver.rsmb.test` for a proxied one.
    pub fn host(self, tapped: bool) -> &'static str
}
/// The endpoint of tests/fast.rs: `Ep::Fast`, or `Ep::Proxied` when RSMB_TEST_VIA=socks5 (any
/// other value panics).
pub fn fast() -> Ep
/// Lines of the proxy's log that contain `needle` (`docker logs rsmb-socks`).
pub fn proxy_lines(needle: &str) -> usize
/// An empty directory for this test **and this route**: `<CARGO_TARGET_TMPDIR>/direct/<test>` or
/// `…/socks5/<test>`, wiped at every call.
pub fn tmp(test: &str) -> PathBuf
impl Client {
    /// A session whose tap the test made itself; `args` is the whole command line (route and
    /// login). The client owns the tap; `take_tap()` gives it back, and dropping it closes its port.
    pub fn through(tap: Tap, args: &[&str]) -> Client
}
```

- `Client::tapped(ep)`, `Client::with(.., Some(ep), ..)`, `Pty::tapped`, `piped`, `Scratch::new`,
  `check_startup`, `check_use`, `check_cd`, `check_ls`, `check_tab_many` use `ep.tap()`,
  `ep.route(..)`, `ep.host(..)` and `ep.container()` instead of building `-H 127.0.0.1 -P <port>`
  and the prompt `//127.0.0.1> ` themselves. `Client::to(ep)` is the session without a tap;
  `Client::direct(addr)` and `target(addr)` stay for the tests that name an address.
- `tests/fast.rs`: `FAST`, `SAMBA`, `Ep::Fast`, `Tap::start(Ep::Fast.addr())` and the literal
  hosts in prompts become `fast()` and its methods (about 130 lines name the fast endpoint, a
  tap address or the container today). Afterwards
  `grep -nE '10\.213\.45\.10|"rsmb-samba"|Ep::Fast' tests/fast.rs` finds nothing; the lines of
  `startup_failures` that name 127.0.0.1 port 1 on purpose stay.
- `tmp` (finding 11): today it keys on the test's name alone and wipes the directory, so a
  direct and a proxied run of the same test at the same time (both hold only the shared lock)
  would delete each other's files and history. The route is part of the path.
- A test that passes its own `--proxy` (F61, N5, S63, S64, S65's last part) builds its whole
  command line itself and never adds to `route()`: in the proxied run `route()` already holds
  `--proxy`, a second one is refused by clap with exit 2, and a test that expects exit 2 for a
  bad URL would pass for that reason (2.15, finding 4).
- Nothing is skipped in the proxied run. Tests that need no server run twice. A test whose
  expectation names the route (the banner) asks `fast().proxied()`.
- A failure in the proxied run shows its route: `Client::what` and the pty transcript hold the
  arguments.
- The rules of Stages 2 and 3 hold on the private server: a `Scratch` per writing test, no walk
  into `upload/` outside `tests/serial.rs`, an observed event before a cut or a signal.

### 6.2 `./test.sh`

One new step after `tests/fast`, in both modes. It counts what the proxy logged for the private
server before and after, and fails unless the run added at least 100 lines:

```
step "tests/fast through the proxy"
proxy_log() { local l; l=$(docker logs rsmb-socks 2>&1); grep -c 'connected to fileserver.rsmb.test:445' <<<"$l" || true; }
before=$(proxy_log)
RSMB_TEST_VIA=socks5 flock -s "$lock" cargo test --release --test fast
added=$(($(proxy_log) - before))
[ "$added" -ge 100 ] || { echo "FAIL: tests/fast through the proxy added $added lines to the proxy's log, expected at least 100: did the tests see RSMB_TEST_VIA?" >&2; exit 1; }
```

Why (finding 2): if the variable does not reach the test binary, `fast()` gives the direct
endpoint, every test passes against `samba`, and the step is green without having touched the
proxy. Why 100, and why it proves this step without the exclusive lock:

- A direct run of the binary opens 149 TCP connections on the server (2.15), and every
  connection through the proxy is one such line (2.4). A proxied run therefore adds about 150,
  and a little more with the tests this stage adds.
- A run that went direct adds only what its tests ask of the proxy on purpose: the five
  accepted URL forms of F61. 5 is far below 100.
- Other holders of the shared lock can only add lines. Inside one `./test.sh` nothing else runs
  during the step. `check.sh` and `tests/serial` need the exclusive lock and cannot overlap. A
  second gate on the same machine could overlap with its `tests/slow` (about 20 such lines) or
  with its own proxied step (about 150). Only that last case, two gates in this step at the same
  moment, could hide a step that went direct; one gate at a time is how the project is worked
  (`AGENTS.md`, one agent per task), and `docs/testing.md` says that the check assumes it.
- The lines are counted with the output captured first (`AGENTS.md`: `grep` under `pipefail`).

Budgets: **150 s for `fast`, 600 s for the full run** (master plan section 6, after Stage 4).

| Step | Now | Expected | From |
|---|---|---|---|
| `check.sh` | 27 s | 27 | unchanged |
| unit tests | 2 | 3 | nine small tests, three with a fake proxy on a local socket |
| `tests/fast` | 13 | 15 | F61, F62, I61 (five short pty runs). 12.66 s for the test run itself, measured again (2.15) |
| `tests/fast` through the proxy | — | about 19 | not run (10, assumption 1). The floor is the 12 s of `idle_sends_nothing`; 2.10: single commands equal, `put -r` of many small files three times slower, `tree /huge` 0.3 s more; two `docker logs` calls |
| `tests/slow` | 24 | about 28 | its tests run side by side; S64 is 10 s (the direct dial that must time out, with `--timeout 5`), S63 about 10 s (the proxy that is not one, with `--timeout 5`), S62 about 8 s |
| `tests/serial` | about 329 | about 363 | X61 5 s, X62 25 s (15 s of silence and the 10 s dial), X63 4 s |
| tshark | 3 | 3 | |
| **counted** | **398** | **about 458 of 600** | |

`fast`: 43 s now, **about 64 of 150**. The review's changes add about 2 s to each (the pty
cases of I61 in both runs, the log count); the dial limit adds nothing, because every test that
waits for a dial passes `--timeout 5` and so keeps the 10 s. No new test is longer than 30 s. If the first full run
is over: X62's second half (the dial into the silent proxy, 10 s) goes first, then S63's
"not a proxy" case (10 s, in parallel, so it buys little).

### 6.3 Unit tests (no server)

| Test | File | Asserts |
|---|---|---|
| `proxy_urls` (extended) | session | as now, and: `socks5h://h:1` equals `socks5://h:1`; `socks5://h:1/` is accepted, `socks5://h:1//` and `socks5://h:1/x` are not; an empty user, an empty password, a user and a password of 256 bytes after decoding are `usage`, 255 bytes are accepted; `SOCKS5://`, `socks4://`, `socks4a://`, `socks://`, `http://`, `https://`, no scheme, port 0, port 65536 are `usage`; **every refused form gives the same message, which holds `expected socks5://` and `SOCKS5 only` and no part of the input**: checked with `socks5://user1:secretpw@host` (no port), `socks5://alice:hunter2` (no `@`), `user1:secretpw@host:1` (no scheme) and `socks5://user1:sec%zzret@host:1` (master row 4.9, finding 5) |
| `socks5_handshake_bytes` | session | a fake proxy on `[::1]` and one on 127.0.0.1 that record what they read, through `parse_proxy` and `open_tcp`: without credentials the greeting is `05 01 00`; **for the target `fileserver.invalid` the request is `05 01 00 03`, the length, the name's bytes, the port: a name, not an address**; `10.1.2.3` goes as type 1, `::1` as type 4; with `socks5://us%40er:p%3Ass%2F%25w%C3%B6rd@…` the greeting is `05 02 00 02` and the login is `01`, 5, `us@er`, 11, `p:ss/%wörd`; `TCP_NODELAY` is set (the assertion of `dial_sets_nodelay`, which uses the same helper now) |
| `proxy_errors` | session | the fake proxy answers: method `ff` without and with credentials; login status 1; replies 1, 3, 4, 5, 6, 2, 7, 8; `HTTP/1.1 400` ; closes after the greeting. Each: `Kind::Connect`, the message starts `proxy <hostport>: `, has the words of 5.3, names the target for a reply, never holds the password; exit code 3 at start-up |
| `dial_says_how_far_it_got` | session | `open_tcp` with a limit of 200 ms against a listener that accepts and never answers: `connected to the proxy, but the SOCKS5 handshake did not finish`, the three causes and the target, within a second; `hostport("::1", 1080)` is `[::1]:1080`; `dial_limit`: 1 s and 5 s give 10 s, 10 s gives 10 s, 30 s gives 30 s, 120 s gives 120 s (findings 3 and 8) |
| `banner_names_the_proxy` | session | the pure part of `describe` (host, port, user, proxy, dialect → the line): with a proxy that has credentials the line holds its host and port and neither user nor password; without a proxy the word `proxy` is absent |
| `args_are_sound` (extended) | main | the help of `--proxy` says the proxy resolves the name; the help of `--timeout` names the dial |
| `table_is_complete`, `help_all_has_everything` (extended) | commands | 8 topics; the topic `proxy` (with "never a reason to connect directly"); the example line; the JSON paragraph no longer speaks of a prompt on stdout and names the non-interactive rules; the timeouts topic says max(10 s, --timeout) |
| `socks5_preamble_is_relayed_and_not_counted` | smbtap | a fake proxy thread that reads each message with **one `read`** and fails the test if a message arrives in pieces; a client that sends greeting, login, request and then two SMB2 frames: the tap's stats show 2 requests in 1 flight on 1 connection and nothing else; without a login the same; a proxy that answers `05 ff` and closes: the client sees the two bytes and then the end of the stream, 0 frames, `live_connections()` 0; `drop_connections()` during a handshake that stalls closes it |

### 6.4 Integration tests

Every test runs the real binary. "The private server" is reached as `fileserver.rsmb.test`
through the named port; "tapped" means the tap of 5.7 between rsmb and the proxy.

**`tests/fast.rs`** (each run twice: direct to `samba`, and through port 1080 to the private
server)

| # | Test | How observed | Asserts | Master row |
|---|---|---|---|---|
| — | **every test of Stages 1 to 3 in this file** (66 tests today) | as now, against `fast()` | unchanged: outputs, exit codes, sha256 on both sides (the server's from `rsmb-samba-private` in the proxied run), requests and flights per command, connections in the tap, the pty sessions, the examples of `help all` | 4.1 |
| F61 | `proxy_urls_at_startup` | one-shot runs, text and `--json`; **every command line built in the test (`--proxy <url> -H fileserver.rsmb.test`, the login, `-N -C pwd`), never from `route()`** (finding 4) | each refused form of 2.8 and P5: exit 2, with `--json` one `error` of kind `usage` and an empty stderr, **the message holds `expected socks5://`** (the parser's text: clap's refusal of a second `--proxy` has the same exit code and must not pass); for `socks5://user1:secretpw@10.213.45.20` and for **`socks5://alice:hunter2`** neither stdout nor stderr holds `user1`, `secretpw`, `alice` or `hunter2` (finding 5); with `--proxy socks5://10.213.45.20:1080` a `-H` of 256 bytes: exit 2, `usage`, the message holds `255`; a `-H` of 255 bytes: exit 3, `connect` (the proxy was asked). Accepted and working against the private server (`pwd` after `use share`, exit 0): `socks5://10.213.45.20:1080`, `socks5h://…`, `socks5://10.213.45.20:1080/`, `socks5://tester:testpass@10.213.45.20:1081`, and port 1080 with credentials it does not need; `help all` has the topic `PROXY` and the example line | 4.6, 4.9 |
| F62 | `environment_is_not_read` | `run_env` with `ALL_PROXY`, `all_proxy`, `SOCKS_PROXY`, `RSMB_PROXY` all `socks5://127.0.0.1:1` | the session to `fast()` works as without them (exit 0, `ls /sizes` 11 entries) | P8 |
| I61 | `json_never_starts_the_shell` | pty | `rsmb <route> --json -C pwd`: ends by itself with exit 0; the transcript has no ESC byte and no `> `; its two lines parse as JSON (`pwd`, `done`). `rsmb <route> --json`, then `pwd` and Ctrl-D typed: exit 0, the lines that start with `{` parse, no ESC byte, no banner. **Typed `cd /nope` (after `use share`), then `pwd`: the run ends after the first with exit 1 and no `pwd` object; with `-k` both run and Ctrl-D gives exit 1** (finding 11). In a scratch directory, `--json -C 'get <file that exists locally>'` on the pty: `already_exists`, exit 1, no `Overwrite?` | P12 |
| I1 | `interactive_basics` (extended) | pty | the banner: `through proxy 127.0.0.1:<tap port>` in the proxied run, no `proxy` in the direct one | P11 |
| N5 | `startup_failures` (extended) | the command line built in the test, not with `run_fast` (finding 4) | `--proxy http://x`: exit 2, and with `--json` kind `usage` and a message that holds `expected socks5://` | 4.6 |

**`tests/slow.rs`** (parallel, shared lock)

| # | Test | How observed | Asserts | Master row |
|---|---|---|---|---|
| S61 | `proxy_with_login` | `Client::tapped(Ep::ProxiedLogin)`, `session_script` (every command) in a `Scratch` on the private server | every line `ok`; the downloaded `65537.bin` and the uploaded one equal the server's sha256; the tree of `put -r d01` equals `smallfiles/d01` on the server (`server_tree`); `find /tree -name file.txt` 340 entries equal to `server_find` there; "expected 1 TCP connection in the tap for the whole session"; exit 0 | 4.2 |
| S62 | `big_file_through_the_proxy` | `Client::tapped(Ep::Proxied)`; `get /large/big.bin`, `put big.bin` into a `Scratch`; `Remove` guard for the local file | sha256 `ad2f1026…` locally and on the server; `bytes` 1,181,116,007 both ways; never more than 16 outstanding; peak RSS ≤ 64 MB; 0 new connections | 4.8 |
| S63 | `proxy_failures` | one-shot runs, `--json` and text, each command line built in the test; the proxy password in the URL a string of its own (`Wr0ngSecret`) | the cases of 2.5 with the messages of 5.3. Each: exit 3, kind `connect`, one `error` with `"command":null` and no `done`, the message starts `proxy 10.213.45.20:<port>: `, text mode prints the same line after `rsmb: `, **no output holds the URL's password**. Wrong password and wrong user at 1081: `user name or password refused`, within 2 s. No credentials at 1081: `wants a user name and password`. Port 1082: `refused`, within 2 s. **Port 1082 with `-H 10.213.45.10`, a server the host reaches without any proxy: the same failure, naming `proxy 10.213.45.20:1082`; it must not connect** (finding 1). Target port 446: `cannot reach fileserver.rsmb.test:446: connection refused`. `-H nosuch.rsmb.test`: `cannot reach nosuch.rsmb.test:445`. `-H 10.213.47.99`: `cannot reach 10.213.47.99:445: host unreachable`, at most 12 s. `--proxy socks5://10.213.45.10:445 --timeout 5`: at most 12 s, `connected to the proxy, but the SOCKS5 handshake did not finish within 10 s` and the three causes (finding 3). A wrong SMB password through port 1080: exit 3, kind **`auth`**, no `proxy` in the message | 4.6 |
| S64 | `no_way_around_the_proxy` | one-shot runs | precondition, asserted: `("fileserver.rsmb.test", 445).to_socket_addrs()` fails on the host. Direct to the name: exit 3, kind `connect`, at most 12 s. Direct to 10.213.47.10 with `--timeout 5`: exit 3, kind `connect`, at most 12 s. **No lower bound on either** (finding 9): how fast a lookup fails and whether an unreachable address is refused at once or times out is the host's network, not rsmb. Through port 1080 by name, by `10.213.47.10` and by `FileServer.RSMB.Test`: `ls /sizes` 11 entries. The loader probe (2.7): with `LD_DEBUG=files`, the control (direct to the name) loads at least one `libnss_` module, and then the proxied run by name must load none; if the control loads none, the test prints `leak probe blind on this host` and goes on | 4.4, 4.5 |
| S65 | `cut_through_the_proxy` | `Client::tapped(Ep::Proxied)`; `drop_connections()`. Last part: `Client::through(Tap::start_socks5("10.213.45.20:1080"), …)` with `--proxy socks5://127.0.0.1:<tap port> -H 10.213.45.10`, then `drop(c.take_tap())` | idle cut: `pwd` 0 requests; `ls` succeeds on "exactly 1 new connection", 9 requests. Cut at the first QUERY_DIRECTORY of `ls /huge`: the command succeeds with 100,000 entries, 1 new connection. Cut at the 100th READ of `get /large/big.bin`: `connection_lost`, the "not run again" tail, no local file and no local temporary file, 0 new connections for the failed command. Cut at the 100th WRITE of `put big.bin` to a new name in a `Scratch`: `connection_lost`; afterwards the directory on the server holds neither the name nor a `.rsmb-*.part` (`wait_no_open`, then `temps_on_server`); the next `ls` works. **The proxy gone, the server reachable directly** (finding 1): through the tap to `samba`, `use share` and `ls /sizes` work (11 entries, 1 connection in the tap); the tap is dropped, so the proxy address rsmb has is a closed port; `ls /sizes` **fails**, kind `connect`, the message holds `reconnect failed: proxy 127.0.0.1:<port>`; a second `ls` the same; `pwd` works. A client that fell back to a direct connection would list 11 names (measured today: it fails, 2.15) | 4.7, 4.8 |
| S66 | `signals_through_the_proxy` | tapped, non-interactive; SIGINT at the 100th READ of `get /large/big.bin`; SIGTERM the same; SIGINT at the 100th WRITE of a `put` | each: an `error` of kind `interrupted`, exit 130 within 3 s (4 s for the `put`); no local file, no temporary file on either side, no `left_behind` | 4.8 |

`socks5_smoke` is removed (D5).

**`tests/serial.rs`** (one at a time, exclusive lock)

| # | Test | How observed | Asserts | Master row |
|---|---|---|---|---|
| X61 | `one_connection_through_the_proxy` | for `Ep::Proxied` and `Ep::ProxiedLogin`: `conns-reset samba-private`; `proxy_lines("connected to fileserver.rsmb.test:445")` before; `session_script` through the tap; then both again | "expected 1 TCP connection in the tap, 1 on the server (`ctl.sh conns samba-private`) and 1 more line in the proxy's log". Then a session with `-H FileServer.RSMB.Test`: `proxy_lines("connected to FileServer.RSMB.Test:445")` rises by 1 (the name goes out as given). Then `drop_connections()` and `ls`: the log has 1 more line for that name and the server counts 2 | 4.1, 4.3, 4.7 |
| X62 | `silent_proxy` | `Client::to(Ep::Proxied)` with `--timeout 5` (so the dial limit is 10 s); `ctl.sh pause socks` with a `Guard` that unpauses | `ls /sizes` fails between 13 and 20 s with `connection_lost`; `pwd` at once; `ls` again fails between 9 and 13 s with kind `connect` and a message that holds `reconnect failed: proxy 10.213.45.20:1080`, `the SOCKS5 handshake did not finish within 10 s` and `is silent` (finding 3: the proxy is silent here, and the message does not blame the server); after the unpause `ls` lists 11 names | 4.6 |
| X63 | `ctrl_c_through_the_proxy` | `Pty::tapped(Ep::Proxied, …)`; `get /large/big.bin`, Ctrl-C at the 100th READ | `interrupted`, the prompt `//fileserver.rsmb.test/share/> ` back within 3 s; no local file; `ls /sizes` lists 11 names on "exactly 1 new connection"; `pwd` unchanged | 4.8 |

Rows of master plan section 10 and where each is enforced: 4.1 the proxied run of
`tests/fast.rs`, X61 · 4.2 S61 · 4.3 X61 · 4.4 S64, `socks5_handshake_bytes`, X61 (the spelling)
· 4.5 S64 · 4.6 S63, F61, N5, X62, `proxy_errors`, `dial_says_how_far_it_got` · 4.7 S65, X61 ·
4.8 S62, S65, S66, X63 · 4.9 `proxy_urls`, `socks5_handshake_bytes`. Beyond the rows: no
fallback to a direct connection S63, S65 · the proxied step really proxied `test.sh` (6.2).

### 6.5 Not in the gate, and why

A proxy other than microsocks (none in the environment). Latency between client and proxy (the
environment has none; computed in 2.10). A proxy that refuses a login on a reconnect (the
credentials of a session do not change). An IPv6 server. The packet capture of 2.7 (repeated by
hand at the end, 6.6). `tests/slow.rs` through the proxy (S4-2). The wall time of anything
through the proxy (it measures microsocks; `docs/performance.md` records the planning
measurement). A dial at the default `--timeout` that gets no answer (30 s for what
`dial_limit`'s unit test and the 10 s cases already show). A target whose SYN is dropped
behind the proxy (10 s under the exclusive lock for the message `dial_says_how_far_it_got`
already covers). A proxy paused at start-up (the same dial as X62's second `ls`).

### 6.6 The bench, the final run, the final table

**No `proxy` mode in `bench-rsmb.sh`** (findings 7 and 10). The proxied figures are the ones
measured while planning: the counts of 2.3 and the three tables of 2.10 go into
`docs/performance.md`, section "Through a SOCKS5 proxy", marked as measured on 2026-11-18 with
the build of commit `93712f1`, microsocks 1.0.5, and with their method in full so that anyone
can repeat them by hand: the command lines (`--proxy socks5://10.213.45.20:1080 -H
10.213.45.30` beside `-H 10.213.45.30`), one session per route under the exclusive lock, each
command timed from its line to its `done` object, medians of 20 for `cd` and the small `ls`
and of 5 for `ls /many`, one run for each walk and transfer, `ctl.sh drops` 0. The throwaway
scripts are not committed (`docs/development.md`, "Throwaway measurements").

**`bench-rsmb.sh transfers`** gains: `medium.bin` down and up with smbclient-ng at the slow
endpoint, 3 runs each, time with the command minus time without, the login line and the absence
of `[error]` checked as in `bench-smbclientng.sh`, sha256 of the result on the receiving side;
and rsmb's `medium.bin` by the same method (process time), 3 runs. About 6 minutes more (2.13).

**The final run**, by the implementing agent on the final code, nothing else running:

| Order | Command | About |
|---|---|---|
| 1 | `./testenv/bench-smbclientng.sh \| tee testenv/baseline-smbclientng.txt` (a fresh baseline: the image changed since 2026-11-17) | 18 min |
| 2 | `./testenv/bench-rsmb.sh` | 8 min |
| 3 | `./testenv/bench-rsmb.sh transfers` | 27 min |
| 4 | `./testenv/bench-rsmb.sh walk` | 6 min |
| 5 | the capture of 2.7 once more (proxied by name, and the control), by hand | 1 min |

**The final table**, in `docs/performance.md` and, shortened to its first seven rows, in the
README, with the key Stage 0 numbers below it (section 9). Every cell from the runs above,
except the proxy row, which is the planning measurement and says so; the figures below are what
the earlier runs and section 2 gave, as what to expect:

| At 100 ms each way | smbclient-ng 3.1.0 | rsmb | Faster by |
|---|---|---|---|
| `cd` | 1,610 ms; 6 requests in 6 round trips; 2 new TCP connections | 201 ms; 2 requests in 1 round trip; none | 8× |
| `ls`, 5 entries | 1,208 ms; 5 round trips | 201 ms; 5 requests in 1 round trip | 6× |
| `ls`, 10,000 entries | 4,535 ms; 17 round trips | 424 ms (1.5 s as the first listing of a connection); 1 round trip | 11× (3×) |
| `ls`, 100,000 entries | 37.3 s; 139 round trips | 3.2 s; 1 round trip | 12× |
| `tree`, 85 directories | 69.3 s; 342 round trips | 1.61 s; 340 requests in 60 flights | 43× |
| download, 50 MiB | 18.3 s (one run while planning) | 4.3 s | 4× |
| upload, 50 MiB | 68.5 s (one run while planning) | 5.1 s | 13× |
| 1.1 GiB each way | not run (about 6 and 24 minutes at those rates) | 62.8 s and 63.5 s, at the rate of iperf3 | |
| the same through a SOCKS5 proxy (measured while planning, 2026-11-18, one session each) | no proxy option | `cd` and `ls` unchanged (201 ms); walks and small files 1.1 to 1.3×, a 50 MiB download 1.6× the direct time with microsocks | |
| a second Tab in a directory | a full listing each time | 0 requests | |

## 7. Cleanup before the review of the whole codebase

Each item is from 2.14 or from a limitation noted in an earlier stage. Documents are fixed in
the task that makes the new text true (section 8).

| # | Item | What is done |
|---|---|---|
| C1 | `--proxy` credentials and IPv6 "only the URL parser is tested" (`docs/known-limitations.md`) | `socks5_handshake_bytes` reads the bytes at a fake proxy on `[::1]`; the line becomes: tested against a fake proxy and once by hand against a python proxy (2.8); the test proxy's own credentials are plain |
| C2 | `--json` on a terminal writes the prompt to stdout | fixed (5.5, I61); the sentences in `docs/scripting.md` and `help all` replaced |
| C3 | The password in the usage message | fixed: the message no longer repeats the URL (5.2, F61, `proxy_urls`) |
| C4 | Empty and over-long proxy credentials exit 3 | `usage` (5.2) |
| C5 | Proxy messages in the library's words | 5.3 |
| C6 | `socks5_smoke` | removed (D5) |
| C7 | README | final form (section 9), with the final table and, below it, a short table of the key Stage 0 numbers and the reasons for the library, which the brief asks for |
| C8 | "as of Stage 3", "What Stage 4 adds where", "later stages", "not in Stage 1/2/3" | every document describes the finished program; a sentence that names a stage survives only where the stage is the subject (the reviews, the plans, the history of a number) |
| C9 | `docs/testing.md`: two "longest tests" paragraphs, three sets of step times | one paragraph, one table of step times from the final gate run; the earlier figures in one line |
| C10 | Budgets and durations in `AGENTS.md`, `README.md`, `docs/development.md`, `docs/testing.md`, `docs/test-environment.md` | 150 s and 600 s and the measured times, the same in all five |
| C11 | `docs/development.md` dependency list against `Cargo.toml` | checked line by line (it is right today; `indicatif` of the master plan was never added) |
| C12 | `docs/performance.md` "Not done" | every entry either done in the final run (the baseline, smbclient-ng's transfers) or kept as a deliberate "not built" |
| C13 | A sweep for what the compiler cannot see | `grep -rn 'allow(' src smbtap/src` (two today, both justified in a comment); `cargo clippy --workspace --all-targets -- -D warnings` clean; every `pub fn` of `tests/common/mod.rs` used by a test (the file allows dead code because three binaries share it: list the unused with the attribute removed for one build per binary, delete them, restore the attribute); every `testenv/*.sh` mode named in `AGENTS.md` |
| C15 | "Connecting has its own 10 s" in `help all`, `docs/commands.md`, `docs/architecture.md`, `docs/known-limitations.md` | max(10 s, `--timeout`) everywhere (P9) |
| C14 | The scratch directories of the stages (`spikes/smb2/target/scratch-*`, `adv-*`, `review-*`) | ignored by git and not part of the product; nothing to do but check `git status` is clean and `git ls-files spikes` holds only the two spikes and their result files |

## 8. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` runs after T0, T4
and T6. Documentation is written with the task that makes it true.

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T0 | The tap and the harness | — | `Tap::start_socks5`, `socks5_preamble` (5.7); `Ep::Proxied`, `Ep::ProxiedLogin`, `tap`, `route`, `host`, `fast()`, `proxy_lines`, `Client::to`, `Client::through`, `tmp` with the route (6.1); `tests/fast.rs` on `fast()`, with `startup_failures`' proxy line built without `route()`; the step with its log count and the budgets in `test.sh` (6.2); `socks5_smoke` removed; `docs/measuring.md` (the mode), `docs/testing.md` (the second run and its check) | `socks5_preamble_is_relayed_and_not_counted`; **all of `tests/fast.rs` through the proxy**. The first run gives the real duration of the new step (assumption 1), the real number of log lines (assumption 18: if it is not about 150, find out why before touching the floor) and any assertion bound to the direct route (assumption 2). Check once by hand that the step fails when the variable is taken out of its line. **The full `./test.sh` here** |
| T1 | URL, messages, the dial limit, banner | T0 | 5.2 (with the `-H` check in `main.rs`), 5.3, 5.4; the flag rows of `--proxy` and `--timeout` and the new section "Proxy" in `docs/commands.md`; C15 | `proxy_urls`, `socks5_handshake_bytes`, `proxy_errors`, `dial_says_how_far_it_got`, `banner_names_the_proxy`; F61, F62, N5, I1; S63. After it, the three serial tests that wait for a dial (`silent_link_blackhole`, `silent_link_mid_put`, `leftover_temp_is_reported_once`) are run once by name: they pass `--timeout 5` and must still see 10 s |
| T2 | `--json` and the line editor | T0 | 5.5; `docs/scripting.md` (with the rules for typed lines), `docs/commands.md` ("How commands are given"), `docs/architecture.md` (start-up) | I61 |
| T3 | The manual | T1, T2 | 5.6; `docs_match_the_table` checks the section "Proxy" | `table_is_complete`, `help_all_has_everything`, `args_are_sound`, H1 to H4 |
| T4 | The proxy tests | T1 | nothing new in `src/` unless a test shows a defect | S61, S62, S64, S65, S66; X61, X62, X63; **the full `./test.sh`** |
| T5 | Cleanup | T4 | section 7, C7 to C14 as far as they do not need the final numbers | — |
| T6 | Bench, final numbers, final documents | T5 | the smbclient-ng transfer rows in `bench-rsmb.sh transfers`; the final run of 6.6; the documents of section 9, `docs/performance.md` with the planning measurements of 2.3 and 2.10; the final full gate and its budget line | — |

If a count in the proxied run of `tests/fast.rs` differs from the direct run, or a count in S61,
S62, S65 or X61 from section 2, the implementing agent finds out why before changing a number:
the numbers were measured, and a difference is a defect or a finding to record under
"Deviations during implementation" at the end of this file.

## 9. Documents

No new document. Each in the task that makes it true:

| Document | What changes |
|---|---|
| `README.md` (final form) | what rsmb is; **status: all four stages done**; "Tested against Samba 4.23.8 only; no claim for Windows servers"; using it (the existing examples and one with `--proxy`); the final table of 6.6 (first seven rows and the proxy row) with the date, the commit and the two versions; the three uses in three lines (interactive, scripted with `--json`, developed with `./test.sh`); the library: the reasons for `smb2` in one paragraph and **a short table of the key Stage 0 numbers** (per crate: requests and time for connect, share list, a directory listing, the directory check and a file read, with and without the 100 ms, and whether it takes a caller's stream), with the link to `docs/library-decision.md` for the rest. The brief asks for the measurements and reasons in the README (finding 6); the table stands below the final one and is labelled as the Stage 0 spike; the test environment with the measured gate times; the documents; where the limitations and the reviews are |
| `docs/performance.md` (final) | first the final comparison (6.6); then targets and measured values, with the rows "through the proxy: the same requests and flights" (2.3) and the big file; navigation, transfers and walks from the final run; a section "Through a SOCKS5 proxy" (the counts of 2.3 and the three tables of 2.10 as **measured while planning**, with the date, the build and the method of 6.6; the handshake's round trips; what microsocks does to bursts and why); method; "Not done" in its final state |
| `docs/known-limitations.md` (final, complete) | reorganised by topic instead of by stage (server compatibility; connections and timeouts; the proxy; names and paths; commands and output; transfers; `tree` and `find`; out of scope; Samba; the library; the test environment). New, the proxy: SOCKS5 only; no proxy from the environment; credentials visible in the process list, no prompt for them; the proxy's own name is resolved on the host; the dial has max(10 s, `--timeout`) and no flag of its own, so at the default an unanswered dial takes 30 s; a timeout after the TCP connection to the proxy stood cannot say whether the proxy, its protocol or the server behind it was the cause; a proxy that cannot be reached is an error even when the server could be reached directly; with `--proxy` the server name is at most 255 bytes; the proxied timings are single runs from planning, not a bench mode; the message for an unknown name is as good as the proxy's answer; a proxy that holds back small segments slows walks, small files and transfers (the numbers); only microsocks was tested; latency between client and proxy not measured; an IPv6 proxy tested with a fake proxy only, an IPv6 server not at all, no brackets around `-H`; the loader probe is blind on a host that loads no name-service module. **Complete** means: every entry under "assumptions not verified" and "not in the gate" of the four stage plans, every "not fixed" and "not checked" of `docs/reviews.md`, every "Not done" of `docs/performance.md` and the brief's "not in scope" is either in this document or was resolved, and the implementing agent lists the resolved ones under "Deviations during implementation" |
| `docs/commands.md` | the `--proxy` row; a section `## Proxy` (the content of the topic in 5.6, with every message of 5.3 and its exit code); "How commands are given": `--json` never starts the shell, and typed lines follow the non-interactive rules; "Timeouts and reconnect": the dial limit; exit code 3; "Differences from smbclient-ng": it has no proxy option |
| `docs/scripting.md` | `--json` with a terminal on stdin: no line editor, and **the rules of a non-interactive run for what is typed** (the first failing command ends the session unless `-k`; Ctrl-C during a command ends the run with 130; Ctrl-C between commands exits 130; no overwrite question) (finding 11); a proxy failure as an `error` object (kind, `command: null`, the message's start); an example run through a proxy |
| `docs/architecture.md` | the dialer: `dial_limit`, `open_tcp` with its limit and stage, `proxy_error`, `hostport`, that nothing else dials, that there is no fallback, and why a reconnect goes the same way; start-up rule 5 with `--json`; the banner; "as of Stage 4"; the section "What Stage 4 adds where" removed |
| `docs/testing.md` | the second run of `tests/fast` and `RSMB_TEST_VIA`, the log count that proves it with its floor and the one case it cannot see (two gates in that step at once); `Ep::Proxied`, `route`, `host`, `fast()`, `proxy_lines`, `Client::through`, `tmp` with the route; that a test with its own `--proxy` never uses `route()`; the steps, the budgets 150 and 600, one table of measured step times; every new test with its row; that proxy-log and server-side counts are serial; what is not in the gate (6.5); C9 |
| `docs/test-environment.md` | "Tests": the new step, the budgets, that `tests/fast` now also needs `socks` and `samba-private`; the lock table: the new step (shared); that the proxy reaches `samba` and `wan` on `pub` (what the no-fallback tests and the planning measurement at the slow endpoint use); microsocks: one `recv` per handshake message, no `TCP_NODELAY` |
| `docs/measuring.md` | `Tap::start_socks5` (library only): where the tap sits, what it relays and does not count, the one-write rule, the table of 2.3 as its cross-check; how the smbclient-ng transfer rows are taken; the new baseline's date |
| `docs/development.md` | how to run one test through the proxy (`RSMB_TEST_VIA=socks5 cargo test --release --test fast <name>`); that a new test in `tests/fast.rs` names `fast()`, never an address; the durations |
| `docs/library-decision.md` | nothing unless the implementation meets a crate behaviour not yet recorded |
| `docs/transfers.md` | one line: the rules hold through a proxy (S62, S65, S66) |
| `docs/reviews.md` | the Stage 4 section (the whole codebase), after the review |
| `AGENTS.md` | the index row for this plan (added with the plan) and the changed rows; "Working commands": the budgets, the new step with its check, an rsmb line through the proxy; principles, **only those the work has shown** (candidates with evidence from section 2: forward a protocol message in one write when the peer may parse one read, 2.3; prove a negative with a control that shows the probe can see, 2.7; put the tap where the path under test begins so that the proof by construction survives, 2.3; count shared logs under the exclusive lock, 2.6; a test for "goes through X" needs a case where going around X would succeed, 2.15; a rerun selected by a variable must prove the variable arrived, 6.2) |

## 10. Assumptions this plan could not verify

Marked so that the adversary can attack them. Each says what would show it wrong.

1. **The proxied run of `tests/fast.rs` takes about 19 s.** It was not run: the harness does not
   exist. The figure comes from the direct 13 s, the 12 s floor of `idle_sends_nothing` and the
   command times of 2.10. T0's first run measures it.
2. **Every test of `tests/fast.rs` passes through the proxy with its assertions unchanged.** 33
   commands were counted and every command was run (2.2, 2.3); the 66 tests were not. A test may
   hold a host in an expected message, a time bound that microsocks' 40 ms breaks (`put -r` of
   many small files is three times slower, 2.10), or a use of the `samba` container by name.
3. **Only microsocks 1.0.5 was measured.** Its answers (general failure for an unknown name, no
   `TCP_NODELAY`, one `recv` per message, "no login" preferred when both are offered) are its
   own. OpenSSH's dynamic forwarding, Dante and Tor were not tried; the handshake is standard,
   the messages and the timing are not.
4. **Latency between client and proxy** was never produced. "1 + 2 round trips more per
   connection, 3 with a login" is counted from the messages (2.8), not timed.
5. **IPv6**: the proxy at `[::1]` was a python script; no IPv6 SMB server exists here, so what
   the server makes of `\\::1\share` is unknown.
6. **Ctrl-C in the interactive shell during a proxied transfer** was not run; SIGINT in a
   non-interactive run was (2.4). X63 is the first run.
7. **`--json` on a terminal after the change** was not prototyped (it is a change in `src/`).
   That typed lines are echoed by the terminal and not by rsmb, and that the password prompt
   goes to the terminal device and not to stdout, rests on reading `rpassword` and
   `docs/architecture.md`; I61 shows it.
8. **The loader probe** works on this host because its `nsswitch.conf` names modules that are
   loaded at run time. On a host with `hosts: files dns` and a recent glibc nothing is loaded
   and the probe is blind; S64 then proves resolution by the proxy only through the
   precondition, the bytes and the log. A statically linked build would be blind as well.
9. **The packet capture** saw the control's queries here; a resolver that caches the negative
   answer or encrypts its queries would hide a leak from it. It is a recorded measurement, not
   a test.
10. **smbclient-ng's transfer times are one run each** (2.13), and its navigation baseline was
    not re-run while planning; that `bench-smbclientng.sh` still passes on the rebuilt image is
    expected, not checked.
11. **The tap's relay** was a prototype of 45 lines in a copy of `smbtap`; its unit test and the
    handling of a handshake that stalls (`live` before the preamble) are designed, not run.
12. **`samba-private` admits only the proxy** (`hosts allow`): configured, never checked
    (`docs/test-environment.md`). The proof here does not need it: the host has no route.
13. **Peak RSS through the proxy** was not read; it is expected to equal the direct figure,
    since nothing above the socket changes. S62 asserts it.
14. **The upper bounds of S63 and S64** (12 s) come from single runs of 3.06 s (the absent
    address, the proxy container's ARP timeout) and 10.00 s (the dial limit with `--timeout 5`).
    Lower bounds were removed after the review (finding 9).
15. **The budget estimate** (about 458 s of 600, about 64 s of 150) adds expected durations to
    the last measured run.
16. **The cleanup list** (2.14, section 7) comes from searching the documents and the code for
    stage words and markers, not from reading all 10,000 lines of `src/` again; the review of the whole
    codebase is expected to find what a search cannot.
17. **`find /fanout` and the other proxied ratios** (2.10) are one run each, and they are what
    `docs/performance.md` will carry: there is no bench mode that repeats them (findings 7, 10).
18. **The floor of 100 log lines** (6.2) rests on one direct run that opened 149 connections
    (2.15); the proxied run was never made, so "about 150 lines" is inferred from "one line per
    connection" (2.4). Tests added or removed later move the number; the floor leaves a third.
    Two gates in the proxied step at the same moment would hide a step that went direct.
19. **The dial limit at the default `--timeout`** becomes 30 s. No run was made with it (it is a
    change in `src/`); that no existing test waits for an unanswered dial at the default was
    read in `tests/serial.rs`, not run. Whether 30 s is what a person at the prompt wants for an
    address that does not answer was not asked of the user (the finding was accepted as written).
20. **Typed lines under `--json` on a terminal** ending the run at the first failure, and Ctrl-C
    during a command ending it with 130, is read from `main.rs`'s loop over stdin lines; I61 is
    the first run (with assumption 7).
21. **No fallback** was measured for a closed proxy port after a session stood and at start-up
    (2.15). A proxy that goes silent while the server is reachable directly was not tried; the
    code path is the same `open_tcp`.

## 11. Adversary findings and what changed

One adversary attacked the first version of this plan on 2026-11-18. All eleven findings were
validated by the orchestrating session and accepted. What this plan measured for the revision
is in 2.15.

| # | Finding | Evidence | What changed |
|---|---|---|---|
| 1 | **No test would notice a fallback to a direct connection** (medium): every failure case used a target the host cannot reach anyway | the proxy reaches the main server (`--proxy socks5://10.213.45.20:1080 -H 10.213.45.10` works, log `connected to 10.213.45.10:445`). Here (2.15): with the proxy port closed, start-up exits 3 and a standing session's `ls` fails with `reconnect failed: proxy …`; rsmb does not fall back today | S63 gains the closed proxy port 1082 with `-H 10.213.45.10`; S65 gains a session through a SOCKS5 tap to `samba` whose tap is then dropped: the next `ls` must fail with a message naming the proxy. `Client::through` in the harness (6.1). "No fallback" is stated in 5.3, the manual's topic and the known limitations |
| 2 | **The proxied rerun of `tests/fast` never checked that it went through the proxy** (medium): if `RSMB_TEST_VIA` does not reach the binary, `fast()` gives the direct endpoint and the step is green | the plan's own 6.1 and 6.2. Here: a direct run opens 149 connections and adds 0 lines to the proxy's log (2.15) | `test.sh` counts the proxy's log lines for the private server before and after the step and fails below 100 (6.2, with the reasons for the number and for doing without the exclusive lock, and the one case it cannot see); T0 checks by hand that the step fails without the variable; assumption 18 |
| 3 | **The timeout message blamed the proxy-to-server leg** when the proxy itself is silent or is not a SOCKS5 proxy (low to medium) | a paused proxy accepts TCP and never answers the greeting (2.6) | The third line of 5.3 is neutral: `connected to the proxy, but the SOCKS5 handshake did not finish within N s (the proxy is silent, is not a SOCKS5 proxy, or cannot reach <target>)`. A flag for the first answered byte would need the handshake split, which the library does not offer. `dial_says_how_far_it_got`, S63 and X62 assert the new words |
| 4 | **In the proxied run the bad-URL checks passed for the wrong reason** (low): `route()` already holds `--proxy`, a second one is refused by clap with exit 2 | the orchestrator's run: `the argument '--proxy <URL>' cannot be used multiple times`, exit 2 | F61 and N5 build their command lines without `route()` and assert the parser's own text `expected socks5://` (6.1, 6.4); the rule is in the harness notes for every test with its own `--proxy` |
| 5 | **The redaction leaked for a URL without `@`**, and an over-long `-H` with a proxy was a failed connection (low) | the orchestrator's run and 2.15: `--proxy socks5://alice:hunter2` prints the password; `-H` of 256 bytes exits 3 | The usage message never repeats the URL; `redact` is gone (5.2, P7). With `--proxy`, a `-H` above 255 bytes is `usage`, exit 2, checked in `main.rs` (5.2, P10). F61 and `proxy_urls` have the no-`@` form; F61 has 255 and 256 bytes |
| 6 | **The brief wants the Stage 0 measurements and reasons in the README**; the plan moved the table out (low) | `docs/brief.md`, Stage 0: "record the measurements and reasons in the README" | The README keeps a short table of the key Stage 0 numbers and the reasons, below the final table, with the link (section 9, C7, 2.14) |
| 7, 10 | **A `proxy` mode in `bench-rsmb.sh` and a `--socks5` flag in the `smbtap` command line served no requirement** (low, plainness) | AGENTS.md rule 5; smbclient-ng has no proxied column, and the rows would time microsocks | Both dropped. The counts of 2.3 and the tables of 2.10 go into `docs/performance.md` as measured while planning, with date, build and method (6.6, P15, section 9). `Tap::start_socks5` stays in the library for the tests; `smbtap/src/main.rs` is unchanged (5.1, 5.7). The final run is five steps, about an hour |
| 8 | **The 10 s dial limit ignored `--timeout`** (low) | `DIAL_TIMEOUT` in `src/session.rs`; a link slow enough to need a larger `--timeout` gets no longer dial | The limit is max(10 s, `--timeout`), direct and proxied: `dial_limit` (5.3, P9, D11), in the messages, the manual and the documents (C15). **At the default `--timeout 30` an unanswered dial takes 30 s** (assumption 19). The tests that expect 10 s pass `--timeout 5`: the three existing ones already do (2.15), S63, S64 and X62 are written so; T1 runs the three once |
| 9 | **S64's lower time bounds depended on the host's network** (low) | "between 9 and 12 s" for a direct dial to an address the host has no route to, "within 2 s" for a lookup | S64 asserts exit 3, kind `connect` and at most 12 s, nothing below; S63's bound for the absent address likewise (6.4, assumption 14) |
| 11 | **(a) What typed lines do under `--json` on a terminal was not said. (b) `tmp(test)` is keyed on the test's name alone**, so a direct and a proxied run at once would wipe each other's files (low) | `main.rs`: the loop over stdin lines ends at the first failure unless `-k`; `tests/common/mod.rs`: `tmp` removes and recreates `<target tmp>/<test>` | (a) 5.5, the JSON paragraph of `help all`, `docs/scripting.md` and `docs/commands.md` state the non-interactive rules for typed lines; I61 types a failing line with and without `-k` (assumption 20). (b) `tmp` puts the route into its path (6.1, T0) |

Rejected: nothing.

## Deviations during implementation

Recorded by the implementing agent, each with the evidence that made it necessary.

| # | What | Evidence and reason |
|---|---|---|
| V1 | **The dial limit stays a fixed 10 s; `dial_limit` = max(10 s, `--timeout`) (P9, D11, finding 8, 5.3, C15) is not built.** Decided by the orchestrator before implementation. `open_tcp(t, limit)` takes the limit as a parameter (the unit test passes 200 ms); `dial` passes `DIAL_TIMEOUT` = 10 s, direct and through the proxy. Messages, `help all`, `docs/commands.md`, `docs/architecture.md` and `docs/known-limitations.md` keep saying 10 s, and the last says that a proxy needing more than 10 s for its handshake and connect is not supported and `--timeout` does not extend it | With the default `--timeout 30`, max(10 s, `--timeout`) would make an unanswered connect take 30 s instead of 10 s for every user, to serve a slow-proxy case nobody measured (2.10: the handshake is local and adds nothing measurable here). `dial_says_how_far_it_got` asserts `DIAL_TIMEOUT` = 10 s instead of the `dial_limit` table; `silent_proxy` and `proxy_failures` see the 10 s (10.00 s measured), and the three serial tests that wait for a dial (`silent_link_blackhole` 10.0 s, `silent_link_mid_put`, `leftover_temp_is_reported_once`) pass unchanged. Assumption 19 and the 6.5 row "a dial at the default `--timeout`" are moot |
| V2 | T0's measurements, made before relying on the floor of 100 | A proxied run of `tests/fast` (66 tests, 14.6 s) added **149** lines `connected to fileserver.rsmb.test:445` to the proxy's log; a direct run added 0. The step with `RSMB_TEST_VIA=socks5` taken out of its line printed `FAIL: tests/fast through the proxy added 0 lines … did the tests see RSMB_TEST_VIA?`. All 66 tests passed through the proxy at the first run with their assertions unchanged (assumption 2 held): no count, host or time bound was bound to the direct route once the harness gave the host and the container |
| V3 | `Client::to(ep, extra)` takes extra flags; `Ep::args(tap)` (route and login) and `Pty::finish` (exit code and the whole transcript after the exit) were added to the harness | `silent_proxy` needs `--timeout 5` on a session without a tap; `json_never_starts_the_shell` reads the transcript of a process that ends by itself. Small helpers, no change of meaning |
| V4 | `parse_proxy` also refuses a `/` inside the host (`socks5://h/x:1`) and a port with a sign (`socks5://h:+1`, which `u16::from_str` accepts) | "anything else after the port is `usage`" (5.2) would otherwise let both through; `proxy_urls` has them |
| V5 | A timeout message below a second says `ms` (`within 200 ms`) | the unit test's 200 ms limit printed as `0 s` with `as_secs()`; in the product the limit is always 10 s |
| V6 | `cut_through_the_proxy` waits for the tap to report no live connection after the idle cut instead of the 200 ms sleep `reconnect_after_idle_cut` uses; its download is taken into a directory of its own (`dl/`) so that "no local file" is not confused with the local `big.bin` the upload needs; the upload goes to the new name `up.bin` | an observed event, not a sleep; the plan's assertions unchanged |
| V7 | `signals_through_the_proxy` uploads a local copy of `big.bin` (taken with `docker exec … cat`, as `medium_copy` does); `cut_through_the_proxy` the same | the plan names a `put` at its 100th WRITE; `big.bin` keeps the 100th WRITE at 4 % of the file, so the signal always lands mid-transfer |
| V8 | C13: `grep -rn 'allow(' src smbtap/src` finds four, not two: `Alive`'s `dead_code` (its doc comment says why: it is held, never read) and three `clippy::too_many_arguments` on `end_of_walk`, `get_tree` and `put_tree` | left as they are: each function takes the parts of one walk or transfer, and a struct for them would be an abstraction with one user (AGENTS.md rule 5) |
| V9 | `docs/known-limitations.md` completeness (section 9): items found **resolved** rather than listed | Stage 1 assumptions 1, 6, 9, 11 (`timing_*`, the silent-link and interrupt tests, measured step times, the tap against tshark); Stage 2 assumptions 1, 4, 9, 14, 15, 17, 20, 22 (`signal_mid_put_edge`, `two_thousand_small_files`, finding 8, `over_4g` in the gate, `disk_full_tiny`, `big_file`, `signals_mid_walk`, `wait_no_open`); Stage 3 assumptions 6, 7, 8, 9, 11, 14, 15, 16 and `fanout/` at the slow endpoint (X41, X42, X43, S44, the `tree/d0` row, S45, the cache unit tests, S46, V7 of that plan, `bench-rsmb.sh walk`); Stage 4 assumptions 1, 2, 6, 7, 11, 13, 14, 15, 18, 20 (V2, X63, I61, the tap's unit test and S65/X61, S62, S63/S64, the gate times) and 19 (V1); review Stage 1 finding 6 (`--json` on a terminal, fixed by P12). Everything else of those lists is in `docs/known-limitations.md` |
| V10 | **The final run's smbclient-ng download rows failed and were taken again in a second run of that section only.** The new section 7 of `bench-rsmb.sh transfers` changed smbclient-ng into the upload directory before `get large/medium.bin`, a path relative to the share root: `[error] 'NoneType' object has no attribute 'is_directory'`, three times; and the `die` inside `$(…)` ended only the subshell, so the script printed an empty cell and went on (its exit status 0). Fixed: the download runs after `cd /`, and a row whose run fails stops the script (`row`). Then only section 7 was run again (a copy of the script with sections 1 to 6 cut out, under the same exclusive lock, 374 s) | every other row of the final table is from the full runs (`bench-smbclientng.sh` 1,067 s, `bench-rsmb.sh` 505 s, `transfers` 1,616 s, `walk` 364 s, all 2026-11-18, netem 0 drops); the 50 MiB rows of both clients are from the second run, so they share one run. smbclient-ng's upload rows of the first run (66.51, 66.76, 66.77 s) and rsmb's (4.84, 4.84, 5.74 s down; 5.85, 5.45, 5.45 s up) agree with the second |
| V11 | The packet capture of 2.7, repeated at the end (6.6 step 5) | proxied by name: 2 packets on the DNS ports, none naming `rsmb.test` (background mDNS); control direct to the name: 18 packets, 8 naming `fileserver.rsmb.test`. Recorded in `docs/performance.md` |
| V12 | The README's comparison table has the plan's first seven rows and the proxy row; the 1.1 GiB row and the Tab row are in `docs/performance.md` | as section 9 says |

## After the stages: `shares` access column

**The request.** The user could not see in `shares` which shares they have access to; smbclient-ng
shows that (`shares -R`). Per-directory access was to be built only if equally trivial; the
orchestrator decided against it (a listing carries no rights; it would cost one open per
directory).

**Measured first** (throwaway probe under `spikes/smb2/target/scratch-shares/`, raw requests on
`smb2` 0.43.0, 2026-11-18). The crate parses `MaximalAccess` (`TreeConnectResponse::maximal_access`)
but `Tree::connect` drops it, and a `Tree` cannot be built outside the crate (`#[non_exhaustive]`,
a private session pin), so the access comes from a raw TREE_CONNECT. Samba 4.23.8:

| Share | `tester` | anonymous |
|---|---|---|
| `share` (`read only = no`, root directory owned by root, 0755) | 0x001f01ff | ACCESS_DENIED |
| `readonly` (`read only = yes`, same directory) | 0x001f00a9 | ACCESS_DENIED |
| `guest` (`guest ok`, read only) | 0x001f00a9 | 0x001f00a9 |
| `encrypted` (`smb encrypt = required`) | 0x001f01ff, ENCRYPT_DATA | ACCESS_DENIED |
| `tiny` | 0x001f01ff | ACCESS_DENIED |
| `IPC$` | 0x001f00a9 | 0x001f00a9 |
| a name that does not exist | BAD_NETWORK_NAME | — |
| `edge001`…`edge300` (read only) | 0x001f00a9 | ACCESS_DENIED |

0x001f00a9 has READ_DATA, READ_EA, EXECUTE, READ_ATTRIBUTES and DELETE, READ_CONTROL, WRITE_DAC,
WRITE_OWNER, SYNCHRONIZE, and none of WRITE_DATA, APPEND_DATA, WRITE_EA, WRITE_ATTRIBUTES: the
`read only = yes` share is truthfully read-only. `share` says full access although its root
directory refuses `tester` new files: the value is share-level, not the rights of a directory.
So the column is built, read as FILE_READ_DATA and write as FILE_WRITE_DATA or FILE_APPEND_DATA,
and the documents say what it does not tell.

- **TREE_DISCONNECT in the same compound.** A related TREE_DISCONNECT after each TREE_CONNECT
  closes the tree just made; a chain of pairs in one compound is answered pair by pair (a refused
  or unknown share in the middle changes nothing for the others). 16 and 64 pairs worked; 256
  pairs (512 requests) were answered and then the connection was lost: hence at most 16 shares
  per compound.
- **Encryption.** On a connection that does not encrypt, Samba refuses the TREE_DISCONNECT of
  `encrypted` (ACCESS_DENIED) and encrypts its answer to the whole compound. The tree stays
  connected until the connection ends (`smbstatus -S`: one more per `shares`; none once the
  connection encrypts after `use encrypted`). rsmb decrypts the answer; the tap could not, and
  never settled: it now attributes an encrypted answer while no encrypted request waits to the
  oldest plain frame still waiting (`docs/measuring.md`).
- **Nagle.** In the probe on the crate's own transport (no `TCP_NODELAY`) six or more concurrent
  compounds took two round trips at the slow endpoint; rsmb sets `TCP_NODELAY`, and through the
  tap six took one.

**Cost** (product, requests / flights through `smbtap`, time direct, median of 5):

| Endpoint | Before, first / again | After, first / again |
|---|---|---|
| fast | 7 / 7 1.1 ms, 6 / 6 0.7 ms | 19 / 8 2.8 ms, 18 / 7 2.3 ms |
| slow (100 ms each way) | 7 / 7 1,403 ms, 6 / 6 1,202 ms | 19 / 8 1,606 ms, 18 / 7 1,404 ms |
| edge (306 shares, 16 credits) | 13 / 13 6.4 ms, 12 / 12 5.2 ms | 625 / 52 65 ms, 624 / 51 57 ms |

Through the proxy (ports 1080 and 1081) the same 19 / 8 and 18 / 7.

**Decisions.**

- **Disconnect, do not keep.** Keeping the probed trees would need them in the tree map, which
  holds crate `Tree`s that only `Tree::connect` makes, and that call hides the access; keeping them
  would also hold 300 trees open on edge. The related TREE_DISCONNECT costs one request per share
  and no round trip. A later `use` still costs its 1 TREE_CONNECT.
- **No flag.** One round trip more on the normal server at the slow endpoint (200 ms) and 60 ms
  on edge are inside the bounds set (about one round trip; not seconds), so the column is simply
  on.
- **Form.** Text: a 4-column field after the type, `rw`, `r-`, `-w`, `--` (refused), `?` (no
  answer). JSON: the `share` object gains `access`: `"rw"`, `"r"`, `"w"`, `"none"` or `null`;
  no field renamed or removed. ACCESS_DENIED on the TREE_CONNECT is `none`; any other failure of
  one share is `null` and the share is still listed; only a lost connection fails the command.
- **Concurrency.** Up to 16 shares per compound, as many compounds in flight as the credits at the
  start pay for (`credits / (2 × per compound)`, at least 1): 1 compound on the test server, 39 one
  after another on edge (8 shares each). Tab after `use` takes the share list alone and never
  probes.

**Tests.** `shares_fast` (direct and through the proxy): the access of all six shares as `tester`
and as anonymous, the text column, 19 / 8 then 18 / 7, 7 then 6 TREE_CONNECT and 6
TREE_DISCONNECT, `use readonly` afterwards still 1 / 1. `shares_edge`: 306 values, 307 / 306 tree
requests, one connection, within 5 s; on an anonymous session (no encrypted answer) never a credit
not granted, peak 16 outstanding. `counts_add_up` and `one_connection_*` run `shares` twice in
their scripts. Unit tests `share_access_names` and the tap's `encrypted_answer_to_a_plain_frame`.
