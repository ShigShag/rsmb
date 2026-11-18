# Stage 1 plan: connection and navigation

> Written on 2026-11-18 by a planning agent that read the repository and nothing else, before any
> Stage 1 code exists. It follows [`master-plan.md`](master-plan.md) (sections 1–7 and 11–14); where
> it departs from it, section 4 says where and why. It is written for one implementing agent who
> was not present: everything needed to build Stage 1 in one pass is here or in a document this
> one names. **Revised on 2026-11-18 after the adversary's review:** section 11 lists the twelve
> findings, the evidence for each and what changed; the text above it is the revised plan.
>
> Words are used as in `docs/measuring.md` (request, flight, outstanding) and in the master plan
> (fast, slow, edge, wan-bad).

## 1. Scope, what is not in it, exit criteria

**In scope** (brief, Stage 1; master plan section 7): connect with host, port, user, password and
domain, plus guest and anonymous; an interactive shell with history and tab completion for
commands, share names and remote paths; a non-interactive mode with meaningful exit codes; the
commands `shares`, `use`, `cd`, `pwd`, `ls`, `refresh`, `help` (`help`, `help <cmd>`, `help all`),
`exit`; `rsmb --help-all`; `--json`; one TCP connection, one tree connect per share, no liveness
probe, reconnect when a request fails, `cd` by one compound open/close, cached listings reused by
completion, a manual refresh; the dialer with SOCKS5 and `--proxy` (smoke test only).

**Not in Stage 1:** transfers and file operations, `cat`, the local commands, `tree`, `find`, the
walk and its concurrency limit, the full SOCKS5 test matrix, any command that changes the server
(so cache invalidation by commands is Stage 2; the cache here has `get`, `put` and `clear`),
colours, command aliases, listing a single file with `ls <file>`, a debug or log flag, Kerberos
and NT-hash logins, a compounded share list (S1-9), a run-time fallback for servers that refuse
the listing compound (section 10, assumption 3).

**Exit criteria** (master plan section 7): every row of section 6.4 passes in `./test.sh`; the
full run is within 4 minutes (section 6.2 says how that is counted); the listing shape is
confirmed through the product and recorded with its numbers in `docs/library-decision.md`;
`docs/performance.md` holds the navigation table from `testenv/bench-rsmb.sh` beside the baseline;
the documents of section 8 exist and match the code; the review's confirmed findings are fixed and
their tests are in the suite.

## 2. What was measured while planning

All on 2026-11-18, Samba 4.23.8, `smb2` 0.43.0, read-only, under the shared lock
(`flock -s /tmp/rsmb-env.lock`), with throwaway programs under `spikes/smb2/target/` (not
committed; the target directory is ignored). Counts through `smbtap`, times direct to the endpoint.
The implementing agent repeats the listing rows through the product (task T4) and records them in
`docs/library-decision.md`; the numbers here are what the design was chosen on.

### 2.1 Listing shapes

Shapes, each built from `execute_compound`, class 37, pattern `*`:

- **A**: `[CREATE, QD(8 MiB), QD(64 KiB)]`, then single queries until `NO_MORE_FILES`, then CLOSE.
- **B2**: `[CREATE, QD(8 MiB), QD(64 KiB), CLOSE]`; if the last query returned entries, list again
  with the handle kept open.
- **BL** (the master plan's B read literally): `[CREATE, QD(8 MiB), QD(8 MiB), CLOSE]`, same
  fallback.
- **P** (chosen, section 5.7): `[CREATE, QD(b) × n, QD(64 KiB), CLOSE]` with `b` and `n` from the
  negotiated transact size and the credits at hand (Samba: b = 8 MiB − 64 KiB, n = 2; edge:
  b = 64 KiB, n = 13); if the last query returned entries, list again as `[CREATE, QD(b) × n]`,
  `[QD(b) × n]` … until a `NO_MORE_FILES`, then CLOSE.

Every shape returned exactly the server's set of names (byte strings from
`docker exec … ls -A`, no duplicates) for `sizes/`, `special/`, `cjk/`, `longnames/`, `many/` and
`huge/`, on the shares `share` and `encrypted`, at the fast endpoint, the slow endpoint and edge:
144 rows for A, B2, BL and a first form of P (n fixed at 2), 30 more for P as planned, none
differing.

Requests / flights, share `share` (on `encrypted` the same number of flights, each one opaque
frame):

| Directory | Through the crate (Stage 0) | A | B2 | BL | **P** |
|---|---|---|---|---|---|
| `sizes/` (11), slow | 4 / 4 | 4 / 2 | 4 / 1 | 4 / 1 | **5 / 1** |
| `cjk/` (1,010), slow | — | 4 / 2 | 4 / 1 | 4 / 1 | **5 / 1** |
| `longnames/` (2,000), slow | 4 / 4 | 4 / 2 | 4 / 1 | 4 / 1 | **5 / 1** |
| `many/` (10,000), slow | 4 / 4 | 4 / 2 | 4 / 1 | 4 / 1 | **5 / 1** |
| `huge/` (100,000), slow | 5 / 5 | 6 / 4 | 9 / 5 | 9 / 5 | **5 / 1** |
| `cjk/`, edge | — | 6 / 4 | 10 / 6 | 10 / 6 | **16 / 1** |
| `many/`, edge | 21 flights with a 64 KiB buffer (on `samba`) | 22 / 20 | 26 / 22 | 26 / 22 | **44 / 4** |
| `huge/`, edge | 174 flights with a 64 KiB buffer (on `samba`) | 187 / 185 | 191 / 187 | 191 / 187 | **213 / 17** |

Time at the slow endpoint, direct, ms: warm = median of 3 further listings on the connection,
fresh = the first listing on a new connection (TCP slow start, as in Stage 0):

| Directory | Crate, warm (Stage 0) | A warm | B2 warm | **P warm** | **P fresh** | smbclient-ng |
|---|---|---|---|---|---|---|
| `sizes/` | 803.0 | 402.0 | 201.4 | **201.2** | 210.8 | 1207.9 (`tree/d0`) |
| `cjk/` | — | 412.0 | 211.3 | **211.7** | 812.6 | — |
| `longnames/` | 829.4 | 428.8 | 227.6 | **228.0** | 1430.6 | — |
| `many/` | 901.1 | 499.2 | 298.8 | **298.1** | 1500.7 | 4535.4 |
| `huge/` | 2337.0 | 2122.1 | 3090.9 | **1712.6** | 3133.6 | 37320.2 |

P needs 257 credits on Samba (1 + 127 + 127 + 1 + 1); a connection has 513 after login and after
the tree connect, and 2045 after the first listing. On edge P uses all 16 credits in one compound
(1 + 13 + 1 + 1) and did not stall. Peak memory of P for `huge/` in a fresh process, with the
crate's entry type and the reply sliced without a copy: +51.1 to +51.2 MB (56 MB in all); `many/`
+4.3 MB.

The second phase of P with multi-credit queries was forced on Samba by passing a smaller plan:
with b = 2 MiB, n = 2, `huge/` came back equal on both shares at both endpoints (15 requests in
6 flights); with b = 4 MiB, n = 1, on both shares at the fast endpoint (10 requests in 6 flights).

### 2.2 Other facts

| Fact | Measured | Used in |
|---|---|---|
| Listing errors arrive on the CREATE of the compound | `nope` → `OBJECT_NAME_NOT_FOUND`, `sizes/1.bin` → `NOT_A_DIRECTORY`, `forbidden` and `traverse-only` → `ACCESS_DENIED` | 5.7 |
| File ids through `smb2`, class 37 | `loops/self` has the id of `loops/.`; `loops/up` has the id of the share root's `.`; the share root's `..` has id 0; `loops/dangling` is not listed at all | 9 (settles S2-6 early) |
| A connection cut by the tap while idle | the next raw call returns `Error::Disconnected` (kind `ConnectionLost`) after 1 µs, `is_disconnected()` is true, so the receiver task had seen the close before anything was sent; a second call the same; a new connection through the same tap works | 5.10 |
| A connection cut while a listing is in flight (slow) | `Disconnected` at once | 5.10 |
| Share list with `IPC$` kept connected | `samba`: 7 requests in 7 flights the first time (TREE_CONNECT, CREATE, 2 WRITE, 2 READ, CLOSE), 6 after; edge: 306 shares, stub 25,736 bytes in 7 fragments, 13 requests the first time, 12 after | 5.9 |
| `use` of a share that does not exist | `STATUS_BAD_NETWORK_NAME`, kind `NotFound` | 5.5 |
| SOCKS5, then `Socks5Stream::into_inner()` and `TcpStream::into_split()` | login, tree connect and `ls huge/` (one compound) through `fileserver.rsmb.test`; the proxy log gained one line | 5.4 |
| `rustyline` 19.0.0 on a pty, driven through a python3 relay | completion works with `Handle::block_on` inside the completer; `CompletionType::List`: a unique candidate is inserted, several give the common prefix and a BEL (0x07), a second Tab lists them **without calling the completer again**; Ctrl-C at the prompt gives a new prompt; Ctrl-D exits; history is read back by a second process | 5.15, 5.16, 6.1 |
| `rustyline` and SIGINT | it replaces the SIGINT handler while it reads a line and puts back what was there before. With tokio's handler installed *after* the first prompt, Ctrl-C during a command killed the process; installed before, it interrupted the command and the shell went on | 5.3 |
| The pty helper's file name | a helper called `pty.py` shadows python's own `pty` module and fails at `pty.fork` | 6.1 |
| `println!` to a closed pipe | panics (`failed printing to stdout: Broken pipe`) | 5.11 |
| Minimal feature sets | `clap` without default features (`std, derive, help, usage, error-context`) and `rustyline` without default features (`with-file-history`) compile and behave as above; the whole dependency tree is 91 crates, of which `smb2` alone brings 67 | 5.1 |

## 3. Decisions on the open questions (master plan section 14)

| # | Question | Decision | Reason |
|---|---|---|---|
| S1-1 | Listing shape | **P** of section 2.1: one compound `[CREATE, QD × n, probe QD, CLOSE]`, a second pass with the handle kept open when the directory did not fit. **f = 1** flight per listing for every directory up to n × b bytes of entries (15.9 MiB on Samba). Fallback, fixed beforehand: if the product's own tests (T4) do not return the server's set of names on every fixture, server and share of the master plan's rule, take A (f = 2), and if A fails too, C (the crate's calls, f = 4), and change the flight numbers in section 6.4 accordingly | It is the master plan's candidate B with two changes the measurement asked for: the buffers are sized so that the whole reply fits one transport frame, and a third, small query makes the end of the listing certain, which lets `huge/` finish in the same flight. It has the fewest flights for `sizes/` (1), returned the server's set everywhere, and lists `huge/` in 1 flight against the limit of 7 |
| S1-2 | JSON schema | Section 5.13. Sizes as integers, times as RFC 3339 in UTC with seconds (`2026-11-17T21:33:05Z`, `null` when the server sends 0), names as JSON strings, file ids as decimal strings | The defaults. File ids are 64-bit and would lose digits as JSON numbers in some readers |
| S1-3 | PTY for tests | A python3 relay, `testenv/ptyrun.py` (about 25 lines), driven by the Rust tests. No dev-dependency | Prototyped while planning: every interactive behaviour the tests need was driven through it |
| S1-4 | Reconnect | Replace the connection: the same dial and login as at start-up, then an empty tree map. The crate's reviver is not used | It is two lines on code that start-up already tests. The reviver needs a trait implementation, its default policy (four attempts with back-off, a 10 s cool-down after a failure) contradicts "once" and "the next command tries again" and would have to be configured away, and the tree map would still be ours to rebuild |
| S1-5 | History file | `$RSMB_HISTORY` if set (empty means no history file); else `$XDG_STATE_HOME/rsmb/history`; else `$HOME/.local/state/rsmb/history`. The directory is created on first save. Every test sets `RSMB_HISTORY` to a file in its own temporary directory | The default |
| S1-6 | Times in text mode | UTC, `YYYY-MM-DD HH:MM:SS`, computed from the FILETIME with the civil-from-days arithmetic (about 15 lines), no time crate. `help ls` and `docs/commands.md` say UTC | The default. Local time would need the zone database |
| S1-7 | `-C`, `-S`, stdin | `-S` first, then every `-C` in order. Then: if stdin is a terminal and `-N` is absent, the interactive shell; if stdin is not a terminal **and neither `-C` nor `-S` was given**, commands are read from stdin, one per line, until end of input; otherwise rsmb exits. `-N` with no `-C`, no `-S` and a terminal on stdin is a usage error | Departs from the default in one point (section 4, D3): with `-C` or `-S`, stdin is never read. A caller that starts `rsmb … -C ls` with an inherited pipe on stdin that nobody closes would otherwise wait for ever, and "never waits for input" is the third use |
| S1-8 | Undecodable names | Replace the bad UTF-16 unit with U+FFFD and list the rest. This needs our own parser for the entries (about 35 lines), because the crate's `parse_entries` fails the whole buffer on one unpaired surrogate (`String::from_utf16`). A name with a replaced unit cannot be opened by that name; `docs/known-limitations.md` says so | The default. One odd name must not hide a directory. The parser is checked against the crate's `pack_entries` |
| S1-9 | Cost of `shares` | Keep `IPC$` in the tree map; no further work: 7 requests the first time on `samba`, 6 after | The default. A compounded variant is noted in `docs/performance.md` as possible, not built |

Decisions on points the master plan left to this plan:

| # | Point | Decision |
|---|---|---|
| P1 | Order of the listing-shape measurement | Done at planning time (section 2.1). The tasks are ordered by dependency; T4 confirms the shape through the product under the fallback rule of S1-1 |
| P2 | Command functions in the table | The table holds an id (`CmdId`); one `match` over the id dispatches. The compiler then refuses a table entry without a function. Function pointers to `async fn` would need boxing |
| P3 | Flags inside commands | A token is a flag only if it is exactly one of the flags the command's table entry declares and comes before a `--` token. Everything else is positional, so `cd -rf` and `ls --help.txt` work on the fixture names. Stage 1 commands declare no flags |
| P4 | Where the retry after a reconnect lives | In one place, `commands::run_line`, driven by the table's `rerun` field. The reconnect itself lives in one other place, `Session::tree` (section 5.10) |
| P5 | Completion of `use` before any `shares` | The share list is fetched once on the first such Tab (like an unlisted directory) and kept |
| P6 | Build profile of the tests | The integration tests run with `--release`: the wall-time rows and the memory row measure the binary that ships, and Stage 2's large transfers need it anyway (S2-7's default). The unit tests run in the dev profile, where clap's `debug_assert` and integer overflow checks are active (finding 8) |
| P7 | Signals | SIGINT and SIGTERM are handled from Stage 1 on, as master plan section 4.3 says, although Stage 1 has nothing to clean up: Ctrl-C during a command must return to the prompt (section 5.3) |
| P8 | File ids in the output | The `entry` object carries `file_id`. It costs nothing, keeps the field from being dead code until Stage 2, and lets a Stage 1 test confirm through the product what the walk will rely on |

