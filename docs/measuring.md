# Measuring: requests, flights, connections, time

The design goal is fewer round trips, so the project measures round trips directly instead of
inferring them from seconds. Three independent counters exist:

1. **`smbtap`** sits between client and server and counts SMB2 requests, responses and flights.
2. **The server-side connection count** (`testenv/ctl.sh conns <service>`, an iptables counter
   inside each Samba container; see `docs/test-environment.md`) counts TCP connections without
   knowing anything about the tap. A client that opened a connection around the tap would show up
   here and not there.
3. **tshark** on the `rsmb-pub` bridge, used once per change to the tap to prove the tap counts
   what is on the wire (`testenv/validate-smbtap.sh`, part of the full `./test.sh`).

## smbtap

A TCP forwarder (`smbtap/`, library plus thin CLI, no dependencies outside the Rust standard
library). The client connects to the tap's address instead of the server's; the tap opens one
upstream connection per client connection and copies bytes both ways unchanged, with
`TCP_NODELAY` on both sockets. A copy of each direction is cut into frames (4-byte header: a zero
byte and a 24-bit big-endian length) and each frame is parsed:

- `FE 'SMB'`: an SMB2 header chain. Each header of a compound chain is followed through its
  `NextCommand` offset; command, message id, credit charge, credits requested or granted, status
  and flags are read from each.
- `FF 'SMB'` (SMB1), `FD 'SMB'` (encrypted, transform header), `FC 'SMB'` (compressed): counted per
  kind as opaque frames, never parsed.
- A frame that starts like SMB2 but whose chain does not parse is counted as `malformed`.

Nothing the client reports is used. A frame is recorded when its last byte has been read and
before that byte is forwarded, so the order of records follows cause and effect: a client frame
sent in reaction to a server frame is always recorded after it.

### Definitions

These are also the doc comment at the top of `smbtap/src/lib.rs`.

- **request**: one SMB2 header sent by the client. Each element of a compound chain counts once.
- **response**: one SMB2 header sent by the server, interim responses included.
- **interim response**: `STATUS_PENDING` (0x00000103) with the async flag set; the final response
  for the same message id follows later. A large read on a cold cache produces these; issue #185 of
  smb-rs (credits from interims dropped) can only be tested on a run that has some.
- **notification**: a server message with message id 0xFFFFFFFFFFFFFFFF (oplock or lease break).
  Not a response to anything.
- **flight**: a new flight starts when the client sends a frame after at least one server frame
  has arrived on that connection since the client's previous send; the first send on a connection
  starts flight 1. Requests sent back to back without waiting are one flight; a request that
  needed the previous answer starts a new one. At high latency wall time follows the number of
  flights, not of requests: a `cd` that sends CREATE and then CLOSE after the CREATE response is
  two requests in two flights; the same pair as one compound is two requests in one flight. Any
  server frame (interim, notification) counts as "a server frame has arrived". Flights are counted
  per connection; `Stats::flights` is the sum over connections.
- **outstanding**: requests without a final response yet, keyed by (connection, message id). A
  CANCEL is not added (it gets no response of its own). When a connection closes, what it left
  outstanding is forgotten.
- **opaque requests and outstanding**: each opaque client frame counts as one outstanding until
  the next opaque server frame, or the next SMB2 response that matches no outstanding request, on
  the same connection. This is exact for the SMB1 NEGOTIATE (answered by an SMB2 NEGOTIATE response
  with message id 0) and approximate on encrypted connections, where an interim response cannot
  be told from a final one: there "outstanding" can reach zero early. Counts of opaque frames are
  exact.