## 4. Departures from the master plan

| # | Master plan | Here | Why |
|---|---|---|---|
| D1 | 4.6: buffer = `max_transact`, capped at 8 MiB; candidate B = CREATE + 2 QUERY_DIRECTORY + CLOSE; small `ls` = 4 requests | buffer capped at 8 MiB − 64 KiB (8,323,072); n full queries plus one 64 KiB probe; small `ls` on Samba = **5 requests**, 1 flight | Two full 8 MiB replies in one compound would exceed the 16,777,215-byte transport frame. The probe makes "complete" certain, so `huge/` takes 1 flight instead of 5 (1.7 s against 3.1 s warm at the slow endpoint). The extra query is answered with a bare `NO_MORE_FILES` |
| D2 | 4.9: after a connection-level error a reading command is run again once | run again only when the connection was **closed**; when the server went **silent** (the response deadline), the command fails and the next command that sends starts on a new connection | The master plan's own row 1.10 wants the failure between 15 and 20 s. A second attempt on a link that is still silent adds the 10 s connect timeout (blackhole) or the response deadline once more (paused server) |
| D3 | S1-7 default: stdin is read after `-C` when it is not a terminal | stdin is read only when neither `-C` nor `-S` is given | see S1-7 |
| D4 | section 3: flags `-H -P -d -u -p --no-pass -C -S -N --proxy --json --help-all --timeout` | plus **`-k`, `--keep-going`**: a failing command does not end a non-interactive run; the exit code is that of the first failure | A program that drives rsmb line by line over stdin (an agent, and our own test harness) needs the session to survive a failed `cd`. The default stays decision 7: stop at the first failure |
| D5 | 4.7 and section 11: the cache "with its invalidation entry points" | `get`, `put`, `clear` in Stage 1; `invalidate(share, path, subtree)` is added by Stage 2 with its first caller | Nothing in Stage 1 changes a directory. A function without a caller fails clippy's dead-code check and cannot be tested through the product |
| D6 | section 11: the listing-shape measurement comes first inside Stage 1 | it was made while planning; the confirmation through the product is task T4 | P1 |
| D7 | 4.6: built from `parse_entries` | our own entry parser | S1-8 |
| D8 | section 6: "`./test.sh` removes stale run directories" and the harness's unique run directories, in Stage 1 | the one-line removal of `upload/rsmb-test-*` is in `test.sh` now; the helper that creates such a directory arrives with the first test that writes (Stage 2) | no Stage 1 test writes to a server |

| D9 | 4.9: "no timer of its own around requests" | one exception: the network work of a Tab completion is bounded by 5 s; when the bound expires the connection is marked dead and replaced by the next command that sends | In the line editor the terminal is in raw mode without signals, so Ctrl-C cannot interrupt a completion: on a silent link one Tab would freeze the terminal for 15 to 30 s (finding 6). The master plan's objection to a second timer (an abandoned request leaves the connection in an unknown state) is met by not using that connection again |
| D10 | 4.9: after an interrupted `ls` "the next command may wait for up to one reply buffer" | after an interrupt (or the completion bound) with requests in flight, the connection is marked dead; the next command that sends reconnects (4 round trips) | Measured by the adversary: the abandoned request keeps its credits until its answer arrives, and the crate refuses to wait for credits when nothing is outstanding, so the next request fails at once; on edge even a `cd` (finding 5) |
| D11 | 4.9 and row 1.10: "effective floor 15 s"; fails "between 15 and 20 s" | the wording is "between `--timeout` and max(`--timeout`, 15 s)"; the silent-link tests send a command just before the fault and expect the failure 13 to 20 s after the request | 15 s is not a floor: the crate counts the server's silence from its last frame, not from the request (finding 11) |

Nothing else departs. In particular: no timer of our own around requests except D9, the keepalive
is not touched, no probe, no prefetch on `cd`, `ls` always asks, CLOSE is always awaited inside
the command.

## 5. Design

### 5.1 Workspace, files, dependencies

Root `Cargo.toml` becomes workspace and package:

```toml
[workspace]
resolver = "3"
members = [".", "smbtap"]
exclude = ["spikes"]

[package]
name = "rsmb"
version = "0.1.0"
edition = "2024"
publish = false
description = "Fast SMB shell client"

[dependencies]
smb2 = "=0.43.0"                 # exact pin, master plan 4.2
tokio = { version = "1.54", features = ["rt-multi-thread", "net", "io-util", "time", "sync", "signal", "macros"] }
async-trait = "0.1.92"           # smb2's transport traits are defined with it
tokio-socks = "0.5.3"
clap = { version = "4.6", default-features = false, features = ["std", "derive", "help", "usage", "error-context"] }
rustyline = { version = "19.0", default-features = false, features = ["with-file-history"] }
serde_json = "1.0"
rpassword = "7.5"

[dev-dependencies]
smbtap = { path = "smbtap" }
```

Resolved while planning: `tokio` 1.54.1, `async-trait` 0.1.92, `tokio-socks` 0.5.3, `clap` 4.6.8,
`rustyline` 19.0.0, `serde_json` 1.0.154, `rpassword` 7.5.5. `Cargo.lock` is committed. No
dependency beyond the master plan's list. Not added: `indicatif` (Stage 2), a PTY crate, a time
crate, a temp-file crate (tests use `env!("CARGO_TARGET_TMPDIR")`), `libc` or `nix` directly.

Files (master plan 4.1; Stage 1 writes four source files):

| File | Holds |
|---|---|
| `src/main.rs` | `Args` (clap), start-up rules (password, `-S`/`-C`/stdin), the runtime, signal handling, `run_blocking`, the non-interactive runner, exit codes |
| `src/session.rs` | `Target`, `Proxy`, the dialer and the transport halves, login, `Session` (connection, tree map, share, working directory, cache, kept share list), `cd` check, listing (`plan`, entry parser, both phases), share list, `Cache`, `ensure_connected`, `Error`, `Kind`, `classify`, `resolve`. **All use of `smb2` is in this file** |
| `src/commands.rs` | `COMMANDS` (the table), `CmdId`, `Arg`, `run_line`, argument splitting into flags and positionals, the command functions, `Out` (text / JSON), help rendering, time formatting |
| `src/shell.rs` | `tokenize`, `partial`, `quote`, the completer, the prompt, history, the interactive loop |
| `tests/common/mod.rs` | the harness (section 6.1) |
| `tests/fast.rs`, `tests/slow.rs`, `tests/serial.rs` | section 6.4 |
| `testenv/ptyrun.py` | the pty relay |
| `testenv/bench-rsmb.sh`, `testenv/baseline-smbclientng.txt` | section 6.5 |

Dependencies between the modules: `main` → `shell`, `commands`, `session`; `shell` → `commands`,
`session`; `commands` → `session` and `shell::tokenize`. No traits of our own, no generics over
transports or front ends.

### 5.2 Command line and start-up

```
rsmb -H <host> [-P <port>] [-u <user>] [-d <domain>] [-p <password> | --no-pass]
     [--proxy socks5://[user:pass@]host:port] [--timeout <seconds>]
     [-S <file>] [-C <command>]... [-N] [-k] [--json]
rsmb --help-all [--json]
```

| Flag | Meaning |
|---|---|
| `-H`, `--host <HOST>` | server name or address; required unless `--help-all` (`required_unless_present`) |
| `-P`, `--port <PORT>` | default 445 |
| `-u`, `--user <USER>` | default empty = anonymous. `DOMAIN\user` is split at the first backslash when `-d` is not given |
| `-d`, `--domain <DOMAIN>` | default empty |
| `-p`, `--password <PASSWORD>` | conflicts with `--no-pass` |
| `--no-pass` | empty password, never prompt |
| `--proxy <URL>` | `socks5://[user:pass@]host:port`; anything else is a usage error |
| `--timeout <SECONDS>` | response deadline, default 30, at least 1. A silent server fails a request after between `--timeout` and max(`--timeout`, 15 s); a reply that keeps arriving has 6 × `--timeout` to complete (section 5.10) |
| `-S`, `--startup-script <FILE>` | commands, one per line |
| `-C`, `--command <LINE>` | one command line; repeatable |
| `-N`, `--not-interactive` | no interactive shell after the commands |
| `-k`, `--keep-going` | do not stop at the first failing command |
| `--json` | one JSON object per stdout line |
| `--help-all` | print the manual (as JSON with `--json`) and exit 0; no server needed |
| `-h`, `--help`, `-V`, `--version` | clap's own |

Start-up, in this order:

1. clap parses. A bad flag is clap's message on stderr and exit 2.
2. `--help-all`: print, exit 0.
3. Usage checks, each exit 2 with kind `usage`: the `--proxy` URL; `-S` file unreadable; `-N` with
   no `-C`, no `-S` and a terminal on stdin.
4. Password: `-p` value; else `--no-pass` or an empty user → empty; else, if stdin is a terminal,
   `rpassword::prompt_password("Password for <DOMAIN\user>: ")`; else usage error
   (`no password: give -p or --no-pass`), exit 2, before anything touches the terminal or the
   network. The check is on stdin because `rpassword` itself reads `/dev/tty`.
5. Build the runtime, install the signal handlers (5.3), connect and log in (5.4). A failure here
   exits 3 (2 for usage, 130 when interrupted). In interactive mode one line on stderr says who is
   logged in where and with which dialect.
6. Run the lines of `-S`, then the `-C` lines. A failing line ends the run with its exit code
   unless `-k`; with `-k` the run goes on and the first failure's code is remembered.
7. Then, as S1-7: the interactive shell (a failed start-up line does not prevent it; its exit code
   is 0), or the lines of stdin with the rule of step 6, or exit with the remembered code.

A blank line and a line whose first non-blank character is `#` are skipped everywhere and produce
no output. `exit` and end of input end the run with the remembered code (0 if none).

### 5.3 Runtime, the two front ends, signals

- One multi-thread tokio runtime, 2 workers, built in `main` and owned there. The main thread owns
  the terminal and calls `block_on` for one command at a time; nothing runs between commands
  except the crate's own tasks.
- Both front ends run a line through the same function in `main.rs`:

  ```rust
  /// Runs one line on the runtime; an interrupt drops the command.
  pub fn run_blocking(rt: &Runtime, sess: &mut Session, out: &mut Out, line: &str) -> Flow
  pub enum Flow { Ok, Failed(u8), Exit }      // Failed carries the exit code of the error
  ```

  In this order: create the `Notified` future (`INTERRUPT.notified()`), pin it and call
  `enable()` on it; only then set `BUSY`; then `block_on(select! { biased; _ = &mut notified =>
  …, flow = commands::run_line(sess, out, line) => flow })`; then clear `BUSY`. The future exists
  before `BUSY` says a command is running, so a Ctrl-C in between is not lost (finding 9). When
  the interrupt arm won, the command's future has been dropped; `run_blocking` then calls
  `sess.abandon()` (5.10), prints the error (kind `interrupted`) and the `done` line itself and
  returns `Failed(130)`.
- Signals: `static INTERRUPT: tokio::sync::Notify`, and three `AtomicBool`s: `BUSY` (a command
  or the connect is running), `SHELL` (the interactive loop has started), `TERM`. The two
  `tokio::signal::unix::signal` streams (SIGINT, SIGTERM) are created inside a `block_on`
  **before the line editor is first used** (section 2.2: rustyline saves and restores the handler
  it finds), then moved into one spawned task:
  - SIGINT: if `BUSY`, `INTERRUPT.notify_waiters()`; else, if `SHELL` is false (a non-interactive
    run waiting for its next line on stdin), exit the process with 130; else nothing.
  - SIGTERM: set `TERM`; if `BUSY`, `notify_waiters()`; else exit the process with 130.

  `notify_waiters` stores no permit, so a signal that arrives while no command runs does not
  interrupt the next one.
- Non-interactive: `Failed(130)` ends the run with 130 whatever `-k` says. Interactive: after an
  interrupted command the shell prints `interrupted` and prompts again, unless `TERM` is set, then
  it exits 130. At the prompt the terminal is in raw mode, so Ctrl-C is a key: rustyline returns
  `Interrupted` and the shell prompts again; Ctrl-D on an empty line exits 0.
- The connection attempt at start-up runs under the same `select!`, so Ctrl-C during a slow
  connect exits 130.

### 5.4 Session: target, dialer, login, tree map

```rust
pub struct Proxy  { pub host: String, pub port: u16, pub auth: Option<(String, String)> }
pub struct Target { pub host: String, pub port: u16, pub user: String, pub password: String,
                    pub domain: String, pub proxy: Option<Proxy>, pub timeout: Duration }
pub fn parse_proxy(url: &str) -> Result<Proxy>      // by hand; usage error otherwise

pub struct Session {
    target: Target,
    conn: Connection,
    trees: HashMap<String, Arc<Tree>>,   // key: share name in lower case
    share: Option<String>,               // as the user wrote it
    cwd: Vec<String>,                    // components below the share root, as the user wrote them
    cache: Cache,
    shares: Option<Arc<Vec<Share>>>,     // the last share list, for completing `use`
    in_flight: bool,                     // a method of ours is between its first request and its last answer
}
impl Session {
    pub async fn connect(target: Target) -> Result<Session>
    pub fn mark_dead(&self)                                         // 5.10
    pub fn abandon(&mut self)                                       // 5.10: after a dropped command
    pub fn location(&self) -> String        // "//host", "//host/share/", "//host/share/a/b"
    pub fn resolve(&self, arg: &str) -> Vec<String>
    pub async fn use_share(&mut self, name: &str) -> Result<()>
    pub async fn cd(&mut self, arg: &str) -> Result<()>
    pub async fn list(&mut self, dir: &[String]) -> Result<Arc<Vec<Entry>>>        // asks, refreshes the cache
    pub async fn list_cached(&mut self, dir: &[String]) -> Result<Arc<Vec<Entry>>> // cache, else `list`
    pub async fn shares(&mut self) -> Result<Arc<Vec<Share>>>                      // asks, keeps the result
    pub fn known_shares(&self) -> Option<Arc<Vec<Share>>>
    pub fn refresh(&mut self)               // empties the cache and forgets the share list
}
```

- **Dialer**, one function, `async fn dial(t: &Target) -> Result<Connection>`, the whole of it
  under one `tokio::time::timeout` of 10 s:
  - direct: `TcpStream::connect((host, port))`, `set_nodelay(true)`;
  - proxy: `TcpStream::connect((proxy.host, proxy.port))`, `set_nodelay(true)`, then
    `Socks5Stream::connect_with_socket(tcp, (host, port))` or
    `connect_with_password_and_socket(…)`, the target given as a name so the proxy resolves it,
    then `.into_inner()` to get the plain `TcpStream` back;
  - in both cases `into_split()` and two small structs, `Tx(Mutex<OwnedWriteHalf>)` and
    `Rx { half: Mutex<OwnedReadHalf>, progress: Arc<smb2::transport::ReceiveProgress> }` (tokio's
    async mutex), handed to `Connection::from_transport(tx, rx, host)`.
  - `TransportSend::send`: the 4-byte big-endian length and the frame in **one** `write_all`; a
    frame of 2^24 bytes or more is an error.
  - `TransportReceive::receive`: `read_exact` of the 4-byte header, whose first byte must be 0;
    `progress.begin_frame(len)`; then the body in a loop of `read` into the rest of the buffer,
    `progress.record(n)` after every read, a read of 0 bytes is `smb2::Error::Disconnected`;
    `progress.end_frame()` on every way out, also on an error (a small guard, as the crate's own
    TCP transport does it). `UnexpectedEof` → `Disconnected`, other errors → `smb2::Error::Io`.
  - `TransportReceive::receive_progress` returns `Some(Arc::clone(&self.progress))`. **This is
    not optional** (finding 2): the default returns `None`, and then a large reply that takes
    longer than the response deadline to arrive counts as silence and the connection is torn
    down, although the server is sending. A whole listing is one frame of up to 16 MiB here.
  - Errors are kind `connect`; through a proxy the message starts with `proxy <host>:<port>:`.
- **Login**, `async fn login(t: &Target) -> Result<Connection>`: `dial`, then
  `conn.set_response_timeout(Some(t.timeout))`, `conn.negotiate()`,
  `smb2::Session::setup(&mut conn, &t.user, &t.password, &t.domain)`. Used by `connect` and by
  the reconnect. Guest is user `Guest` with an empty password, anonymous an empty user.
  `GuestSessionRefused` becomes kind `auth` with the message *the server answered the login of
  '<user>' with a guest session (wrong user name or password, on a server that maps unknown users
  to guest). To log in as guest on purpose: -u Guest --no-pass*.
- **Tree map.** `async fn tree(&mut self, share: &str) -> Result<Arc<Tree>>`: first
  `ensure_connected()` (5.10; it sends nothing on a live connection and empties the map when it
  had to reconnect), then the tree from the map, or `Tree::connect(&mut self.conn, share)`
  (1 request), stored and returned. Callers use `tree.tree_id`, and `tree.is_dfs`,
  `tree.server` and `tree.share_name` for the wire path. `use_share` calls it, then sets `share`
  and empties `cwd`. **Every method that sends starts with `tree`** (`use_share`, the `cd` check,
  `list`, `shares` with `IPC$`); nothing else calls `ensure_connected`, so a command that sends
  nothing never touches the network, dead connection or not (finding 3). Nothing ever sends
  TREE_DISCONNECT.
- **A share that encrypts changes the whole connection.** After a tree connect to a share with
  `smb encrypt = required`, the crate encrypts every later request on that connection, to every
  share, until the connection is replaced. That is the crate's behaviour and is kept; the
  documents say it, and the tests that count named requests never select `encrypted` (F12 does,
  in a session of its own).
- **`resolve(cwd, arg)`**: split `arg` at `/` and `\`; a leading separator starts from the root,
  otherwise from `cwd`; empty components and `.` are dropped; `..` removes the last component and
  does nothing at the root. No request.
- Wire paths, one pure function
  `wire_path(dfs: Option<(&str, &str)>, components: &[String]) -> String` (`dfs` is
  `Some((&tree.server, &tree.share_name))` when `tree.is_dfs`): the components joined with `\`,
  no leading separator; the share root is the empty string. On a DFS tree the crate sets the
  DFS flag on every request for that tree, and the path must then start with the server and the
  share: `<tree.server>\<tree.share_name>` for the root,
  `<tree.server>\<tree.share_name>\<path>` otherwise, exactly as the crate's own `format_path`
  writes it (finding 10). DFS links are not followed: a path that crosses one fails with the
  server's status. Names go out and come back as they are: `smb2::name` (the private-use
  mapping) is not used in Stage 1 (section 9).

### 5.5 Errors and exit codes

```rust
pub enum Kind { NotFound, AccessDenied, NotADirectory, IsADirectory, AlreadyExists, DiskFull,
                InvalidName, Auth, Connect, ConnectionLost { silent: bool }, Interrupted,
                LocalIo, Usage, Other }