- **an encrypted answer to plain requests**: an opaque server frame that arrives while no opaque
  request is outstanding answers the oldest plain request frame on that connection that still
  waits, and all of that frame's requests stop being outstanding. Samba does this to the
  unencrypted compound of `shares` that holds a TREE_CONNECT to a share requiring encryption
  (`encrypted`): it encrypts the whole answer. The responses and the credits inside are not seen,
  so `responses` and the credits granted are short for that frame (`credit_balance` cannot be
  used across it; `shares_edge` checks credits on an anonymous session, which `encrypted`
  refuses in plain text). Unit test: `encrypted_answer_to_a_plain_frame`. **No tshark
  cross-check exists for this rule** (`validate-smbtap.sh` runs one plain smbclient session; a
  capture cannot decrypt the answer either, so there is nothing to compare it with): it is checked
  by the synthetic unit test and by Samba's behaviour as seen through the product (`shares`
  settles, 19 requests and 8 flights). It cannot change rsmb's counts: the crate encrypts
  connection-wide, so after a session or tree that requires encryption every frame rsmb sends is
  encrypted, and rsmb never has a plain and an encrypted request outstanding on one connection at
  the same time, except in the one `shares` compound above, where the rule is what lets the tap
  settle. A client that does have both outstanding at once (a plain request and an encrypted one
  on one connection) would be miscounted: the tap pairs an opaque server frame with an
  outstanding opaque request first, so an encrypted answer to the plain frame would be taken for
  the answer to the encrypted request, and both frames' `outstanding` and per-frame counts would
  be off.
- **peak outstanding**: the most requests outstanding at once, sampled after every client frame.
  Used to check a concurrency bound ("never more than 16 in flight").

### Library

```rust
let tap = smbtap::Tap::start("10.213.45.30:445")?;   // listens on 127.0.0.1, port chosen by the system
// point the client at tap.addr()
tap.settle(Duration::from_millis(300), Duration::from_secs(30)); // after each command
let s = tap.stats();          // Stats: counters, per_command, request_log, response_log
tap.reset();                  // zero counters and logs; outstanding requests stay tracked
tap.drop_connections();       // cut every live connection now (returns how many)
```

| Item | Meaning |
|---|---|
| `Tap::start(upstream)` / `Tap::start_on(listen, upstream)` | start forwarding; dropping the `Tap` stops it and cuts its connections |
| `Tap::start_socks5(proxy)` | the same with a SOCKS5 proxy as upstream (below); library only, the CLI has no such flag |
| `addr()` | where clients connect |
| `stats() -> Stats` | snapshot: `connections`, `upstream_failures`, `client_frames`, `server_frames`, `flights`, `requests`, `responses`, `interim`, `notifications`, `opaque_requests`, `opaque_responses`, `malformed`, `outstanding` (now), `peak_outstanding`, `per_command` (request count per command name; opaque kinds appear as `SMB1`, `ENCRYPTED`, `COMPRESSED`, `UNKNOWN`), `request_log`, `response_log` |
| `Request` | `conn`, `at` (since tap start), `flight` (on its connection), `command`, `message_id`, `chain_index`, `chain_len`, `credit_charge`, `credits_requested` |
| `Response` | `conn`, `at`, `command`, `message_id`, `status`, `credits_granted`, `interim` |
| `outstanding()` | is anything outstanding now (0 = no) |
| `live_connections()` | connections currently open through the tap |
| `drop_connections()` | shuts down both sockets of every live connection: the client sees the connection close (FIN, not RST). A connection still waiting for its upstream connect is not affected. |
| `settle(quiet, timeout) -> bool` | waits until nothing is outstanding and no client frame has arrived for `quiet`; false if `timeout` passes first |
| `Stats::to_json(with_log)` | one JSON line, as the CLI prints it |

The logs grow with every request; a run that reads 4.3 GiB in 64 KiB chunks keeps about 70,000
records, which is fine for a test process.

### In front of a SOCKS5 proxy

`Tap::start_socks5(proxy)` puts the tap where the proxied path begins: rsmb → tap → proxy →
private server (rsmb is given `--proxy socks5://127.0.0.1:<tap port> -H fileserver.rsmb.test`).
The private server cannot be reached without the proxy, so a session the tap counted went through
it by construction. For each connection the tap connects to the proxy, puts both sockets into its
list of live connections (so `drop_connections` also reaches a connection still in its
handshake), then relays the SOCKS5 handshake (`socks5_preamble`, RFC 1928 and 1929: greeting,
method, user and password if the proxy chose that method, request, reply) and only then starts the
two pumps. The handshake is **not counted**: the engine sees no byte of it, so "the first send on
a connection starts flight 1" holds for the NEGOTIATE, and the counts are those of a direct
session. A connection whose handshake the proxy refused counts in `connections` and has no
frames.