pub struct Error { pub kind: Kind, pub message: String }
pub type Result<T> = std::result::Result<T, Error>;
impl Kind  { pub fn name(self) -> &'static str }      // "not_found" … "connection_lost" … "other"
impl Error { pub fn exit_code(&self, at_startup: bool) -> u8 }
pub fn classify(e: smb2::Error, what: &str) -> Error  // `what` names the path, share or step
```

- A raw frame with a non-success status is turned into
  `smb2::Error::Protocol { status, command }` and goes through `classify` too, so there is one
  mapping.
- `classify`, by `e.kind()`: `AccessDenied`, `NotFound`, `NotADirectory`, `IsADirectory`,
  `AlreadyExists`, `DiskFull`, `InvalidName` → the kind of the same name. `AuthRequired`,
  `SigningRequired`, `GuestSessionRefused` → `Auth`. `ConnectionLost` → `ConnectionLost
  { silent }` with `silent = matches!(e, smb2::Error::ServerUnresponsive { .. })`.
  `TimedOut` → `ConnectionLost { silent: true }`. `SessionExpired`, `Io` → `ConnectionLost
  { silent: false }`. Everything else → `Other`, with the crate's text, which carries the NT
  status.
- The message is a whole line for a person: `<what>: <reason>`, for example
  `/tree/nope: not found`, `share 'nope': not found`. A lost connection reads
  `connection lost: <cause>`: for a silent server (`ServerUnresponsive`) `connection lost: no
  answer from the server for <n> s`, for a response timeout (`Timeout`) the text of 5.10 that
  names `--timeout`; tests match on `connection lost`.
- `not found` covers `OBJECT_NAME_NOT_FOUND`, `OBJECT_PATH_NOT_FOUND`, `BAD_NETWORK_NAME`.
- Exit codes: `Usage` → 2; `Interrupted` → 130; at start-up everything else → 3; later
  everything else → 1.
- Local errors (a script file, a closed stdout, the history file) are `LocalIo`, except the `-S`
  file at start-up, which is `Usage`.

### 5.6 `cd`

`cd [<dir>]`; without an argument the target is the share root.

1. No share selected → `Usage` (`no share selected: use <share> first`).
2. `target = resolve(arg)`. If `target` is empty or a prefix of `cwd` (component-wise, exact
   bytes; this includes `cwd` itself), set `cwd = target` and send nothing.
3. Otherwise `tree(share)` (which reconnects if it must), and one `execute_compound` of two
   operations on that tree, the name from `wire_path`:
   - `CreateRequest { requested_oplock_level: OplockLevel::None, impersonation_level:
     Impersonation, desired_access: FILE_READ_ATTRIBUTES | SYNCHRONIZE (0x0010_0080),
     file_attributes: 0, share_access: READ | WRITE | DELETE, create_disposition: FileOpen,
     create_options: FILE_DIRECTORY_FILE (0x1), name: <wire path>, create_contexts: [] }`;
   - `CloseRequest { flags: 0, file_id: FileId::SENTINEL }`.
4. The answer is the CREATE's status: success → `cwd = target`; anything else → the classified
   error, `cwd` unchanged. An outer or inner `Err` from the crate is classified the same way.

2 requests, 1 flight, `chain_len == 2`. The working directory keeps the spelling the user typed
(the server compares without case; finding the stored spelling would cost a request).

### 5.7 Listing

```rust
pub struct Entry { pub name: String, pub size: u64, pub mtime: u64 /* FILETIME */,
                   pub attributes: u32, pub file_id: u64 }
impl Entry { pub fn is_dir(&self) -> bool }      // attributes & 0x10

fn plan(max_transact: u32, credits: u16) -> (u32, usize)        // (b, n)
async fn list_dir(conn: &Connection, tree: TreeId, path: &str, b: u32, n: usize) -> Result<Vec<Entry>>
fn parse_entries(buf: &[u8], out: &mut Vec<Entry>) -> Result<()>
```

**`plan`** (pure, unit-tested):

```
PROBE     = 65_536
MAX_FRAME = 16 MiB − 128 KiB
c = max(credits, 4) − 3                       // CREATE, probe and CLOSE cost one credit each
b = min(max_transact, 8 MiB − 64 KiB, c × 65_536), rounded down to a multiple of 65_536, at least 65_536
q = b / 65_536                                // credit charge of one full query
n = clamp(min(MAX_FRAME / b, c / q), 1, 13)
```

Samba after login: (8,323,072, 2), 257 credits. Edge: (65,536, 13), 16 credits. 13 is the longest
run of queries that was measured in one compound. `Session::list` calls `plan` before every
listing with `conn.credits()` and the negotiated transact size (`conn.params()` is an `Option`;
it is `Some` after every login, and 65,536 is used if it ever is not).

With 4 credits or more the compound never asks for more than there are (the unit test says so
without a `max`). With fewer than 4 the smallest compound (CREATE, one query, probe, CLOSE)
cannot be funded and the crate refuses it at once; such a server cannot be listed, and neither
can `cd` work on fewer than 2 (`docs/known-limitations.md`; section 10, assumption 2). The plan
may spend every credit the connection has. That is safe only because nothing else is in flight
during a listing and because an abandoned listing gets a new connection (5.10).

**`list_dir`**, two phases, both awaited to the end inside the command:

1. One compound: `CREATE`, `QD(b)` × n, `QD(PROBE)`, `CLOSE`.
   - CREATE: as in 5.6 but `desired_access: FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES |
     SYNCHRONIZE (0x0010_0081)`.
   - Every query: `QueryDirectoryRequest { file_information_class:
     FileIdBothDirectoryInformation, flags: RESTART_SCANS on the first and 0 on the others,
     file_index: 0, file_id: FileId::SENTINEL, output_buffer_length, file_name: "*" }`, as a
     `CompoundOp` with `credit_charge: CreditCharge(ceil(length / 65_536))`.
   - CLOSE on `FileId::SENTINEL`.
   - CREATE not successful → the classified error; the other answers are not looked at.
   - Each query answer in order, through one pure function
     `query_outcome(status, first: bool) -> Entries | End | Fail`: `SUCCESS` → parse its entries;
     `NO_MORE_FILES` → the listing is complete; `NO_SUCH_FILE` on the **first** query → complete
     and empty (what a server without `.` and `..` entries answers for an empty directory,
     finding 10); any other status → the classified error.
   - If any query ended the listing: done, 1 flight.
2. Otherwise the directory is larger than n buffers and the handle is closed. Throw the entries
   away and list again with the handle kept: compound `CREATE`, `QD(b)` × n; then, until a query
   answers `NO_MORE_FILES`, compounds of `QD(b)` × n, the first with the real file id and the
   others with `SENTINEL`; then a single CLOSE with the real file id. An error from a query after
   the CREATE succeeded still sends the CLOSE before it is returned, unless the error is a lost
   connection.

There is no run-time fallback to another shape when a server refuses a query of the compound
(finding 10, rejected in section 11): the error is reported with the server's status.

A reply is read without copying it: in `frame.body`, `OutputBufferOffset` is the u16 at offset 2
(counted from the start of the 64-byte SMB2 header, so subtract 64) and `OutputBufferLength` the
u32 at offset 4. Each frame is dropped as soon as its entries are parsed.

**`parse_entries`**, class 37 (MS-FSCC 2.4.22), offsets from the start of an entry:
`NextEntryOffset` u32 at 0, `LastWriteTime` u64 at 24, `EndOfFile` u64 at 40, `FileAttributes`
u32 at 56, `FileNameLength` u32 at 60, `FileId` u64 at 96, the name in UTF-16LE at 104. The walk
ends when fewer than 104 bytes remain or `NextEntryOffset` is 0. A name that runs past the buffer
is an error (`Other`, malformed reply). The name is decoded with `String::from_utf16_lossy`.

`list_dir` drops `.` and `..`. `Session::list` sorts the entries by name (byte order of the
UTF-8), wraps them in an `Arc`, puts them into the cache and returns them.

What a listing costs on Samba: 5 requests, 1 flight, for every directory with up to 15.9 MiB of
entries (an entry is 104 bytes plus the name in UTF-16, padded to 8: about 138,000 names of 7
characters, 27,000 names of 250). Above that: one more pass, which sends those 15.9 MiB twice
(about 1 s at the slow endpoint's 17 MB/s). On a server with 64 KiB buffers and 16 credits:
1 flight up to 832 KiB of entries (about 7,000 short names); above that the flights are
1 + ⌈buffers ÷ 13⌉ + 1 (`many/` 4, `huge/` 17).

### 5.8 Cache

```rust
pub struct Cache { map: HashMap<(String, String), (Arc<Vec<Entry>>, u64)>, total: usize, cap: usize, tick: u64 }
impl Cache {
    pub fn new(cap: usize) -> Cache                 // rsmb uses 500_000 entries
    pub fn get(&self, share: &str, dir: &[String]) -> Option<Arc<Vec<Entry>>>
    pub fn put(&mut self, share: &str, dir: &[String], entries: Arc<Vec<Entry>>)
    pub fn clear(&mut self)
}
```

- Key: share and directory path, both through `to_lowercase()`, the path joined with `/`.
- `put`: remove an older entry for the key; if the listing alone has more entries than `cap`, do
  not store it; else, while `total + len > cap`, remove the entry with the smallest tick (a linear
  search, only at the cap); then insert with the next tick.
- Filled by `ls` and by completion. Emptied by `refresh`. Kept across a reconnect.
- `ls` always asks the server; completion reads the cache and asks only on a miss.

### 5.9 Share list

`Session::shares`: the Stage 0 spike's `all_shares` without the tree disconnect: the tree for
`IPC$` from the tree map; CREATE `srvsvc` (`FILE_READ_DATA | FILE_WRITE_DATA`, no create
options); WRITE `smb2::rpc::build_srvsvc_bind(1)`; READ and `parse_bind_ack`; WRITE
`build_net_share_enum_all(2, "\\\\<server_name>")`; READ (65,536 bytes, repeated while the status
is `BUFFER_OVERFLOW`) and `parse_response_fragment`, repeated until the last fragment; CLOSE;
then the stub is wrapped into one response PDU and parsed with
`parse_net_share_enum_all_response`.

```rust
pub struct Share { pub name: String, pub share_type: u32, pub comment: String }
fn share_pdu(stub: &[u8]) -> Result<Vec<u8>>     // Err when 24 + stub.len() > 65_535
```

`share_pdu` fails with `Other` and the message *the share list is too large for rsmb (<n> bytes;
the limit is 65,511)*; nothing is returned in that case. The result, in the server's order, is
kept in `Session::shares` until `refresh`.

### 5.10 Timeouts, dead connections, reconnect, interruption

- **The deadline.** The only timer on requests is the crate's response deadline, set from
  `--timeout` at login (default 30 s); the one exception is the completer (below). The keepalive
  is not touched. A request to a server that has gone silent fails after between `--timeout` and
  max(`--timeout`, 15 s): the crate extends the deadline while the server's last frame is less
  than 15 s old, counted from that frame and not from the request. A request sent right after an
  answer therefore fails after about 15 s with `--timeout 5`; one sent after 15 s of quiet fails
  after 5 s. A reply that is still arriving is not silence, because the receive half reports its
  progress (5.4); but the crate extends the deadline of one request to at most 6 × `--timeout`
  however much is arriving, so **one reply has to be complete within 6 × `--timeout`** (3 minutes
  by default). A listing is one reply of up to 16 MiB: on wan-bad `huge/` (11.4 MiB) took 119.6 s
  with the default, and failed after 34.0 s with `--timeout 5` although bytes were flowing
  (measured for this revision). The error for a response timeout therefore says what to do:
  `connection lost: no complete reply after <n> s; on a slow link raise --timeout`.
  The dial has its own 10 s.
- **No probe.** A dead connection is known in two ways, neither of which sends anything: the
  crate's receiver task saw the close (`conn.is_disconnected()`), or a request failed.
- **`ensure_connected`** (private, called only by `tree`, 5.4): if `conn.is_disconnected()`,
  then `conn = login(&target)` and `trees.clear()`; share, working directory, cache and share
  list stay. A failure is `ConnectionLost { silent: false }` with the message
  `reconnect failed: <cause>`; the dead connection stays in place, so the next command that
  sends tries again. Commands that send nothing (`pwd`, `help`, `refresh`, `exit`, a `cd` to the
  root or an ancestor) and completions served from the cache never call it: after a cut they
  cost nothing and do not wait on a dead link.
- **`mark_dead`** is `conn.mark_dead()`. It is called in exactly three places, each before
  anything else is sent: by `run_line` when a command returned `ConnectionLost` of either sort
  (the crate reports that kind also while the socket is still open: `NETWORK_NAME_DELETED`,
  `USER_SESSION_DELETED`, an expired session, an I/O error, an invalidated handle, a response
  timeout); by the completer for the same error; and by `abandon`.
- **`in_flight` and `abandon`.** Every `Session` method that sends sets `in_flight` before its
  first request and clears it after its last answer, on success and on error. A future dropped
  in between leaves it set. `Session::abandon()`: if `in_flight`, clear it and `mark_dead()`.
  It is called after an interrupted command (5.3) and after the completer's bound expired.
  Reason: a dropped request keeps its credits and its reply until the server has answered, no
  CANCEL is sent, and the crate refuses to wait for credits when nothing is outstanding; the
  next request would fail at once or wait behind up to 16 MiB of reply. A new connection costs
  four round trips and is in a known state.
- **The retry**, in `commands::run_line` and nowhere else:

  ```
  r = dispatch(cmd)                       // a method that sends goes through tree(), which reconnects if needed
  if r is Err(ConnectionLost { .. }):     sess.mark_dead()
  if r is Err(ConnectionLost { silent: false }) and cmd.rerun:
      r = dispatch(cmd)                   // tree() now makes the new connection; no third attempt
      if r is Err(ConnectionLost { .. }): sess.mark_dead()
  ```

  So: a connection that died while idle is replaced when the next command that sends starts,
  without a failed first attempt. A connection closed, or a session lost, in the middle of a
  reading command costs one new connection and the command runs again from its start. A server
  that went silent fails the command after the deadline and is not tried again inside that
  command (D2); the next command that sends reconnects. All Stage 1 commands that send
  (`shares`, `use`, `cd`, `ls`) have `rerun: true`; they print only after their last request, so
  running one again prints nothing twice.
- **The completer** (5.15) runs its network work, a listing or the share list on a cache miss,
  under `tokio::time::timeout(5 s, …)`. On expiry: `abandon()`, no candidates, the editor beeps.
  On a `ConnectionLost` error: `mark_dead()`, no candidates, no retry. A cache hit sends
  nothing and has no bound to hit. Consequence, stated in the documents: Tab in a directory that
  is not cached gives up after 5 s on a link too slow to list it in that time; `ls` there first
  (no bound) fills the cache.
- **Interruption** drops the command's future; `abandon()` then replaces the connection if
  requests were in flight. Nothing is left open on the server beyond the old session, which the
  server ends with the connection.

### 5.11 The command layer

```rust
pub enum CmdId { Shares, Use, Cd, Pwd, Ls, Refresh, Help, Exit }
pub enum Arg { Command, Share, RemoteDir, RemotePath }   // what a positional argument completes to
pub struct Cmd {
    pub id: CmdId,
    pub name: &'static str,
    pub syntax: &'static str,                        // "cd [<dir>]"
    pub summary: &'static str,                       // one line
    pub details: &'static str,                       // what it does, what it costs, what it prints
    pub flags: &'static [(&'static str, &'static str)],      // (flag, meaning); empty in Stage 1
    pub args: &'static [Arg],                        // positional arguments, in order
    pub required: usize,                             // how many of them are required
    pub examples: &'static [(&'static str, &'static str)],   // (line, what it does); never empty
    pub rerun: bool,                                 // may run again after a reconnect
}
pub static COMMANDS: &[Cmd] = &[ … ];

pub async fn run_line(sess: &mut Session, out: &mut Out, line: &str) -> Flow

pub struct Out { pub json: bool, /* buffered stdout */ }
impl Out {
    pub fn text(&mut self, line: &str)                 // text mode only
    pub fn obj(&mut self, v: &serde_json::Value)       // JSON mode only
    pub fn error(&mut self, e: &Error, command: Option<&str>)
    pub fn done(&mut self, command: &str, ok: bool)    // JSON mode only; flushes in both modes
}
```

- `run_line`: skip blank and `#` lines → `shell::tokenize` → look the first token up in
  `COMMANDS` (unknown → `Usage`, `unknown command '<x>'; see 'help'`) → split the rest into flags
  and positionals (P3), check the count against `required` and `args.len()` (wrong →
  `Usage` with the syntax line) → `dispatch` with the retry of 5.10 → on an error `out.error` →
  `out.done` → `Flow`.
- `dispatch` is one `match cmd.id` calling one function per command:
  `async fn ls(sess: &mut Session, out: &mut Out, args: &[String]) -> Result<()>` and so on.
- `Out` writes with `writeln!` to a buffered stdout and remembers the first write error;
  `done` flushes and, if a write failed, the line's result becomes `LocalIo` (exit 1). Nothing
  uses `println!` (section 2.2). In text mode errors go to stderr as `rsmb: <message>`, after
  stdout has been flushed, so that the order on a terminal is the order of events; in JSON mode
  they are objects on stdout and stderr stays empty.
- `ls` checks `out.json` once and builds only the form it prints.

Adding a command in a later stage is: a `CmdId` variant, a table entry, a function, a `match`
arm, a test, a section in `docs/commands.md`.

### 5.12 The commands

Requests are for Samba; "0" means nothing is sent.

| Command | Does | Requests / flights | Text output | JSON objects before `done` |
|---|---|---|---|---|
| `shares` | asks for the share list, keeps it | 7 / 7 first, 6 / 6 after | one line per share: name (20 wide), type, comment | one `share` each |
| `use <share>` | selects the share, working directory = its root | 1 / 1 first time per share and connection, then 0 | none | none |
| `cd [<dir>]` | 5.6 | 0 for the root and for ancestors, else 2 / 1 | none | none |
| `pwd` | prints the location | 0 | `//host/share/a/b` (`//host` without a share, `//host/share/` at the root) | one `pwd` |
| `ls [<dir>]` | lists the directory (default: the working directory), refreshes its cache entry | 5 / 1 | one line per entry, sorted by name: `d` or `-`, the size right-aligned in 13 columns, two spaces, the modification time in UTC as `YYYY-MM-DD HH:MM:SS` (or `-`), two spaces, the name, `/` after a directory | one `entry` each, same order |
| `refresh` | empties the cache and forgets the share list | 0 | none | none |
| `help [<command>\|all]` | 5.14 | 0 | the help text | `command` objects (and the others of 5.14 for `all`) |
| `exit` | ends the session | 0 | none | none |

`ls`, `cd` without a selected share → `Usage`. `ls <file>` → `not_a_directory` (showing the one
entry is left out of Stage 1 and listed in `docs/known-limitations.md`). Too many or too
few arguments → `Usage`. Examples in the table are written for the fixture share, from the root
of `share`: `use share`, `cd tree/d0`, `cd /`, `cd ..`, `ls`, `ls /sizes`, `ls "special/dir with
space"`, `pwd`, `shares`, `refresh`, `help`, `help cd`, `help all`, `exit`.

### 5.13 JSON lines (`--json`)

Every stdout line is one object with a `type`. Nothing else is written to stdout.

| `type` | Fields | When |
|---|---|---|
| `share` | `name` string, `share_type` `"disk"`, `"printer"`, `"device"`, `"ipc"` or `"unknown"` (low 16 bits of the type), `hidden` bool (name ends in `$`, or the type has bit 31), `comment` string | `shares` |
| `entry` | `name` string, `path` string (from the share root, starting with `/`, components joined by `/`), `is_dir` bool, `size` integer, `mtime` string or `null`, `file_id` string of decimal digits | `ls` |
| `pwd` | `host` string, `share` string or `null`, `path` string (`/`, `/a/b`) | `pwd` |
| `error` | `kind` string (one of the 14 names of 5.5), `message` string, `command` string or `null` (start-up) | any failure |
| `done` | `command` string (the line as given, trimmed), `ok` bool | after every command line that was run, last line of that command |
| `command` | `name`, `syntax`, `summary`, `details` strings, `flags` array of `{flag, help}`, `examples` array of `{line, does}` | `help`, `help <cmd>`, `help all`, `--help-all` |
| `option` | `flag` string (`-H, --host <HOST>`), `help` string | `help all`, `--help-all` |
| `exit_code` | `code` integer, `meaning` string | `help all`, `--help-all` |
| `topic` | `name`, `text` strings | `help all`, `--help-all` |
| `json_object` | `name` (a value of `type`), `fields` string | `help all`, `--help-all` |

A failed command prints `error`, then `done` with `ok: false`. A failed start-up prints one
`error` and no `done`. Skipped lines print nothing. The `json_object` list comes from one
constant in `commands.rs`; a test checks that every `type` rsmb ever prints is in it.

### 5.14 `help`

- `help`: one line per command (name and summary), then one line pointing to `help <command>`
  and `help all`.
- `help <command>`: syntax, summary, details, flags, every example with what it does.
- `help all` and `rsmb --help-all`, the same text, in this order:
  1. what rsmb is, in three lines;
  2. the synopsis and every start-up flag, **rendered from clap's own metadata**
     (`Args::command().get_arguments()`: short, long, value name, help), so the flags cannot
     drift;
  3. how commands are given: `-S`, `-C`, stdin, the interactive shell, `-N`, `-k`, blank and `#`
     lines, stop at the first failure;
  4. quoting and paths (the rules of 5.15, `/` and `\`, relative and absolute, `--`);
  5. every command in table order, as `help <command>` prints it;
  6. `--json`: every object type with its fields, and the `done` rule;
  7. exit codes 0, 1, 2, 3, 130;
  8. timeouts and reconnect in a few lines: `--timeout` and its two limits (a silent server:
     between `--timeout` and max(`--timeout`, 15 s); one reply: 6 × `--timeout`), what happens
     when the connection is lost, after Ctrl-C, and after a Tab that ran into its 5 s bound;
  9. two complete examples of a non-interactive call, one text, one `--json`.
- In JSON mode the same content as objects: `command` for 5, `option` for 2, `exit_code` for 7,
  `json_object` for 6, `topic` for 1, 3, 4, 8 and 9.
- `help` inside a session costs no request; `--help-all` needs no `-H`.

Rules for the content (AGENTS.md rule 2): every command has at least one example; every example
is a line that runs successfully in a session that has done `use share` on the test server and
stands at the share root; no example changes the server; the text names every flag, every exit
code and every JSON type.

The test that ties the table, `help all` and `docs/commands.md` together is row H1–H4 of
section 6.4.

### 5.15 Tokeniser, quoting, completion

```rust
pub fn tokenize(line: &str) -> Result<Vec<String>>           // Usage on an open quote or a trailing backslash
pub fn partial(line: &str) -> (Vec<String>, String, usize)   // finished tokens, the token under the cursor, where it starts
pub fn quote(token: &str) -> String
```

- Tokens are separated by ASCII white space only (space, tab, CR, LF, VT, FF). U+3000 is part of
  a name.
- `'…'`: everything up to the next `'`, unchanged.
- `"…"`: up to the next `"`; inside, `\"` is `"` and `\\` is `\`; any other backslash stays.
- Outside quotes a backslash takes the next character literally, whatever it is.
- Pieces that touch form one token (`a' 'b` is `a b`); `''` is an empty token.
- No variables, no globbing, no comments inside a line.
- A consequence the documents state: a backslash as path separator has to be quoted or doubled
  (`cd 'tree\d0'`, `cd tree\\d0`); `/` needs nothing.
- `partial` takes the line up to the cursor and never fails: an open quote runs to the end, a
  trailing backslash is ignored. If the line ends in white space the partial token is empty.
- `quote`: a backslash before each ASCII white-space character, `'`, `"` and `\`; the empty
  string becomes `''`. `tokenize(quote(x)) == [x]` for every string without NUL.

Completion (`rustyline::completion::Completer`, candidates are `Pair { display, replacement }`):

1. `partial(&line[..pos])`. No finished token → complete the command name from `COMMANDS`,
   replacement `name` plus a space.
2. Otherwise look the command up; the index of the positional argument under the cursor (flags
   and `--` skipped as in P3) selects the `Arg` of the table; beyond the declared arguments, or
   for an unknown command, there are no candidates.
3. `Arg::Command`: command names and `all`. `Arg::Share`: the kept share list, fetched once if
   absent (P5, under the 5 s bound of 5.10); names with the typed prefix; replacement
   `quote(name)` plus a space.
4. `Arg::RemoteDir` (`cd`) and `Arg::RemotePath` (`ls`): split the partial token at its last `/`
   or `\` into the directory part as typed and the prefix; `resolve` the directory part;
   `list_cached`, under the 5 s bound of 5.10 (a cache hit sends nothing; a miss lists, and only
   then is a dead connection replaced); the candidates are the entries whose name starts with the
   prefix (exact bytes, no case folding, no normalisation), for `RemoteDir` the directories only;
   replacement `quote(directory part + name)` plus `/` for a directory and a space for a file;
   display the name, with `/` for a directory. No share selected, or any error → no candidates.
   (`ls` completes file names although `ls <file>` is an error in Stage 1: the listing of the
   directory is what Tab is for, and Stage 2's commands take files.)
5. The start position returned to rustyline is the start of the partial token, so the whole
   token is replaced by its canonical quoted form, whatever quotes the user had opened.

Cost: a completion in a directory that is not cached is one listing (5 requests, 1 flight); every
other completion is 0 requests. While a completion runs, Ctrl-C does nothing (the editor has the
terminal in raw mode without signals, and the key is read only after the completer returns);
the 5 s bound is what ends a completion on a dead link.

### 5.16 The interactive shell

- `Editor<Helper, DefaultHistory>` with `Config::builder().completion_type(CompletionType::List)
  .completion_prompt_limit(100).max_history_size(1000).history_ignore_dups(true)
  .auto_add_history(true)`. The helper implements `Completer` and has empty `Hinter`,
  `Highlighter` and `Validator` implementations.
- The session is shared between the loop and the helper as `Rc<RefCell<Session>>`; the helper
  also holds a `tokio::runtime::Handle` and calls `handle.block_on` for a listing. The loop holds
  no borrow while `readline` runs.
- Prompt: `sess.location()` followed by `> `. Building it sends nothing.
- Loop: `readline` → `Ok(line)` → `run_blocking`; `Flow::Exit` ends it; `Interrupted` → prompt
  again; `Eof` → end; any other error → message, end. History is loaded before the first prompt
  and saved when the loop ends (S1-5).
- No colours, no banner beyond the one login line of 5.2.

## 6. Tests

### 6.1 The harness, `tests/common/mod.rs`

```rust
pub enum Ep { Fast, Slow, Edge, Bad }            // .addr() -> "10.213.45.10:445" …, .container()
pub const HOLE: &str = "10.213.45.30:4450";
pub const LOGIN: [&str; 6] = ["-u", "tester", "-p", "testpass", "-d", "TESTDOM"];

/// One rsmb process fed line by line: `--json --keep-going`, stdin and stdout piped.
pub struct Client { /* child, stdin, stdout lines over a channel, stderr collected, tap */ }
impl Client {
    pub fn tapped(ep: Ep) -> Client              // through a new Tap, user login
    pub fn direct(addr: &str) -> Client          // no tap
    pub fn with(addr: Option<&str>, tap: Option<Ep>, args: &[&str]) -> Client   // full control
    pub startup: Reply                           // of the `pwd` every constructor sends first: the start-up cost
    pub fn cmd(&mut self, line: &str) -> Reply   // write the line, read to `done`, settle, read and reset the tap
    pub fn send(&mut self, line: &str)           // write only (for tests that cut or signal in between)
    pub fn wait_requests(&self, command: &str, n: u64, timeout: Duration)   // until the tap has seen n requests of that command
    pub fn wait_done(&mut self, timeout: Duration) -> Reply
    pub fn tap(&self) -> &smbtap::Tap
    pub fn pid(&self) -> u32
    pub fn peak_rss_kb(&self) -> u64             // VmHWM from /proc/<pid>/status
    pub fn finish(self) -> (i32, String)         // close stdin, wait at most 60 s: exit code and stderr
}
pub struct Reply { pub lines: Vec<serde_json::Value>, pub ok: bool, pub stats: Option<smbtap::Stats>, pub took: Duration }
impl Reply {
    pub fn kind(&self) -> Option<String>                     // of the error object
    pub fn names(&self) -> BTreeSet<Vec<u8>>                 // of the entry objects
    pub fn expect(&self, what: &str, requests: u64, flights: u64)   // panics with the message below
}

pub fn run(args: &[&str], stdin: Option<&str>) -> Output     // one-shot, at most 60 s: code, stdout, stderr, duration
pub fn tmp(test: &str) -> PathBuf                            // an empty directory for this test, wiped at every call
pub fn server_names(container: &str, dir: &str) -> BTreeSet<Vec<u8>>     // docker exec … ls -A
pub fn fixture_names(var: &str) -> Vec<String>               // bash: . testenv/samba/names.sh; printf of $CJK_NAMES …
pub fn ctl(args: &[&str]) -> String                          // testenv/ctl.sh
pub fn connect_ms(addr: &str) -> f64                         // median of 5 TCP connects after one warm-up
pub fn q(name: &str) -> String                               // the tokeniser's quoting, written again here on purpose

/// rsmb on a pseudo-terminal: `python3 testenv/ptyrun.py 200 50 <rsmb> <args…>`.
pub struct Pty { /* child, stdin, output chunks over a channel, everything read so far */ }
impl Pty {
    pub fn start(addr: &str, args: &[&str]) -> Pty           // TERM=xterm, RSMB_HISTORY in the test's directory
    pub fn tapped(ep: Ep, args: &[&str]) -> (Pty, smbtap::Tap, smbtap::Stats)   // the Stats are the start-up's
    pub fn send(&mut self, bytes: &[u8])
    pub fn expect(&mut self, needle: &str, timeout: Duration) -> String   // output since the last expect, up to and including the needle
    pub fn exit_code(self, timeout: Duration) -> i32
}
```

Rules of the harness:

- The binary is `env!("CARGO_BIN_EXE_rsmb")`; temporary files go under
  `env!("CARGO_TARGET_TMPDIR")/<test name>`, which `tmp` removes and creates again, so a history
  file from an earlier run cannot answer an Up key.
- Nothing waits without a limit: `cmd`, `wait_done`, `wait_requests`, `finish`, `run`,
  `Pty::expect` and `Pty::exit_code` all have a timeout; when it passes the child is killed and
  the test panics with what was read so far.
- `Client::cmd` waits for the `done` line with a timeout (60 s unless the test says otherwise),
  then `tap.settle(300 ms, 60 s)` must return true, then `tap.stats()`, then `tap.reset()`. So a
  request sent late is charged to the command that caused it. A timeout panics with the lines
  received so far and the child's stderr, and kills the child; no test can hang the gate.
- Start-up is counted on its own: every `Client` constructor sends `pwd` and keeps its `Reply`
  in `startup` (3 requests, 1 connection), so the first command a test sends is charged only
  what it costs itself. `Pty::tapped` does the same: it waits for the first prompt, settles,
  returns the start-up's `Stats` and resets the tap.
- A test that must act while a command is on the wire (cut, interrupt) does not sleep: it calls
  `send`, then `wait_requests("QUERY_DIRECTORY", 1, …)`, then acts. The pty tests do the same on
  their tap.
- After `use encrypted` every request of that session is an opaque frame, to every share. Only
  F12 selects that share; a test that counts named requests never does.
- `Reply::expect` prints: what, endpoint, expected and actual requests and flights, the tap's
  `per_command` map, and the request log (command, flight, chain index and length).
- `Pty::expect` panics with the whole transcript, escaped, when the needle does not come in time.
  The terminal is 200 columns wide so that no line of a test wraps. Text is matched as a
  substring of the raw output; rustyline's escape sequences sit between redraws, not inside the
  text it draws.
- The end of a completion is seen in the output: the inserted name, or the BEL (`\x07`) for
  several candidates. A test never waits a fixed time for one. Because a second Tab in a row only
  lists what the first one found (section 2.2), "a second completion" in a test is always a new
  line: Ctrl-U, new text, Tab.
- `testenv/ptyrun.py` (not `pty.py`, section 2.2): `pty.fork()`, exec the command in the child,
  set the window size with `TIOCSWINSZ`, then relay stdin → pty and pty → stdout with `select`
  until the pty closes; exit with the child's code, or 128 + signal.
- A test that pauses a server or turns the blackhole on holds a guard whose `Drop` undoes it.
- `tests/serial.rs` takes one process-wide mutex in every test, so it is serial even when
  somebody forgets `--test-threads=1`.

### 6.2 `./test.sh`

```
./test.sh fast    environment up · stale test directories · check.sh · fmt · clippy · build ·
                  unit tests · tests/fast
./test.sh         the same, then tests/slow · tests/serial · smbtap against tshark
```

| Step | Command | Lock |
|---|---|---|
| test environment up | `./testenv/up.sh` | its own |
| stale test directories | `docker exec <c> sh -c 'rm -rf /srv/share/upload/rsmb-test-*'` for the three Samba containers | none |
| test environment check | `./testenv/check.sh` | its own, exclusive |
| cargo fmt --check | `cargo fmt --all --check` | none |
| cargo clippy | `cargo clippy --workspace --all-targets -- -D warnings` | none |
| build | `cargo test --workspace --lib --bins --no-run`, then `cargo test --release --workspace --no-run` | none |
| unit tests | `cargo test --workspace --lib --bins` (dev profile: debug assertions and overflow checks on) | none |
| tests/fast | `flock -s /tmp/rsmb-env.lock cargo test --release --test fast` | shared |
| tests/slow (full) | `flock -s /tmp/rsmb-env.lock cargo test --release --test slow` | shared |
| tests/serial (full) | `flock -x /tmp/rsmb-env.lock cargo test --release --test serial -- --test-threads=1` | exclusive |
| smbtap against tshark (full) | `./testenv/validate-smbtap.sh` | none |

- The meaning of `fast` changes from "no tshark" to "fast endpoint only" (master plan section 6).
- `step` prints the seconds the previous step took; the last line prints the total and the
  **counted time** = total − "test environment up" − "build", against the budget: 90 s for `fast`,
  240 s for the full run. Over budget prints `OVER BUDGET` in that line and does not change the
  exit code; it is a finding to fix before the stage closes (master plan section 6).
- Expected: check 30 s, fmt and clippy 5 s, unit 2 s, fast 15 s, slow 30 s, serial 90 s, tshark
  10 s: about 55 s counted for `fast`, about 185 s for the full run. The build step compiles
  two profiles now (dev for the unit tests, release for the rest); it is not counted.
- No single test takes more than 30 s. The longest: S10 on wan-bad (up to about 25 s with its
  `ls /many`) and the two silent-link tests, about 16 s each.

### 6.3 Unit tests (in the source files, no server)

| Test | File | Asserts |
|---|---|---|
| `tokenize_rules` | shell | every rule of 5.15 with one example each; open quote and trailing backslash are `Usage` |
| `ideographic_space_stays` | shell | `cd 東京　大阪` is two tokens |
| `fixture_names_round_trip` | shell | for every name of `CJK_NAMES` and `SPECIAL_NAMES` (read by sourcing `testenv/samba/names.sh` in bash): `tokenize("ls " + quote(name)) == ["ls", name]` |
| `partial_survives_any_cut` | shell | for the same names, `partial` of `"cd " + quote(name)` cut at every character boundary does not panic and returns a prefix of the name |
| `candidates_from_entries` | shell | the pure function that turns entries, a typed directory part, a prefix and the `Arg` into `Pair`s: directories only for `RemoteDir`, files too for `RemotePath` (`/` after a directory, a space after a file), exact-byte prefix, the NFC and NFD Korean names are different candidates, the quoted replacement tokenises back |
| `resolve_paths` | session | `.`, `..`, `..` at the root, a leading `/` and `\`, mixed separators, a trailing separator, `./-f` |
| `classify_statuses` | session | `Protocol` errors with `OBJECT_NAME_NOT_FOUND`, `OBJECT_PATH_NOT_FOUND`, `BAD_NETWORK_NAME`, `ACCESS_DENIED`, `NOT_A_DIRECTORY`, `FILE_IS_A_DIRECTORY`, `OBJECT_NAME_COLLISION`, `DISK_FULL`, `OBJECT_NAME_INVALID`, `LOGON_FAILURE`, and one unlisted status → the kinds of 5.5; `Disconnected` → lost, not silent; `Timeout` → lost, silent |
| `guest_refusal_names_the_flag` | session | `GuestSessionRefused` → kind `auth`, the message contains `-u Guest --no-pass` |
| `exit_codes` | session | every kind, at start-up and later |
| `listing_plan` | session | (8 MiB, 513) → (8,323,072, 2); (64 KiB, 16) → (65,536, 13); (8 MiB, 32) → (1,900,544, 1); (1 MiB, 8192) → (1,048,576, 13); (64 KiB, 4) → (65,536, 1); for each of these and for every credit count from 4 to 600 with both transact sizes: the compound's charge 2 + n × q + 1 ≤ credits (no `max`), and n × b + 65,536 ≤ 16 MiB − 64 KiB. Separately: (64 KiB, 1) → (65,536, 1), the documented case that cannot be funded |
| `query_outcomes` | session | `SUCCESS` → entries; `NO_MORE_FILES` → end; `NO_SUCH_FILE` on the first query → end, on a later one → error; `ACCESS_DENIED`, `INVALID_PARAMETER` → error |
| `wire_paths` | session | components joined with `\`; the root is empty; with a DFS prefix `server\share` for the root and `server\share\a\b` |
| `receive_reports_progress` | session | a local `TcpListener`; the writer sends the 4-byte header and half of a 200,000-byte frame, then waits: while `receive` is still pending, `progress.snapshot()` shows a frame of that length with at least 1 and fewer than 200,000 bytes received; after the rest, `receive` returns the frame and no frame is in progress; `receive_progress()` returns the same `Arc` twice; a peer that closes inside a frame gives `Disconnected` and no frame in progress |
| `entries_match_the_crates_packer` | session | a buffer built with `smb2::msg::directory_info::pack_entries` (class 37; names from the fixtures, a 250-character name, a name outside the BMP, sizes above 2^32, a non-zero file id) parses to the same names, sizes, times, attributes and ids |
| `unpaired_surrogate_is_replaced` | session | a hand-built buffer of three entries, the second with a lone 0xD800: three entries, the second contains U+FFFD, the others are exact |
| `truncated_entry_is_an_error` | session | a name length that runs past the buffer → `Err` |
| `share_list_overflow` | session | `share_pdu` of 65,511 bytes is `Ok`, of 65,512 is `Err` with the message of 5.9 |
| `proxy_urls` | session | with and without credentials, a missing port, a wrong scheme |
| `cache_cap` | session | cap 10: three listings of 4 entries → the oldest is gone, `total` is 8; a listing of 11 is not stored and removes nothing else; a second `put` for a key replaces the first |
| `cache_ignores_case` | session | `put("Share", ["Tree"])`, `get("share", ["tree"])` |
| `filetime` | commands | 0 → none; 116,444,736,000,000,000 → `1970-01-01T00:00:00Z`; 2024-02-29 and 2026-11-17 21:33:05 exact, in both formats; before 1970: 1 → `1601-01-01T00:00:00Z` and 116,444,735,990,000,000 → `1969-12-31T23:59:59Z` (signed arithmetic; the dev profile would panic on a wrap); `u64::MAX` does not panic |
| `ls_line_format` | commands | the text line of 5.12 for a file, a directory, a size of 4,617,089,843 |
| `table_is_complete` | commands | every entry has a summary, a syntax that starts with its name, at least one example whose first token is that name; names are unique; `required <= args.len()` |
| `help_all_has_everything` | commands | the rendered text contains every command's syntax and every example line, the nine parts of 5.14, every exit code, every JSON type |
| `arguments_and_flags` | commands | `cd -rf` and `ls --help.txt` are positional; `--` ends flags; too many arguments is `Usage` |
| `args_are_sound` | main | `Args::command().debug_assert()` (it checks only with debug assertions, which is why the unit tests run in the dev profile); `-p` with `--no-pass` is refused; `--help-all` needs no `-H`; `TESTDOM\tester` splits; every long flag appears in the rendered `help all` |
| `abandon_marks_dead_only_in_flight` | session | on a `Session` built around a connection over a local socket pair: `abandon()` with `in_flight` false leaves `is_disconnected()` false; with it true, the connection is dead and the flag is cleared |

### 6.4 Integration tests

Every test runs the real binary. "tap" = through `smbtap`; "direct" = to the endpoint. Counts are
for one command, taken as in 6.1. The assertion text is what the failure message says.

**`tests/fast.rs`** (fast endpoint, parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| F1 | `startup_is_three_requests` | tap; `client.startup` | "start-up on fast: expected NEGOTIATE 1, SESSION_SETUP 2 and nothing else (3 requests, 3 flights, 1 connection)" | §5 start-up |
| F2 | `use_once_per_share` | tap | `use share` 1 request (TREE_CONNECT), 1 flight; again 0; `use readonly` 1; `use share` 0 | §5 `use` |
| F3 | `free_commands_send_nothing` | tap; after `use share`, `cd tree/d0/d1` | `cd ..`, `cd /tree`, `cd .`, `cd /`, `cd`, `pwd`, `help`, `help cd`, `help all`, `refresh`: "expected 0 requests" each | §5 zero rows |
| F4 | `cd_is_one_compound` | tap; `cd tree/d0/d1`; `cd /cjk/<IDEO_SPACE>`; `cd "/special/dir with space/Grüße"` | each: 2 requests, 1 flight, the log is CREATE (chain 0 of 2) and CLOSE (chain 1 of 2); `pwd` shows the path | §5 `cd` |
| F5 | `cd_answers` | tap; `nope`, `sizes/1.bin`, `forbidden`, `traverse-only` | kinds `not_found`, `not_a_directory`, `access_denied`, ok; `pwd` unchanged after each failure; each 2 requests in 1 flight | 1.2 |
| F6 | `ls_equals_the_server` | tap; `sizes`, `many`, `longnames`, `cjk`, `special` against `docker exec … ls -A`; `mangled` | names equal as byte strings and as many `entry` lines as names; in `sizes/` the size of `<n>.bin` is n; `cjk/nested` and `special/dir with space` have `is_dir`, files have not; `mangled` has 9 entries matching `[A-Z0-9_]{6}~[A-Z0-9](\.[A-Z0-9]{1,3})?`; every `mtime` parses; each `ls`: 5 requests (CREATE 1, QUERY_DIRECTORY 3, CLOSE 1), 1 flight; the entries are sorted | 1.3, §5 `ls` |
| F7 | `ls_text` | direct, text mode, `-C` | `ls sizes` prints 11 lines of the format of 5.12, sorted; `ls cjk` shows `nested/`; stderr empty | 1.3 |
| F8 | `ls_denied` | tap; `forbidden`, `traverse-only` | `access_denied` both; the next `ls sizes` in the same session works; one-shot `-C 'use share' -C 'ls forbidden' -C pwd` exits 1 and prints no `pwd` | 1.4 |
| F9 | `file_ids_follow_links` | tap; `ls /` and `ls /loops` | `self` has the `file_id` of `loops` in the root listing; `up` has an id that is non-zero and differs from it; `dangling` is not listed | (S2-6, early) |
| F10 | `logins` | direct, one-shot | user, password, domain: `ls` of `share` works; `-u 'TESTDOM\tester'` without `-d` works; `-u Guest --no-pass` and no `-u` at all: `use guest`, `ls` shows `hello.txt`, `use share` is `access_denied`; wrong password: exit 3, kind `auth`; `-u nosuchuser -p x`: exit 3, the message contains `-u Guest` | 1.1 |
| F11 | `shares_fast` | tap | `share`, `readonly`, `guest`, `encrypted`, `tiny`, `IPC$` present; `IPC$` is `ipc` and hidden; first call 7 requests, second 6, no TREE_DISCONNECT, one TREE_CONNECT in all | 1.6, S1-9 |
| F12 | `encrypted_share` | tap | `use encrypted`: 1 request; `cd tree`: 0 named requests, 1 opaque frame, 1 flight; `ls sizes`: names equal, 0 named requests, 1 opaque frame | 1.8 |
| F13 | `reconnect_after_idle_cut` | tap; `use share`, `cd tree/d0`, then `tap.drop_connections()`, 200 ms | first the commands that send nothing: `pwd`, `help`, `cd ..`, `refresh`: "expected 0 requests and 0 new connections on a dead connection" each, and each answers within 1 s; then `cd d0` and `ls`: `ls` shows the 5 names of `tree/d0`; over the two "expected exactly 1 new connection and 11 requests (NEGOTIATE 1, SESSION_SETUP 2, TREE_CONNECT 1, CREATE 2, CLOSE 2, QUERY_DIRECTORY 3)"; `pwd` as before | 1.9, finding 3 |
| F14 | `counts_add_up` | tap; the script of X1 once with a read after every command, once in a second session with one read at the end | the sum of the per-command requests equals the second session's total; the same for flights | §5 method |
| N1 | `no_password_no_prompt` | one-shot with stdin `/dev/null`; again under `ptyrun.py` with `sh -c '… < /dev/null'` | exit 2 within 2 s, kind `usage`; the terminal shows no `assword` | 1.11 |
| N2 | `ways_to_give_commands` | direct | `-C` twice, `-S <file>`, piped stdin: the same three stdout lines; `-S` runs before `-C`; with `-C` given and a stdin pipe that is never closed, rsmb exits on its own within 5 s | 1.11, S1-7 |
| N3 | `stop_at_first_failure` | direct | `-C 'use share' -C 'cd nope' -C pwd`: exit 1, no `pwd` line; with `-k`: the `pwd` line is there, exit 1; unknown command: exit 2, kind `usage`; `-N` alone on a terminal (ptyrun): exit 2 | 1.11 |
| N4 | `every_json_line_has_a_type` | direct; every command once, a failing one, `help all` | every stdout line parses as an object; its `type` is in the `json_object` list of `--help-all --json`; each command line ends with exactly one `done`; stderr is empty | 1.11 |
| N5 | `startup_failures` | one-shot | port 1 on 127.0.0.1: exit 3, kind `connect`, within 2 s; `--proxy http://x`: exit 2; `-S /nonexistent`: exit 2 | 1.11 |
| N6 | `closed_stdout_is_not_a_panic` | `--help-all` with the read end of stdout closed at once | the exit code is not 101 and stderr has no `panicked` | — |
| H1 | `help_all_needs_no_server` | `rsmb --help-all`, no `-H` | exit 0; every command name; `--json` variant parses | 1.12 |
| H2 | `docs_match_the_table` | `--help-all --json` against `docs/commands.md` | for every `command`: a line that is exactly ``## `<name>` ``, the syntax and every example line occur in the file; every such heading in the file names a command; for every `option`: its long flag occurs in the file | 1.12 |
| H3 | `examples_run` | direct; for every example of every command a fresh session, `use share`, then the line | `done` with `ok: true` (for `exit`: exit code 0) | 1.12 |
| H4 | `help_in_a_session` | tap | `help`, `help cd`, `help all`: 0 requests; the objects of `help all` (without its `done`) equal the stdout lines of `--help-all --json`; `help nosuch`: `usage` | 1.12 |
| I1 | `interactive_basics` | pty through the tap | the prompt `//127.0.0.1> ` appears and the start-up was exactly 3 requests; 1 s later the tap has seen 0 more; `use share` → prompt `//127.0.0.1/share/> `; `cd tree` → `//127.0.0.1/share/tree> ` and only those commands' requests; Up shows `cd tree`; Ctrl-C gives a new prompt and the process lives; `exit` → code 0; a second process: Up shows the last line of the first (history file), Ctrl-D → code 0 | 1.13 |
| I2 | `password_prompt` | pty, no `-p` | `Password for TESTDOM\tester:` appears, the typed password is not echoed, the prompt follows | 1.1 |
| I3 | `names_through_the_shell` | pty; `cd cjk`; for each of the nine names: `cd `, the name without its last character, Tab, Enter; and `cd /special`, `cd dir`, Tab | the line shows the whole name and `/`; the prompt then ends in that name, byte for byte; `ls` there prints `<name>.txt`; the name with U+3000 is one argument; the NFC name, typed as its first character plus Tab, lands in the NFC directory and the NFD one in the NFD directory; `dir` completes to `dir\ with\ space/` | 1.5 |
| I4 | `completion_costs` | pty through the tap, `use share` | `sh` Tab → `shares `: 0 requests; `ls many/f1` Tab → `f10000`: 5 requests, 1 flight; Ctrl-U, `ls many/f0999` Tab → BEL: 0 requests; `refresh`, then the same Tab: 5 requests; `ls tree`, then `cd tree/d` Tab → BEL: 0 requests; `use rea` Tab → `readonly `: 7 requests (the share list, P5); Ctrl-U, `use enc` Tab → `encrypted `: 0 requests | §5 Tab, `refresh` |
| I5 | `idle_sends_nothing` | pty through the tap, at the prompt after `use share`, 12 s | "expected 0 requests in 12 s idle" | §5 idle |

**`tests/slow.rs`** (slow endpoint, edge, wan-bad, SOCKS; parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| S1 | `startup_and_use_slow` | tap, slow | F1 and F2 at the slow endpoint | §5 |
| S2 | `cd_slow` | tap, slow | F4 at the slow endpoint | §5 `cd` |
| S3 | `ls_small_slow` | tap, slow, `ls sizes` | 5 requests, "expected 1 flight (f = 1)" | §5 `ls` small |
| S4 | `ls_many_slow` | tap, slow | 1 flight (the master plan's limit is 4); 10,000 names equal the server's | §5 `ls many/` |
| S5 | `ls_huge_slow` | tap, slow | 1 flight (limit 7); 100,000 names equal the server's; `peak_rss_kb` after the `done` line ≤ 150 MB | §5 `ls huge/` |
| S6 | `completion_costs_slow` | pty through the tap, slow | `ls many/f1` Tab → `f10000`: 5 requests, 1 flight; Ctrl-U, `ls many/f0999` Tab → BEL: 0; `ls huge/f1` Tab → `f100000`: 5 requests, 1 flight; Ctrl-U, `ls huge/f09999` Tab → BEL: 0; `ls tree`, then `cd tree/d` Tab → BEL: 0 | §5 Tab |
| S7 | `edge_navigation` | tap, edge; then pty through the tap | `cd many`: 2 requests, 1 flight; `ls many`: names equal those in `rsmb-samba-edge`, at most 4 flights and 44 requests, done within 30 s; `ls sizes`: 1 flight, 16 requests; pty: `ls many/f1` Tab → `f10000`, a second completion 0 requests | 1.7 |
| S8 | `shares_edge` | tap, edge; then pty | 306 shares, 300 starting with `edge`, 31 hidden, `IPC$` is `ipc`, a comment present; pty: `shares`, then `use edge300` Tab → `edge300$ `: 0 requests | 1.6 |
| S9 | `socks5_smoke` | `--proxy socks5://10.213.45.20:1080 -H fileserver.rsmb.test`; `docker logs rsmb-socks` before and after | `use share` and `ls` work; exactly one more `connected to fileserver.rsmb.test:445` line | 1.14 |
| S10 | `wan_bad_smoke` | direct, 10.213.45.31, default timeout | `use share`, `cd tree`, `ls` succeed; then `ls /many` (one reply frame of 1.2 MiB, 5 to 17 s when measured): 10,000 entries; the four times are printed | 1.15, finding 2 |
| S11 | `interrupt_non_interactive` | tap, slow; `ls huge`, and when the tap has seen its first QUERY_DIRECTORY, `kill -INT`; again with `-TERM` | an `error` of kind `interrupted`, exit 130, within 2 s of the signal | P7 |
| S12 | `interrupt_interactive` | pty through the tap, slow; `ls huge`, Ctrl-C when the tap has seen the first QUERY_DIRECTORY. A second run on edge with the same steps (there the listing holds all 16 credits) | `interrupted`, the prompt returns; then `ls sizes` lists its 11 names within 3 s of Enter, and `cd tree` works; "expected exactly 1 new connection after the interrupt"; `pwd` unchanged | P7, §3, finding 5 |
| S13 | `cut_during_a_listing` | tap, slow; `ls huge`, and when the tap has seen its first QUERY_DIRECTORY, `drop_connections()` | the command succeeds with 100,000 entries; exactly 1 new connection; "expected the listing's CREATE and QUERY_DIRECTORY in the request log on the first connection and again on the second" (two different `conn` ids, each with CREATE 1 and QUERY_DIRECTORY ≥ 1) | 4.9, finding 7 |

**`tests/serial.rs`** (one at a time, exclusive lock)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| X1 | `one_connection_fast` | `ctl conns-reset samba`; one session through the tap: `pwd`, `shares`, `use share`, `cd tree/d0`, `ls`, `cd /cjk/<KO_NFC>`, `ls /many`, `refresh`, `help all`, `use readonly`, `ls`, `use share`, `cd ..`, `shares`, `exit` | "expected 1 TCP connection in the tap and 1 on the server (`ctl.sh conns samba`)" | §5 connection |
| X2 | `one_connection_edge` | the same on edge, `ctl conns samba-edge` | the same | §5 connection |
| X3 | `silent_link_blackhole` | direct to 10.213.45.30:4450, `--timeout 5`; `use share`, `cd tree`, `ls` (answered), then at once `ctl blackhole on` and `ls` | the second `ls` fails with kind `connection_lost`, "expected between 13 and 20 s after the request (the server's last frame was under 2 s old)"; `pwd` right after: 0 s, unchanged; after `blackhole off` the next `ls` works | 1.10, D11 |
| X4 | `silent_link_paused_shell` | pty, fast, `--timeout 5`; `use share`, `ls sizes` (answered), then at once `ctl pause samba` and `ls` | `ls` prints a `connection lost` message 13 to 20 s after Enter and the prompt returns; after `unpause` the next `ls` lists | 1.10, D11 |
| X8 | `tab_on_a_silent_link` | pty, fast; `use share`; `ctl pause samba`; `cd tr` Tab (the root is not cached) | the BEL comes between 4 and 8 s after the Tab, the line still reads `cd tr`, and typing works; Ctrl-U; after `unpause`, `ls sizes` lists | finding 6 |
| X5 | `timing_cd` | the 10 measured lines are `cd /tree/d0`, `cd /tree/d1`, `cd /tree/d2`, `cd /tree/d3`, `cd /tree/d0`, … : every target is a sibling of the working directory, so every one is checked on the server. First the same lines once through the tap, slow: "expected 20 requests in 10 flights for 10 cd". Then direct: `(t(10) − t(0)) ÷ 10`, median of 3 pairs, slow and fast; `connect_ms` just before | slow: "≤ 1.25 × connect time"; fast: "≤ 20 ms"; both printed beside the baseline (1610.1 ms) | §5 wall times, finding 1 |
| X6 | `timing_ls_small` | set-up line `cd /tree/d0` in both runs, then `ls` × 10, slow. First once through the tap: "expected 50 requests in 10 flights for 10 ls". Then direct as X5 | "≤ 1.25 × 1 × connect time"; printed beside 1207.9 ms | §5 wall times |
| X7 | `timing_ls_large` | slow: `ls` in `many/` (N = 10, one pair) and in `huge/` (N = 1, one pair) | printed beside 4535.4 and 37320.2 ms; asserts only success, the entry counts and that `ctl drops` did not change during X5–X7 | §5 report |

The 16 rows of master plan section 7: 1.1 F10, I2 · 1.2 F5 · 1.3 F6, F7 · 1.4 F8 · 1.5 I3 and
the unit tests of the tokeniser · 1.6 F11, S8 · 1.7 S7 · 1.8 F12 · 1.9 F13 · 1.10 X3, X4 ·
1.11 N1–N5 · 1.12 H1–H4 · 1.13 I1 · 1.14 S9 · 1.15 S10 · 1.16 `cache_cap`. Tests that exist
because of the adversary's findings: `receive_reports_progress`, `query_outcomes`, `wire_paths`,
`abandon_marks_dead_only_in_flight`, X8, the `ls /many` of S10, and the added assertions in F13,
S12, S13, X3–X6. There is no gate test that shows a slow reply surviving end to end: on wan-bad
`many/` arrives in 5 to 17 s, too close to both limits (15 s without progress, 6 × `--timeout`
with it) to give a test that neither passes blindly nor fails by chance, and `huge/` takes two
minutes. The unit test covers our half; the bench row of 6.5 covers the whole.

### 6.5 `testenv/bench-rsmb.sh`

`testenv/bench-rsmb.sh [quick]`: the rows and the method of `testenv/bench-smbclientng.sh`, for
`target/release/rsmb`; not part of the gate.

- Shared lock for the whole run; `ctl.sh drops` before and after, non-zero difference fails.
- Rows at the fast and the slow endpoint: `cd` (N = 10: `cd /tree/d0`, `/tree/d1`, `/tree/d2`,
  `/tree/d3` in turn, because a repeated `cd` to the working directory sends nothing in rsmb and
  would be timed as free; smbclient-ng's row repeated `cd /tree/d0`, which it checks on the
  server every time, so the two rows measure the same thing: one checked `cd`), `ls` in `tree/d0`
  (N = 10), `ls` in `many/` (N = 10), `ls` in `huge/` (N = 1; not in `quick`). 5 runs per row
  (`quick`: 1).
- A row is void unless its tapped run shows the requests it must: 2 × N for `cd`, 5 × N for an
  `ls` on Samba. The script stops with a message otherwise, so a free command cannot be
  published as a fast one.
- One more row, not in `quick`: `ls` in `huge/` on wan-bad (10.213.45.31), once, default
  timeout, with the time and the entry count: the end-to-end check that a slow reply is not
  taken for silence. The adversary measured 119.6 s with receive progress and a torn-down
  connection after 30 s without; the limit with progress is 180 s (5.10). It is in the bench and
  not in the gate because of its length.
- One run is `rsmb -H <host> … -N -C 'use share' [-C <setup>] -C <cmd> × N`, text mode, output
  to a file. A run counts only if it exits 0 and the file has N × entries listing lines.
- Per row: the connect time; ms per command = median of (t_N − t_0) ÷ N; connections, requests
  (named plus opaque) and flights per command from one 0-run and one N-run through `smbtap`.
- Two more columns at the slow endpoint for the `ls` rows: **fresh** = t_1 − t_0 (the first
  listing of a new connection), and **paused** = the N commands fed over stdin with `sleep 1`
  before each, (t_N − t_0 − N s) ÷ N.
- The smbclient-ng columns are read from `testenv/baseline-smbclientng.txt`, which holds the
  output of the last full baseline run (to begin with, the table of 2026-11-17 from
  `docs/measuring.md`); `./testenv/bench-smbclientng.sh | tee testenv/baseline-smbclientng.txt`
  renews it (18 minutes).
- Output: one table, both clients side by side, with the Samba version, the date and the run
  time. The implementing agent runs it at the end of the stage and puts the table into
  `docs/performance.md`.

Expected from section 2.1: `cd` about 0.2 s against 1.61 s; `ls` small 0.2 s against 1.21 s;
`many/` 0.3 s warm and 1.5 s fresh against 4.5 s; `huge/` 1.7 s warm and 3.1 s fresh against
37.3 s; 5 requests in 1 flight for each `ls` against 5, 17 and 139 flights.

## 7. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` runs after T5, T7
and T9. Documentation is written with the task that makes it true (AGENTS.md rule 1), and
`docs/commands.md` grows with every command because H2 fails otherwise.

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T0 | Skeleton | — | the workspace change; `Args` and `--help-all` printing the flag list; `Out`; `./test.sh` as in 6.2; the three test files with the harness's `run` and `tmp`; `testenv/ptyrun.py` | `args_are_sound`; H1 (first form); N6 |
| T1 | Connect | T0 | `Target`, `parse_proxy`, the dialer (both variants), **the receive half with its progress counter**, login, `Error`, `Kind`, `classify`, exit codes, the password rules, signals and `run_blocking` (the `Notified` before `BUSY`), the runner with `-S`, `-C`, stdin, `-N`, `-k`, the commands `pwd` and `exit` through the table; `Client` in the harness | `classify_statuses`, `guest_refusal_names_the_flag`, `exit_codes`, `proxy_urls`, `receive_reports_progress`; F1; F10 (exit codes and kinds; the share checks follow in T4); N1, N2, N5 |
| T2 | Tokeniser, table, help | T1 | `tokenize`, `quote`, `partial`; flags and positionals; `help` in its three forms, text and JSON; `docs/commands.md` begun | the tokeniser tests, `table_is_complete`, `help_all_has_everything`, `arguments_and_flags`; H1, H2, H4; N4 (grows with every command) |
| T3 | `use`, `cd` | T2 | `tree` with `ensure_connected` inside it, `wire_path`, `resolve`, `use`, `cd` | `resolve_paths`, `wire_paths`; F2, F3, F4, F5; N3; S1, S2 |
| T4 | Listing | T3 | `plan`, `parse_entries`, `query_outcome`, `list_dir`, `Session::list`, `ls` in both modes, the time formatting; **the confirmation of the listing shape through the product and its table in `docs/library-decision.md`** (rule of S1-1) | `listing_plan`, `query_outcomes`, the three parser tests, `filetime`, `ls_line_format`; F6, F7, F8, F9, F10 (complete), F12; H3; S3, S4, S5, S7 (the part without a pty), S9 |
| T5 | Cache, `refresh`, reconnect, abandon | T4 | `Cache`, `list_cached`, `refresh`, the retry of 5.10, `mark_dead`, `in_flight` and `abandon` | `cache_cap`, `cache_ignores_case`, `abandon_marks_dead_only_in_flight`; F13; S11, S13; X3 |
| T6 | `shares` | T3 | the share list, `share_pdu`, the kept list | `share_list_overflow`; F11; S8 (the part without a pty) |
| T7 | Interactive shell | T5, T6 | the editor, prompt, history, the completer with its 5 s bound, the password prompt, the login line; `Pty` in the harness | `candidates_from_entries`, `partial_survives_any_cut`; I1–I5; S6, the pty parts of S7 and S8, S12; X4, X8 |
| T8 | The remaining rows | T7 | nothing new in `src/` unless a test shows a defect | F14; X1, X2, X5, X6, X7; S10 |
| T9 | Bench and documents | T8 | `testenv/bench-rsmb.sh` (with the wan-bad row), `testenv/baseline-smbclientng.txt`; the documents of section 8; the final full gate and the budget line | — |

If T4's confirmation fails: apply the fallback of S1-1, change the flight numbers in F6, S3–S7,
I4 and the documents, and say so in `docs/library-decision.md` and in the report.

## 8. Documents

New, each added to the index in `AGENTS.md` in the same change:

| Document | Outline |
|---|---|
| `docs/architecture.md` | The four source files and what each holds. Threads and the runtime; signals. The session: dialer, login, tree map. `cd`: the compound, the access mask, when nothing is sent. Listing: `plan`, the two phases, the parser, what it costs per server, memory. Cache: key, cap (500,000 entries, about 100 bytes each), what fills and empties it. Completion and the tokeniser. The receive half and why it reports progress. Timeouts (the two limits: silence, and 6 × `--timeout` for one reply), dead connections, when a connection is marked dead and when it is replaced, closed against silent, interruption and `abandon`, the completer's bound. That a share which encrypts switches the whole connection. Errors and exit codes. What Stage 2 adds where |
| `docs/commands.md` | Start-up flags (a table). How commands are given. Quoting and paths. One section ``## `name` `` per command: syntax, what it does, what it costs in requests, examples, errors. Timeouts (a silent server: between `--timeout` and max(`--timeout`, 15 s); one reply: at most 6 × `--timeout`) and reconnect. Under `use`: an encrypting share encrypts the rest of the session. Under completion: the 5 s bound and that Ctrl-C does not interrupt a Tab. Differences from smbclient-ng (`pwd`, `refresh`, exit codes, no probe, `ls` shows UTC, `-k`) |
| `docs/scripting.md` | The three ways to give commands and their order; when stdin is read; `-N`, `-k`; never prompting. `--json`: every object type with its fields and an example line; the `done` rule; how to drive rsmb as a coprocess. Exit codes. Two worked examples (bash, python) |
| `docs/development.md` | Build and test commands. Where a new command goes (5.11). Where `smb2` may be used. The pin policy in one paragraph. How to run one test, how to read a failure. The scratch area for throwaway measurements |
| `docs/testing.md` | The three test binaries and what goes where. The harness (`Client`, `Reply`, `Pty`, `ptyrun.py`). How per-command counts are taken. The lock. The budget and the counted time. A table of every test: what it protects and which target it enforces |
| `docs/performance.md` | The targets of master plan section 5 with the measured value of each. The navigation table from `bench-rsmb.sh` beside the baseline, with fresh and paused columns. What start-up and `shares` cost. Listing memory. The method. What was not done (compounded share list) |

Extended:

| Document | What is added |
|---|---|
| `docs/library-decision.md` | A section "Listing shape (Stage 1)": the candidates, the rule, the table measured through the product, the choice; the sentence "A directory listing is not compounded" under the library behaviours gets its Stage 1 answer |
| `docs/known-limitations.md` | `--timeout`: a silent server is noticed after between `--timeout` and max(`--timeout`, 15 s); one reply must be complete within 6 × `--timeout`, so a very large directory over a very slow link needs a larger `--timeout` (numbers from the wan-bad bench row). A silent link is not retried inside the command. After Ctrl-C during a command that was on the wire, the next command reconnects. Tab in a directory that is not cached gives up after 5 s and forces a reconnect; Ctrl-C does not interrupt a Tab. After `use` of an encrypting share everything on the connection is encrypted until it is replaced. DFS: paths on a DFS share get the server and share prefix, untested against a real DFS share; links are not followed. A server that never grants 4 credits cannot be listed. Guest needs `-u Guest`. The share list limit (65,511 bytes of stub, about 780 shares). Names with an unpaired surrogate are shown with U+FFFD and cannot be opened. The working directory keeps the typed spelling. `ls` of a file is an error. Times are UTC. A password given with `-p` is visible in the process list. Cache keys fold case with Rust's tables, not the server's. Servers that refuse class 37 or the listing compound cannot be listed; only Samba was tested. No IPv6 literal in `--proxy` |
| `docs/test-environment.md` | "Tests": the two modes and their steps; the lock table gains the rows for `tests/fast`, `tests/slow` (shared), `tests/serial` (exclusive) and `bench-rsmb.sh` (shared); `ptyrun.py` (python3 is a host requirement already) |
| `docs/measuring.md` | A pointer to `bench-rsmb.sh` and the baseline file |
| `docs/reviews.md` | The Stage 1 section, after the review |
| `README.md` | Status: Stage 1. How to start rsmb, three example lines. The navigation numbers. The Samba-only sentence stays |
| `AGENTS.md` | The index rows; "Working commands": the two modes of `./test.sh` with their new meaning and times, `bench-rsmb.sh`, how to run rsmb against the test server; principles, **only those the work has shown** (candidates with evidence from section 2.2: install signal handlers before the line editor takes the terminal; do not name a helper script after a standard module; measure the candidate before writing the plan around it) |

## 9. What Stage 2 gets, and notes for it

- `Session` with `tree` (which reconnects) and the tree map; `list_dir` (make it `pub` and give the walk
  its own `(b, n)`: 16 listings at once must fit the credits, master plan 4.10); `Entry` with
  `file_id`; `Cache` (add `invalidate`); the table, `run_line`, `Out`; the harness and the three
  test binaries.
- File ids (section 2.2): a symlinked directory carries its target's id; the share root's `..`
  has id 0, so the walk's seed for the parent of a walk that starts at the root is absent, not 0;
  `list_dir` drops `.` and `..` today and has to return their ids to the walk.
- `Arg` gets `LocalPath`; the table gets commands with `rerun: false`.
- The crate's `Tree` methods (`download`, `create_file_writer`, …) run every path through
  `smb2::name::encode_path`, which replaces characters that are illegal in SMB names with
  private-use code points. For every name a server lists this is the identity, so Stage 1's raw
  paths and Stage 2's crate calls agree; for a name the user types with `:` or `?` in it they
  would not. Stage 2 decides whether `cd` and `ls` adopt the mapping.
- A SIGTERM at the interactive prompt exits at once (5.3); with temporary files that is still
  safe only because no command is running then.
- Interrupted work: Stage 1 answers S2-3 for its own commands by replacing the connection at
  once (`abandon`, 5.10) instead of waiting. A transfer has more to do before that (remove the
  temporary file), and needs a connection to do it on: the new one.
- Every method that sends must start with `tree()` and set `in_flight`; a transfer that holds a
  clone of the connection across a reconnect holds a dead one.
- One reply must arrive within 6 × `--timeout` (5.10). The walk's smaller buffers (1 MiB) keep
  each reply small; a listing for `ls` does not.

## 10. Assumptions this plan could not verify

Marked so that the adversary can attack them. Each says what would show it wrong.

1. **The wall-time rows hold for the binary.** 201 ms per `cd` and per small `ls` were measured
   on the raw API; X5 and X6 measure the process with the (10 − 0) ÷ 10 method. Process start,
   JSON or text output and the tree connect are expected to cancel out or stay under 50 ms.
   Shown wrong by X5/X6.
2. **`conn.credits()` is what a compound may spend at once.** A compound charging 257 of 513
   (Samba) and 16 of 16 (edge) went out without waiting. Not tested: a server that grants fewer
   credits than it advertises buffer, or one that never grants 4 (then nothing can be listed,
   5.7). `plan` reads the credits before every listing, and an abandoned request no longer
   leaves the next one short, because the connection is replaced (5.10). Shown wrong by a
   listing that fails with the crate's credit error on a live connection.
3. **Servers other than Samba accept the listing compound.** Related compounds with several
   multi-credit QUERY_DIRECTORY requests, a later query answered `NO_MORE_FILES`, class 37: all
   measured on Samba 4.23.8 only. A Windows server may answer differently; rsmb has no run-time
   fallback. Only a run against Windows (master plan section 15) can show it.
4. **A compound reply close to 16 MiB is handled.** The largest reply measured was `huge/`,
   about 11.4 MiB in one frame. `plan` keeps n × b + 64 KiB at or below 16 MiB − 64 KiB, which
   leaves 64 KiB for the headers of the frame, but no fixture fills it; `xl/` (opt-in) would, and
   would also run the second phase with 8 MiB buffers. The second phase was measured with 64 KiB
   (edge), 2 MiB and 4 MiB buffers.
5. **The receiver task always sees a close before the next command.** Measured once for a FIN
   from the tap after 200 ms. If a command starts in the gap, it fails with `Disconnected` and is
   run again (reading commands), which F13 would not distinguish. A reset instead of a FIN was
   not tested.
6. **After a silent link or an interrupt, `mark_dead` plus a new connection is enough.** X3, X4,
   X8 and S12 test it. Not
   measured: how long the crate's tasks of the abandoned connection live, and whether the old
   socket is closed at once when the `Connection` is dropped (the source says the tasks are
   aborted on drop).
7. **rustyline behaves on a real terminal as on the test pty.** The pty has 200 columns and
   `TERM=xterm`; narrow terminals, wide characters at the line end and terminal multiplexers were
   not tried.
8. **`String::to_lowercase` is good enough for cache keys.** Two directories that differ only in
   case (possible on Samba's disk) share a cache entry; `ls` always asks, so only completion can
   show the other one's names.
9. **The budget.** The step times of 6.2 are estimates from the request counts and the Stage 0
   timings, not measurements of tests that do not exist yet.
10. **`ls` output of 100,000 lines through a pty** (S12 interrupts it; no test prints it whole)
    is fast enough not to matter; not measured.
11. **The tap's `settle` with 300 ms of quiet is enough on the slow endpoint**, because
    outstanding requests keep it waiting and the client's own reaction time is local. Stage 0
    used the same value; a client that waits longer than 300 ms between two requests of one
    command would be charged to the next command. rsmb has no such wait by design.
12. **Guest and anonymous sessions can list `IPC$`** (not tried while planning); F10 does not
    run `shares` as guest.
13. **5 s is the right bound for a completion.** It is above every listing measured at the slow
    endpoint (3.1 s for `huge/` on a fresh connection) and below it on wan-bad for anything
    large (`many/` 5 to 17 s). Not measured: how often it cuts off a completion that would have
    come on real links.
14. **The DFS prefix.** Written after the crate's own `format_path`; no fixture has a DFS share,
    so nothing but the unit test of the string exercises it.
15. **`NO_SUCH_FILE` for an empty directory** is what the adversary reports for servers without
    `.` and `..` entries; Samba never sends it, so only the unit test of `query_outcome` covers
    it.
16. **`in_flight` catches every abandoned request.** It is set by hand in each method that
    sends; a method that forgets it would leave an interrupted request on a live connection.
    S12 covers `ls`; nothing covers an interrupt during `shares` or `use`.
17. **6 × `--timeout` is enough by default.** `huge/` on wan-bad took 119.6 s against 180 s. A
    directory near the 15.9 MiB of one compound on such a link would not make it; the answer in
    Stage 1 is the message that names `--timeout`, not a smaller buffer.

## 11. Adversary findings and what changed

One adversary reviewed the first version of this plan; the orchestrator validated all twelve
findings against the plan text, the sources of `smb2` 0.43.0, clap and rustyline, and the
adversary's probes (`spikes/smb2/target/adv-stage1/`, not committed). All twelve are accepted;
in two a part is rejected or corrected, with the evidence.

| # | Finding | Evidence | What changed |
|---|---|---|---|
| 1 | The `cd` timing row repeats `cd /tree/d0`; after the first, the target is the working directory and nothing is sent, so the row measures a free command and its bound cannot fail | 5.6 step 2 of this plan | X5 and the bench row alternate `cd /tree/d0` … `/tree/d3`, so every `cd` is checked on the server; X5 and X6 first run their lines through the tap and assert 2 × N and 5 × N requests; the bench voids a row whose tapped run does not show the requests (6.4, 6.5). X6's set-up `cd` is the same in both of its runs and needed no change |
| 2 | The receive half reported no progress, so a large one-frame reply that arrives slower than the deadline is taken for silence and the connection torn down | `transport/mod.rs`: `receive_progress` returns `None` by default; probe on wan-bad, `ls huge`: "nothing on the wire for 30.03 s" without, ok after 119.6 s with | The receive half holds an `Arc<ReceiveProgress>`, records every read and returns it (5.4); unit test `receive_reports_progress`; S10 lists `many/` on wan-bad; a wan-bad `ls huge` row in the bench. **Correction found while revising:** progress does not lift the limit, it moves it. The crate extends one request's deadline to at most 6 × `--timeout` (`ALIVE_DEADLINE_FACTOR`; a waiter's clock restarts only when it is sent or on `STATUS_PENDING`). Measured for this revision on wan-bad with progress: `ls huge` with `--timeout 5` failed with `TimedOut` after 34.0 s; `ls many` took 5 to 17 s. So one reply must be complete within 6 × `--timeout` (180 s by default); this is stated in 5.10, in the error message, in the documents and as assumption 17. The gate test proposed for it was dropped: no fixture gives a reply on wan-bad that is reliably slower than 15 s and faster than the ceiling (6.4) |
| 3 | `ensure_connected` before every command made `help`, `pwd`, `refresh`, `exit` and every Tab log in again on a dead link, blocking 10 to 15 s | 5.10 of the first version | `ensure_connected` is private and called only by `tree`, which only methods that send call; a cache hit in the completer sends nothing (5.4, 5.10, 5.15). F13 asserts 0 requests and 0 connections for `pwd`, `help`, `cd ..`, `refresh` after a cut, then 1 connection for the commands that send |
| 4 | When `mark_dead` is called was ambiguous; the crate reports `ConnectionLost` with the socket still open in several cases, and the rerun would have reused the broken session | `error.rs`: `NETWORK_NAME_DELETED`, `USER_SESSION_DELETED`, expired session, I/O error, invalidated handle | 5.10 names the three places; `run_line` calls `mark_dead` on any `ConnectionLost` before the rerun and after it |
| 5 | An interrupted command leaves its credits and its reply in flight; the crate refuses to wait for credits when nothing is outstanding; the unit test's `max(credits, 4)` hid a plan that asks for more than there is | `connection.rs` `reserve_credits`; probe: after abandoning a listing that held 511 of 513 credits the next listing failed at once, credits back after about 3.5 s; on edge all 16 are held; otherwise the next `ls sizes` waited 3.07 s | `in_flight` and `abandon`: after an interrupt with requests on the wire the connection is marked dead and the next command that sends reconnects (5.3, 5.10, D10). S12 asserts `ls sizes` within 3 s after Ctrl-C, on the slow endpoint and on edge. `listing_plan` asserts charge ≤ credits for every credit count from 4 up, and 5.7 says what happens below 4 |
| 6 | Ctrl-C during a Tab completion is only a buffered key, because the editor clears ISIG; a Tab in an uncached directory on a silent link froze the terminal for 15 to 30 s | rustyline's raw mode | The completer's network work is bounded by 5 s, then `abandon`, no candidates, a beep (5.10, 5.15, D9); test X8; documented |
| 7 | S13 could pass without the listing having been on the wire when the connection was cut | the fixed 150 ms sleep | S11, S12 and S13 act when the tap has seen the listing's first QUERY_DIRECTORY (`Client::wait_requests`); S13 asserts the listing's CREATE and QUERY_DIRECTORY on two different connections |
| 8 | Unit tests built with `--release` make clap's `debug_assert` empty and switch overflow checks off | `cfg(debug_assertions)` | The unit-test step runs in the dev profile (6.2, P6); `filetime` has cases before 1970 and `u64::MAX` |
| 9 | `BUSY` was set before the `Notified` future existed; an early Ctrl-C was lost | `notify_waiters` stores no permit | `run_blocking` creates and enables the `Notified` first, then sets `BUSY` (5.3) |
| 10 | Other servers: an empty directory without `.` and `..` answers the first query with `NO_SUCH_FILE`; on a DFS tree raw paths lack the server and share prefix; a fallback to the two-flight shape on `INVALID_PARAMETER` or `NOT_SUPPORTED` | MS-SMB2; `tree.rs` `format_path` | **Accepted:** `NO_SUCH_FILE` on the first query is an empty listing (`query_outcome`, unit-tested). **Accepted:** DFS trees get the prefix, as the crate writes it (`wire_path`, unit-tested; unverified against a DFS share, assumption 14); refusing such shares would lock out namespace roots for no gain. **Rejected:** the fallback shape. The two-flight shape is also a compound with queries, so it does not help against a server that refuses queries in a compound, and no server is at hand that shows which status means what; the error is reported with the server's status, and it stays assumption 3 and a known limitation until a run against Windows |
| 11 | "15 s floor" is not a floor: after 15 s of server silence before the request, `--timeout 5` returns after 5 s | `connection.rs`, the deadline loop | Reworded everywhere as "between `--timeout` and max(`--timeout`, 15 s)" (5.2, 5.10, D11, section 8). X3 and X4 now get an answered command immediately before the fault, and expect the failure 13 to 20 s after the request: the 15 s count from the server's last frame, which is a fraction of a second older than the request, so 15 as a lower bound would fail by chance |
| 12 | Points the implementer would have had to guess | the plan text | `tree()` returns `Arc<Tree>` (5.4); `conn.params()` is an `Option` (5.7); every `Client` sends `pwd` itself and keeps the start-up cost apart, so F2 cannot count it into `use` (6.1); `finish`, `run` and every other wait have a timeout; `tmp` wipes a test's directory, so no history survives a run; an encrypting share switches the whole connection, said in 5.4 and in the documents, and only F12 selects it |

Changed beyond the findings: the start-up `pwd` moved from the tests into the harness; the fixed
sleeps before a cut or a signal were replaced in S11 and S12 too, not only in S13; the error for
a response timeout names `--timeout`; assumptions 13 to 17 were added; section 9 tells Stage 2
what `abandon` means for transfers.

Budget and order: the counted time of the full run goes from about 170 s to about 185 s (X8,
the longer S10 and S12, the tapped runs in X5 and X6), still inside 240 s; the build step
compiles a second profile and is not counted. The task order is unchanged; T1 gains the receive
progress, T3 the `ensure_connected` inside `tree`, T5 `in_flight` and `abandon`, T7 the
completer's bound and X8.

## Deviations during implementation

Recorded by the implementing agent (2026-11-18). Everything not listed here was built as planned.
The listing shape P was confirmed through the product on all 36 rows (three servers, two shares,
six directories; `docs/library-decision.md`, "Listing shape"), so the fallback of S1-1 was not
used and no flight number of section 6.4 changed.

| # | What | Why | Evidence |
|---|---|---|---|
| 1 | N1 (`no_password_no_prompt`) checks that the terminal shows no `Password for` (the prompt's own text) and that it shows the usage error, instead of "no `assword`" | The usage error the plan prescribes (5.2 step 4, `no password: give -p or --no-pass`) itself contains `assword`, so the literal check fails on correct behaviour. The new check is as strict about the prompt and also asserts the message | first run: `prompted: "rsmb: no password: give -p or --no-pass\r\n"` |
| 2 | The message for a response timeout is `connection lost: no complete reply within the response deadline; on a slow link raise --timeout`, without "after <n> s" | `smb2::Error::Timeout` carries no duration, and measuring it ourselves would add a clock to every sending method for one message. The message still names `--timeout`; the silent-server message (`no answer from the server for <n> s`) keeps its number, which `ServerUnresponsive` carries | `smb2` 0.43.0 `src/error.rs`, `Timeout` |
| 3 | `Pty::start` and `Pty::tapped` take the history path as a parameter, and `Pty::command` runs any program on the pty (N1 needs `sh -c '… < /dev/null'`) | The plan's signature had no way to name the test's own directory for `RSMB_HISTORY` | `tests/common/mod.rs` |
| 4 | The unit test `filetime` is called `filetime_cases` | A test function named `filetime` inside `mod tests` would shadow the function it tests | — |
| 5 | The pty parts of S7 and S8 are one test, `edge_completion`; S7's `ls many` runs as `ls` after `cd many` | Both need a pty session on edge through a tap; `ls many` after `cd many` would list `many/many` | `tests/slow.rs` |
| 6 | X5, X6, X7 each assert that `ctl.sh drops` did not change during that test, rather than across all three | libtest runs serial tests in name order (`timing_cd`, `timing_ls_large`, `timing_ls_small`), not X5–X7 | `tests/serial.rs` |
| 7 | The `paused` column of `bench-rsmb.sh` is measured by a small python driver over stdin in `--json` mode: each measured command is sent 1 s after the previous one's `done`, and the column is the mean time from sending it to its `done` | With a plain pipe (`sleep 1; echo cmd` piped into rsmb) the sleeps overlap connect and login, and (t_N − t_0 − N s) ÷ N came out negative (−99.7 ms for the small `ls`). Bash reads 100,000 JSON lines too slowly to time them itself | first quick bench run |
| 8 | `X1`'s tap count is the sum of the start-up's and every command's `connections` | `Client` resets the tap after start-up and after every command | `tests/serial.rs`, `one_connection` |
| 9 | In the interactive shell, a failed `-S`/`-C` line without `-k` skips the remaining start-up lines and starts the shell | 5.2 step 6 ("a failing line ends the run") and step 7 ("a failed start-up line does not prevent [the shell]") together | `src/main.rs` |
| 10 | JSON objects are printed with their keys in alphabetical order | `serde_json` without its `preserve_order` feature (which would add `indexmap`); the plan fixes the fields, not their order. `docs/scripting.md` says the order is not fixed | — |
| 11 | `Out::to(json, writer)` exists besides `Out::new(json)` | So that `help_all_has_everything` checks the real rendering of `help all`, not a copy of it | `src/commands.rs` |

Not done here: `docs/reviews.md` gets its Stage 1 section after the review (step 4 of the
process in `AGENTS.md`), as section 8 says.