**One write per message.** The tap reads each handshake message whole (its length follows from
its own fields) and writes it with one `write_all`. microsocks parses one `recv` per message: the
first prototype relayed the CONNECT request in three writes and microsocks answered
`General SOCKS server failure` (`docs/plans/stage-4.md` 2.3). The unit test
`socks5_preamble_is_relayed_and_not_counted` has a fake proxy that reads each message with one
`read` and fails if one arrives in pieces.

The cross-check of this mode is the table of `docs/performance.md`, "Through a SOCKS5 proxy": 33
commands counted through the tap direct and through both proxy ports, the same requests on every
row and the same flights wherever requests do not overlap. `validate-smbtap.sh` is unchanged: the
counting code is the same.

**Kerberos (Stage 5).** A TGT-only Kerberos login opens a second connection, to the KDC, port 88.
Through a tap in SOCKS5 mode that connection would pass the tap as a second connection of frames
it cannot read (Kerberos, not SMB), so such runs are not tapped: a tapped session through the
proxy holds the service ticket in its cache, and no KDC is asked. Direct, the tap stands in front
of the server only (`-H 127.0.0.1`, service name `cifs/127.0.0.1`) and the KDC's connection goes
beside it: a TGT-only start-up is still 2 requests in 2 flights on 1 connection in the tap. KDC
requests are counted in the KDC's own log instead (`docs/test-environment.md`, "The Kerberos
server").

### CLI

```
cargo run -q -p smbtap -- 10.213.45.30:445 [--listen 127.0.0.1:4455]
{"listen":"127.0.0.1:41267"}        <- first line: where to point the client
```

Then one command per line on stdin, each answered with one JSON line on stdout: `stats`, `log`
(stats plus every request and response), `reset`, `drop`, `settle <quiet-ms> [<max-ms>]`
(answers `{"settled":true}` or `false`), `quit`. End of input works like `quit`; both print the
final stats. From bash, run it as a coprocess:

```bash
coproc TAP { exec target/debug/smbtap 10.213.45.10:445; }
read -r first <&"${TAP[0]}"; port=${first##*:}; port=${port%\"\}}
smbclient //127.0.0.1/share -p "$port" -U 'TESTDOM\tester%testpass' -c 'cd many'
echo "settle 300" >&"${TAP[1]}"; read -r _ <&"${TAP[0]}"
echo stats >&"${TAP[1]}"; read -r stats <&"${TAP[0]}"; echo "$stats"
echo drop >&"${TAP[1]}"; read -r _ <&"${TAP[0]}"     # cut the client's connection
```

`testenv/validate-smbtap.sh` is a complete example.

## How per-command counts are taken

- Commands are fed to the client one at a time. After each one, the test calls `settle(quiet, …)`
  and only then reads and resets the tap. A request the client sends in the background after the
  prompt returned (a CLOSE after `cd`, say) is therefore charged to the command that caused it, not
  to the next one. Per-command counts must add up to the session total.
- Requests and flights are counted through the tap at both the fast and the slow endpoint: a
  library can behave differently when answers are slow (more requests in flight, timeouts).
- TCP connections are counted twice: by the tap (`connections`) and on the server
  (`ctl.sh conns`), and also against `samba-edge`, which advertises multichannel.

## How time is taken

- Wall times never go through the tap: the tap adds a loopback hop and a user-space copy per
  read. Timed runs connect to 10.213.45.10 (fast) or 10.213.45.30 (slow) directly, and every timing
  row carries the TCP connect time measured just before it (as `check.sh` measures it).
- One method for both clients: (time for 10 commands − time for 0 commands) ÷ 10, median of 5
  runs. The subtraction removes connect and login.
- rsmb is also timed with 1 s pauses between commands, because TCP restarts slow after idle and a
  large listing then costs extra round trips that back-to-back runs hide.
- Bulk throughput at the slow endpoint is reported next to the raw TCP ceiling through the same
  router (iperf3 to 10.213.45.30:5201; about 165 Mbit/s steady state, see
  `docs/test-environment.md`), so a TCP window limit is not blamed on the client.
- Large-read runs evict the server's page cache first (`ctl.sh evict samba /srv/share/large/…`).
  The tap's `interim` count shows whether the server went async; zero interims means a stall test
  for interim-credit handling was inconclusive, not passed. When two clients are compared, their
  order alternates so neither always reads from a warm cache.
- After a slow-endpoint run, netem on `wan` must show no drops. `testenv/ctl.sh drops` reads the count (and fails if `wan` cannot be asked); `check.sh` asserts 0, the benchmark and both spike `run.sh` print it before and after the run (`spikes/smb2/run.sh` exits non-zero if it changed, `spikes/smb-rs/run.sh` prints a warning).

## The smbclient-ng baseline: `testenv/bench-smbclientng.sh`

rsmb's side of the comparison is `testenv/bench-rsmb.sh` (same rows and method, plus fresh and
paused columns and a wan-bad row); it reads the smbclient-ng columns from
`testenv/baseline-smbclientng.txt`, the output of the last full run of this script. Renew that
file with `./testenv/bench-smbclientng.sh | tee testenv/baseline-smbclientng.txt`. The combined
table is in `docs/performance.md`.

```
./testenv/bench-smbclientng.sh          # 5 runs per row, about 18 minutes
./testenv/bench-smbclientng.sh quick    # 1 run per row, no huge/ row, about 4 minutes
```

Unattended, needs the environment up. It holds a shared `flock` on `/tmp/rsmb-env.lock` for the
whole run, so anyone who needs the environment exclusively (`check.sh`, pause, blackhole,
`conns-reset`) waits, and it waits for them (who takes what: `docs/test-environment.md`, "The
lock"). It changes nothing in the environment. Exits non-zero if any run fails its
check or netem on `wan` dropped a packet during the run.

How smbclient-ng (3.1.0) is driven: one process per session, `-N` (no interactive shell after the
commands) and one `-C` per line: `use share`, an optional setup line, the measured command N times,
and a final empty line. The empty line prints one more prompt, which shows the state after the last
measured command. The run with 0 commands has the same lines minus the measured ones, so connect,
login, `use share`, the setup and that last prompt cancel out in the difference.

Rows, each at the fast (10.213.45.10) and the slow (10.213.45.30) endpoint:

| Row | Setup | Measured | N |
|---|---|---|---|
| `cd` | none | `cd /tree/d0` (absolute, so every repetition does the full check) | 10 |
| `ls` small | `cd /tree/d0` | `ls` (5 entries plus `.` and `..`) | 10 |
| `ls many/` | `cd /many` | `ls` (10,000 entries) | 10 |
| `ls huge/` | `cd /huge` | `ls` (100,000 entries) | 1 |

`huge/` uses N = 1 because one listing takes about 8 s fast and 37 s slow; ten would make the full
run about 35 minutes longer, and at that size the subtraction does not need ten repetitions to
drown the noise of process start-up. `quick` leaves the row out.

Per row: the TCP connect time (median of 5 after a warm-up, as `check.sh` measures it), then
REPS pairs of (0 commands, N commands) timed directly against the endpoint, each pair giving
(t_N − t_0) ÷ N; the median of those is the time per command. Then one 0-command and one N-command
session through `smbtap` give connections, requests and flights per command by the same
difference. The counts are not repeated: they were identical in every run so far.

**Success is judged from the output**, because smbclient-ng exits 0 even when the login fails.
Every session's output must contain `[+] Successfully authenticated to '<host>' as
'TESTDOM\tester'!`; contain no `[error]` line, no `Unknown command`, no `Traceback` and no
disconnected prompt (`[x]`); show exactly one connected prompt (`[v][...]>`) per `-C` line; hold
exactly N × (entries + 2) listing lines (`d-------`-style attribute column); and end on a prompt
showing the expected directory (`[v][\\<host>\share\tree\d0\]>`). A wrong password ("No active
session" prompts) and a `cd` to a missing directory (`[error] Remote directory ... does not
exist`) were both checked to stop the script.

**Requests include encrypted frames.** impacket enables encryption on every SMB 3 session with a
user name when the server supports it (`smb3.py`, session setup), so after SESSION_SETUP every frame
is a transform frame that the tap counts as `ENCRYPTED` and cannot name. The table's requests are
`requests + opaque_requests`, one per frame (impacket never sends compounds). Which commands they
are comes from reading the source (below), not from the wire; the totals agree with it exactly.

**Only the tap counts connections here.** The server-side count (`ctl.sh conns`) needs a
`conns-reset` and an environment nobody else is using; the baseline runs while others share it.

### What one command costs in smbclient-ng, and why

Every scripted line first prints its prompt, and building the prompt calls `ping_smb_session()`:
a new TCP connection to port 445 that is closed at once (`is_port_open`), then an SMB ECHO on the
session. `cd` is decorated with `active_smb_connection_needed`, which pings a second time. Then
`cd` checks the target with `path_isdir`: impacket `listPath` on the parent with the pattern
`d0*` (CREATE of `tree`, QUERY_DIRECTORY until `STATUS_NO_MORE_FILES`, CLOSE; the tree from
`use share` is reused, so no TREE_CONNECT). `ls` has no second ping and lists `.` with `listPath`,
64 KiB per QUERY_DIRECTORY (`maxBufferSize=65535`). Nothing is sent in parallel: every request
waits for the previous answer, so requests = flights.

| Command | TCP connections | Requests = flights |
|---|---|---|
| `cd /tree/d0` | 2 (two pings) | 6: 2 ECHO, CREATE, 2 QUERY_DIRECTORY, CLOSE |
| `ls`, 5 entries | 1 | 5: ECHO, CREATE, 2 QUERY_DIRECTORY, CLOSE |
| `ls many/` | 1 | 17: ECHO, CREATE, 14 QUERY_DIRECTORY, CLOSE |
| `ls huge/` | 1 | 139: ECHO, CREATE, 136 QUERY_DIRECTORY, CLOSE |

At 200 ms round trip: `cd` = 2 connects + 6 flights = 8 round trips = 1.6 s; `ls` small = 6 round
trips = 1.2 s; the large listings add Python time (about 0.8 s per 10,000 entries, the fast-endpoint
figure) to their round trips.

### Results

2026-11-18 (renewed for the final comparison, on the image rebuilt for `links/`), smbclient-ng
3.1.0 (impacket 0.13.1), Samba 4.23.8, full run (5 per row), 1067 s; this is
`testenv/baseline-smbclientng.txt`:

```
endpoint command                          N connect_ms   ms_per_cmd  conns/cmd   reqs/cmd flights/cmd
fast   cd /tree/d0                     10       0.02          4.1        2.0        6.0        6.0
fast   ls small (tree/d0, 5 entries)   10       0.02          3.9        1.0        5.0        5.0
fast   ls many/ (10,000 entries)       10       0.02        823.7        1.0       17.0       17.0
fast   ls huge/ (100,000 entries)       1       0.02       8308.2        1.0      139.0      139.0
slow   cd /tree/d0                     10     200.13       1608.8        2.0        6.0        6.0
slow   ls small (tree/d0, 5 entries)   10     200.14       1207.8        1.0        5.0        5.0
slow   ls many/ (10,000 entries)       10     200.18       4558.7        1.0       17.0       17.0
slow   ls huge/ (100,000 entries)       1     200.10      37561.1        1.0      139.0      139.0

smbclient-ng v3.1.0, Samba 4.23.8, 5 run(s) per row, 2026-11-18, 1067 s
netem drops on wan during the run: 0
```

The first baseline (2026-11-17, before the image was rebuilt) gave the same counts on every row
and times within 1 % at the slow endpoint (1,610.1, 1,207.9, 4,535.4 and 37,320.2 ms).
A quick run the same day gave the same counts on every row and times within 1 % at the slow
endpoint. The fast-endpoint `cd` and small `ls` (1–5 ms) are below the noise of starting a Python
process (about 1 s, varying by several ms), so only their order of magnitude means anything; the
quick run gave 1.0 ms for the same `cd`.

### Transfers in smbclient-ng

`bench-rsmb.sh transfers` ends with `medium.bin` (50 MiB) each way at the slow endpoint for both
clients, by one method: one process per run, the time of a run with the command minus the time of
the same run without it, three runs each. smbclient-ng runs in the bench's local directory: the
download is `cd /` and `get large/medium.bin` (it lands in `./large/`), the upload `cd
upload/<bench dir>` and `put medium.bin`; a run counts only with its login line, no `[error]`,
`[!]` or `[x]` line and its prompts, and the result's sha256 is checked on the receiving side.
rsmb runs `get /large/medium.bin` and `put medium.bin` the same way. Results:
`docs/performance.md`.

### `tree` and `find` in smbclient-ng (Stage 3)

Measured while planning Stage 3 (2026-11-18, `docs/plans/stage-3.md` 2.9), through the tap at the
fast endpoint, one process per run, the method above (a run with `use share` and an empty line
alone: 19 flights): `tree tree/d0/d0` (21 directories) 86 flights above the empty run, `tree
tree/d0` (85 directories) 342, `find tree/d0/d0` 86: **4 requests per directory, one after
another** (peak outstanding 1): its `tree` and `find` are the same walk, a `listPath` per
directory. At the slow endpoint, direct, `tree tree/d0` took 69.3 s above the empty run. On all
of `tree/` it prints `forbidden/`, then an `[error]` line with `STATUS_ACCESS_DENIED`, and goes
on; the bench's rules treat an `[error]` line as a failed run, so its walk row is `tree/d0`, the
largest subtree without a denied directory.

**How the walk row of `bench-rsmb.sh walk` is taken:** for both clients one process per run,
(time of `use share` + `tree <dir>` + exit) − (time of `use share` + exit), direct to the slow
endpoint, rsmb 3 runs and smbclient-ng 1; the counts in a further run of each through the tap,
the same difference. smbclient-ng's run counts only with its login line, no `[error]`, the
expected prompts and 85 `file.txt` lines; rsmb's only with exit 0 and 169 `entry` objects. The
other walk rows are rsmb's own `walk_summary.seconds`. Results: `docs/performance.md`.

## Validation against tshark

`testenv/validate-smbtap.sh [upstream]` runs one `smbclient` session through the tap (listings of
`/`, `many/`, `cjk/`, `tree/d0/d1`, `special/`; two downloads including the 50 MiB `medium.bin`;
`allinfo`; a denied listing; a `cd` to a missing directory; a wildcard listing in `huge/`) while
`dumpcap` captures the tap's upstream leg on `rsmb-pub`. It then counts from the capture with
tshark (requests per command, responses, interim responses, flights computed with the same rule
from packet order, TCP SYNs) and prints both columns. It exits non-zero on any difference, when the
`smbclient` session fails (a run with no SMB traffic would otherwise print 0 = 0 everywhere; with
the proxy address `10.213.45.20` as upstream it now fails), and when fewer than 20 requests were
captured (a real session sends about 86). The background `dumpcap` is killed on any exit.

Results on 2026-11-17 (Samba 4.23.8, smbclient 4.25.1), fast endpoint; three more runs at the fast
endpoint and one at the slow endpoint (10.213.45.30) also matched on every row:

```
                       smbtap   tshark
CLOSE                      22       22
CREATE                     24       24
IOCTL                       2        2
NEGOTIATE                   1        1
QUERY_DIRECTORY            12       12
QUERY_INFO                 11       11
READ                        8        8
SESSION_SETUP               2        2
TREE_CONNECT                2        2
TREE_DISCONNECT             2        2
requests (total)           86       86
responses                  92       92
interim responses           6        6
flights                    85       85
TCP connections             1        1
```

(The number of interim responses varies between runs, 5 to 7, and the flight count by one, with
how the server's reads race; both columns always moved together.)

One lesson from getting there: the first comparison differed (tshark 3 responses short). The
capture, not the tap, was wrong: at bridge speed `dumpcap` with its default 2 MiB buffer missed
segments during the 50 MiB read (tshark showed `TCP Previous segment not captured`), so tshark
could not reassemble three READ responses. With `dumpcap -B 1024` the capture is complete and the
counts agree. A disagreement between two counters is explained before either is believed.

## Known limits of the tap

- On encrypted connections, "outstanding" is approximate (see definitions). Opaque frame counts
  are exact.
- An encrypted answer to a plain compound is attributed to the oldest plain frame still waiting;
  its responses and granted credits are not counted (see definitions); this attribution has no
  tshark cross-check (see definitions for why it cannot affect rsmb's counts).
- `drop_connections()` closes with FIN. A client sees an orderly close, not a reset; the blackhole
  port and `ctl.sh pause` cover silent failures.
- A frame is recorded only once complete, so an 8 MiB READ response is buffered in the tap until
  its last byte arrives (bytes are still forwarded as they come).
- The tap is never used for timing (see above).
