# Master plan: Stages 1 to 4

> Written on 2026-11-18, after Stage 0, by a planning agent that read the repository and nothing
> else. It supersedes the section "Provisional design for Stages 1–4" of
> [`approved-plan.md`](approved-plan.md); the rest of the approved plan (the 12 decisions, the
> documentation table, the process) stays in force. Where this document changes the provisional
> design, section 12 says what changed and why.
>
> It describes what each stage delivers, the design the stages share, the order, and the targets
> and tests each stage must meet. It does not contain what belongs in a stage plan: function
> signatures, struct layouts, test code, the exact JSON schema, the wording of messages. Each
> stage plan (`docs/plans/stage-N.md`) is written later by a separate agent from this document,
> `AGENTS.md`, the other documents and the code as it then stands.

## 1. How to use this document

A stage-plan author needs, in this order: `AGENTS.md` (rules), `docs/brief.md` with its amendments
(requirements), this document (design and targets), `docs/library-decision.md` (what `smb2` does),
`docs/test-environment.md` (fixtures, endpoints, controls, the lock), `docs/measuring.md` (`smbtap`
and the timing method). `spikes/smb2/src/main.rs` shows every `smb2` call this plan relies on,
working against the test environment.

Words used as in `docs/measuring.md`: **request** (one SMB2 header from the client; each element
of a compound counts), **flight** (a client send that follows a server response; wall time at high
latency follows flights), **outstanding**, **interim**. **Fast endpoint** is `samba` at
10.213.45.10, **slow endpoint** is `wan` at 10.213.45.30 (100 ms each way, 200 ms round trip),
**edge** is `samba-edge` at 10.213.45.11, **wan-bad** is 10.213.45.31.

A rule for every stage plan, from AGENTS.md rule 5: before writing code against the raw protocol,
look for a public function in `smb2` 0.43.0 that already does it. The crate has more than Stage 0
used (batch delete and stat, exclusive-create writers, `FileWriter::abort`, a directory reader
that returns one batch per call, a session reviver). Section 4 names the ones this plan uses.

## 2. What Stage 0 established (the facts this design rests on)

All from `docs/library-decision.md` and `spikes/smb2/results-*.txt` unless marked.

| Fact | Number | Used in |
|---|---|---|
| Connect, NEGOTIATE, 2 × SESSION_SETUP, TREE_CONNECT | 4 requests, 4 flights; 1007.5 ms at the slow endpoint including the TCP connect | 5, Stage 1 |
| `cd` check as a compound CREATE+CLOSE | 2 requests, 1 flight, 201.1 ms slow; right answer for all five cases with `FILE_READ_ATTRIBUTES \| SYNCHRONIZE` | 4.5 |
| `Tree::stat` | 5 requests | not used for `cd` |
| Listing through the crate | CREATE, QUERY_DIRECTORY × n, CLOSE, each its own flight: 4 flights for a small directory | 4.6 |
| Query buffer | default 64 KiB; Samba accepts the negotiated `max_transact` = 8 MiB (64 KiB on edge). `huge/`: 174 flights at 64 KiB, 5 at 8 MiB | 4.6 |
| Listing memory at 8 MiB | `huge/` +44.8 to +58.7 MB peak, `many/` +4.5 to +4.8 MB | 4.6, 4.7 |
| First listing on a fresh connection | 1.7–2.3× slower than on a warm one when replies are large (TCP slow start fits, not confirmed) | 5, risks |
| Credits | 513 after login on `samba`, 8192 after warm-up; 16 on edge. An 8 MiB query charges 128 | 4.10 |
| 16 listings in flight | `tree/` 18.5 s against a width-bounded 18.4 s (23 waves × 4 round trips × 0.2 s) and 269 s sequential; peak 16 outstanding; one TCP connection. The tap counted 838 flights for 1365 requests: once requests overlap, the tap's flights no longer equal waves | 4.10, 5, Stage 3 |
| Transfers through the crate's `download` and `FileWriter` | 1.1 GiB: 878–1054 MB/s down, 1126–1191 MB/s up (fast); 17.4 and 18.6 MB/s (slow, the raw TCP ceiling); peak RSS 12–21 MB; at most 8 reads / 4 writes outstanding, 512 KiB chunks | Stage 2 |
| Silent link | `ConnectionLost` after max(response timeout, 3 × 5 s keepalive): 30.0 s default, 15.0 s at the shortest | 4.9 |
| Abandoned request | no CANCEL on drop; the next request waited 1759.5 ms behind a dropped 64 MiB read | 4.9 |
| Idle connection | 0 requests in 12 s | 5 |
| Errors | typed kinds: `AccessDenied`, `NotFound`, `NotADirectory`, `AlreadyExists`, `DiskFull`, `AuthRequired`, `ConnectionLost`, `TimedOut`, … | 4.11 |
| Guest | user `Guest` or an empty user name log in; an unknown user that Samba maps to guest is refused (`GuestSessionRefused`), with no switch in the crate to accept it | Stage 1 |
| Share list | our own srvsvc call: 306 shares on edge, 31 hidden; 8 requests on `samba`. The spike's reassembly holds a stub of at most 65,535 bytes | Stage 1 |
| `wan-bad` | navigation works with default timeouts; an 8 MiB read took 65.7 s (0.1 MB/s) | 6, risks |
| smbclient-ng baseline at the slow endpoint | `cd` 2 TCP connects + 6 flights, 1610.1 ms; `ls` small 5 flights, 1207.9 ms; `many/` 17 flights, 4535.4 ms; `huge/` 139 flights, 37320.2 ms (`docs/measuring.md`) | 5 |

One fact was measured while this plan was written, because the design of the walk depends on it
(2026-11-18, read-only, impacket against the fast endpoint, QUERY_DIRECTORY with class
`FileIdBothDirectoryInformation`): **Samba reports, for a symlinked directory, the file id of the
directory it points to.** In `loops/`, the entry `self` carries the same id as `loops/` itself
(the `.` entry) and `up` carries the id of the share root (the `..` entry). The stage that builds
the walk confirms this through `smb2` before relying on it (section 4.10).

## 3. The three uses and what each demands of the design

| Use | Demands |
|---|---|
| **Developed by AI end to end** | One command, `./test.sh`, builds, starts the environment, checks it and runs every test, with no root and no human step. A failing test prints what it expected in protocol terms (requests, flights, connections, checksums). `docs/development.md` says where a new command goes (one table entry, one function, one test, one section in `docs/commands.md`). All raw use of `smb2` sits in one file, so a crate upgrade touches one place. |
| **Used interactively by a person** | A shell with a prompt, history, tab completion for commands, remote paths, local paths and share names, a progress bar for transfers, a prompt before overwriting, Ctrl-C that stops the running command and keeps the shell. No request is sent for the prompt. |
| **Used non-interactively by an AI** | Commands from `-C` (repeatable), `-S <file>` or piped stdin. Never waits for input when stdin is not a terminal. `--json` gives one JSON object per line. Stable exit codes. `rsmb --help-all` prints the whole manual without a server. |

What follows from this, for all stages:

- **One command layer, two front ends.** Every command is one function that takes the session, the
  parsed arguments and an output sink. The interactive shell and the non-interactive runner both
  call it. Only three things differ between the front ends: whether a prompt may be shown (password,
  overwrite), whether progress is drawn, and the output format. `--json` is a mode of the output
  sink, not a second implementation of any command.
- **Output.** Results go to stdout, diagnostics to stderr. Text mode is for people. In `--json`
  mode every stdout line is one JSON object with a `type` field. Decision 10 lists the kinds:
  shares, `ls` entries, `find` and `tree` entries, transfer results, errors. *Added beyond the
  approved plan:* one `done` object after each command (command text, ok or not), because the
  per-command request counts in the tests need to know when a command that sends nothing has
  finished, and a script running several commands can split the output by it. An error object
  carries a `kind` (section 4.11). The schema is fixed in the Stage 1 plan, documented in
  `docs/scripting.md`, and a test parses every line of every command's output.
- **No prompting when stdin is not a terminal.** A missing password is a usage error (exit 2), an
  existing target without `-f` is a command error (exit 1). No colours and no progress bar unless
  the stream they go to is a terminal.
- **Exit codes** (decision 7): 0 ok, 1 a command failed, 2 usage, 3 connect or login failed. The
  non-interactive runner stops at the first failing command. *Added:* 130 after SIGINT or SIGTERM,
  once temporary files are cleaned up; that is what a shell reports for a signal death, and without
  our own handler a killed transfer could not clean up.
- **Start-up flags** (decisions 8 and 12): `-H`, `-P`, `-d`, `-u`, `-p`, `--no-pass`, `-C`, `-S`,
  `-N`, `--proxy`, `--json`, `--help-all`. *Added:* `--timeout <seconds>` (smbclient-ng has `-T`),
  because the request timeout of section 4.9 has to be settable for bad links and for the tests.
- **One command table.** A static table in the code holds, per command: name, syntax, flags, a
  one-line summary, at least one example, what its arguments complete to, and its function.
  `help`, `help <cmd>`, `help all`, `rsmb --help-all`, command completion and dispatch all read
  this table. With `--json`, `help all` prints the table as data. `help all` also covers the
  start-up flags, non-interactive use, `--json` and the exit codes.
- **The test that keeps help, docs and commands in step** (AGENTS.md rule 2). It fails when a
  table entry has no example, when a command is missing from the output of `help all`, when
  `docs/commands.md` has no section for it, when `rsmb --help-all` needs a server, or when an
  example from the table does not run successfully against the test server. Examples are therefore
  written against the fixture layout (`use share`, `cd tree/d0`, `get large/medium.bin`); the test
  runs the mutating ones in a scratch directory under `upload/`.

## 4. Shared design

### 4.1 Workspace and files

```
Cargo.toml          workspace + package rsmb (root); member smbtap; spikes excluded (as now)
src/main.rs         flags (clap), start-up, the non-interactive runner, exit codes
src/shell.rs        interactive front end: rustyline, history, completion, tokeniser, prompts
src/session.rs      dialer (direct, SOCKS5), login, tree map, cd check, listing, share list,
                    cache, reconnect, error classification. All raw smb2 use is here.
src/commands.rs     the command table, every command function, the output sink (text / JSON)
src/transfer.rs     Stage 2: get, put, temporary names, recursive transfers, progress
src/walk.rs         Stage 2 (core, for get -r) and Stage 3 (tree, find): the one traversal routine
tests/common/       harness: run the binary, drive it line by line, tap, server-side checksums, ctl
tests/fast.rs       fast endpoint, may run in parallel            (./test.sh fast and full)
tests/slow.rs       slow endpoint, edge, wan-bad, SOCKS, stress   (full only)
tests/serial.rs     one at a time under the exclusive lock        (full only)
testenv/bench-rsmb.sh   timed comparison with the smbclient-ng baseline; not part of the gate
```

Six source files and three test binaries. A stage plan may split a test binary into modules by
stage; it does not add source files without a reason written into the plan. There is no trait for
"a transport", "a front end" or "an output format": each has an enum or a struct with a mode.

### 4.2 Dependencies and the pin policy

| Crate | For | Note |
|---|---|---|
| `smb2 = "=0.43.0"` | SMB | exact pin, see below |
| `tokio` | runtime, TCP, signals, file I/O | only the features used |
| `async-trait` | implementing `smb2`'s `TransportSend` / `TransportReceive` | *added:* the crate defines these traits with it, so the dialer cannot be written without |
| `tokio-socks` | SOCKS5 | from Stage 1 |
| `clap` | flags | |
| `rustyline` | line editor, history, completion | |
| `indicatif` | transfer progress | Stage 2 |
| `serde_json` | `--json` | decision 10 |
| `rpassword` | password prompt without echo | "a password prompt crate" in the approved plan |
| dev: `smbtap` (path) | counting in tests | already in the workspace |

Not added: a hashing crate (tests call the host's `sha256sum` and `docker exec … sha256sum`), a
URL crate (the `socks5://[user:pass@]host:port` form is parsed by hand), a time crate (section 14,
question S1-6), a glob crate (S3-2), a PTY crate (S1-3). Any further dependency needs one line of
justification in the stage plan.

Pin policy for `smb2`:

- The version is pinned exactly and `Cargo.lock` is committed. No upgrade happens inside a stage.
- An upgrade is its own change: bump the pin, run `spikes/smb2/run.sh` (the regression check from
  Stage 0) and `./test.sh`, record the result and the reason in `docs/library-decision.md`.
- Reasons to upgrade: a defect we hit that upstream has fixed. "A newer version exists" is not one.
- The raw layer (`execute`, `execute_compound`, message types, `rpc`) is the part most likely to
  move; it is used only in `src/session.rs`. `src/transfer.rs` uses the crate's stream types
  (`download`, `FileWriter`), `src/walk.rs` calls the listing in `session.rs`.

### 4.3 Runtime and threading

- A multi-thread tokio runtime with a small fixed number of workers (default 2). It has to be
  multi-thread: `smb2` runs a receiver task per connection, and that task must keep running while
  the main thread sits in the line editor.
- The main thread owns the terminal. It runs `rustyline` and calls `block_on` for one command at a
  time. The completer runs on the main thread too and calls `block_on` for a listing on a cache
  miss. One command runs at a time; nothing is sent between commands.
- While a command runs, SIGINT and SIGTERM are handled: the command's future is dropped, its
  cleanup runs (section 4.9), the interactive shell returns to the prompt and the non-interactive
  runner exits 130. At the prompt, Ctrl-C clears the line and Ctrl-D exits.
- Blocking work (local file I/O in transfers) goes through tokio's file API or `spawn_blocking`.

### 4.4 Session: dialer, login, tree map

- **Dialer.** One function returns a connected stream: direct TCP, or TCP to the proxy followed by
  a SOCKS5 CONNECT with the target passed as a host name (never resolved locally), with or without
  user and password. `TCP_NODELAY` is set on the socket in both cases. The stream is wrapped in our
  own send and receive halves (one `write_all` per frame, as in the spike) and handed to
  `Connection::from_transport`. The dialer is written in Stage 1 with both variants and `--proxy`,
  so that Stage 4 adds tests and documentation, not design. Connect timeout 10 s.
- **Login.** NEGOTIATE, then `Session::setup` with user, password and domain. Guest is the user
  name `Guest` with an empty password; anonymous is an empty user name. When the crate answers
  `GuestSessionRefused`, rsmb exits 3 and says that the server offered a guest session for that
  name and that `-u Guest --no-pass` asks for one on purpose.
- **Tree map.** `share name → tree`, filled on first use and kept for the session; `IPC$` is in it
  after the first `shares`. `use <share>` costs one TREE_CONNECT the first time and nothing after;
  it sets the working directory to the share root. Tree ids die with the connection: after a
  reconnect the map is empty and refills on use.
- **Share list.** Our own srvsvc `NetrShareEnumAll` on `IPC$`, built from `smb2::rpc` as in the
  spike, so hidden and non-disk shares are included. The result is kept for completing `use`. If
  the answer does not fit the reassembly limit, the command fails with a clear error; it never
  returns a truncated list.
- **State.** Host, port, proxy and credentials (needed to reconnect), the connection, the tree map,
  the current share and directory, the cache. No liveness probe exists anywhere.

### 4.5 `cd`

- The path is resolved locally (`.`, `..`, `/` and `\` as separators, relative to the working
  directory). No request is needed for that.
- If the target is the share root or an ancestor of the working directory, nothing is sent: the
  server proved those exist when it opened the working directory.
- Otherwise one compound through `execute_compound`: CREATE with `FILE_DIRECTORY_FILE`, desired
  access `FILE_READ_ATTRIBUTES | SYNCHRONIZE`, disposition open, followed by CLOSE on the related
  handle. Two requests, one flight. This is the mask Stage 0 found correct; `Tree::stat` (5
  requests) and the mask of the crate's `open_directory` (refused on the 0711 directory) are not
  used.
- Answers: success → the working directory changes; `OBJECT_NAME_NOT_FOUND` / `OBJECT_PATH_NOT_FOUND`
  → not found; `NOT_A_DIRECTORY` → not a directory; `ACCESS_DENIED` (the 0700 `forbidden/`) →
  access denied. On any failure the working directory stays what it was.
- Never a listing, and no prefetch of the listing (decision 9).

### 4.6 Listing

One listing routine in `session.rs` serves `ls`, completion, the walk and recursive transfers.

- **Buffer.** The largest the server negotiated (`max_transact`, capped at 8 MiB): 5 flights
  instead of 174 for `huge/`. On edge that is 64 KiB. The walk may use a smaller buffer
  (section 4.10).
- **Information class.** `FileIdBothDirectoryInformation` (class 37): name, size, times,
  attributes and the 64-bit file id. The provisional design asked for the smallest class; class 37
  is chosen instead because the walk needs the file id to stop at link loops (section 4.10) and
  because the crate's public parser (`smb2::msg::directory_info::parse_entries`) handles classes 3
  and 37 only. It costs 10 bytes per entry over the crate's default.
- **Memory.** A listing holds the reply buffers (up to the buffer size each) and the parsed
  entries. Measured peak for `huge/` at 8 MiB: +45 to +59 MB. Target for rsmb: peak RSS of
  `ls huge` at most 150 MB, checked in a test. `ls` prints from the entries the cache keeps; it
  does not copy them.
- **Order and output.** `ls` prints entries sorted by name, with size, modification time and a
  directory marker; `.` and `..` are not shown. The whole listing is collected before printing.
  The provisional "output streamed for very large directories" is dropped: the cache needs the
  whole listing anyway, sorted output needs it too, and `huge/` arrives in 2.3 s at the slow
  endpoint.
- **Shape on the wire: decided by a measurement at the start of Stage 1, under a rule fixed
  here.** Through the crate a listing is 4 flights however small the directory. The crate's only
  public listing call that takes a buffer size is `list_directory_instrumented`, and the only one
  that returns file ids (`list_directory_with`) cannot take a buffer size, so rsmb needs its own
  routine on the raw API to have both. That routine can also compound. Candidates, built from
  `execute_compound`, `QueryDirectoryRequest` and `parse_entries`:
  - **A:** flight 1 = compound CREATE + QUERY_DIRECTORY + QUERY_DIRECTORY. If the second query
    answers `NO_MORE_FILES`, flight 2 = CLOSE. Otherwise further single queries until
    `NO_MORE_FILES`, then CLOSE. Small directory: 4 requests, 2 flights. Never worse than now.
  - **B:** compound CREATE + QUERY_DIRECTORY + QUERY_DIRECTORY + CLOSE. Small directory: 4
    requests, 1 flight. If the second query returned entries, the handle is already closed and the
    listing is repeated with A (one wasted flight; with an 8 MiB buffer that happens above roughly
    50,000 entries).
  - **C (fallback):** the crate's calls: `list_directory_instrumented` with the buffer for `ls`
    and completion, `list_directory_with` and `ListOptions::index_numbers` for the walk. 4 flights.

  Rule: take the candidate with the fewest flights for `sizes/` that returns exactly the server's
  set of names (compared as byte strings with `docker exec … ls -A`) for `sizes/`, `many/`,
  `huge/`, `longnames/`, `cjk/` and `special/`, on `samba`, on `samba-edge` and on the `encrypted`
  share, at the fast and the slow endpoint, and that keeps `ls huge/` within 7 flights. If A and B
  both fail on any of these, C is used and the flight targets of sections 5 and Stage 3 take the
  value 4. The measurement, its table and the choice go into `docs/library-decision.md`. The CLOSE
  is never detached from the command (see 4.10).
- `ls` always asks the server and refreshes the cache (decision 1).

### 4.7 Cache and invalidation

- `(share, directory path) → entries`. Filled by `ls` and by completion. Not filled by `tree`,
  `find` or recursive transfers, so a walk over a whole share cannot grow it.
- Keys compare without regard to letter case, because SMB name lookup is case-insensitive: an
  invalidation must not miss `Upload/` because the command said `upload/`.
- Invalidation: `put`, `rm`, `mkdir`, `rmdir` (and the rename at the end of `put`) drop the entry
  of each parent directory they change; `rmdir` also drops everything below the removed directory;
  a recursive `put` drops every directory it wrote into. `refresh` drops everything. A reconnect
  keeps the cache (it is no staler than before).
- A cap on the total number of cached entries (default 500,000; `huge/` is 100,000). When a new
  listing would exceed it, the oldest directories are dropped; a single directory above the cap is
  not cached. The cap and what it costs in memory go into `docs/architecture.md`.

### 4.8 Completion and the tokeniser

- Commands complete from the command table, share names (for `use`) from the kept share list,
  remote paths from the cache, local paths (for `put`, `lcd`, `lls`, the local argument of `get`)
  from the local file system. Which kind an argument is comes from the command table.
- A remote completion in a directory that is not cached costs one listing; every later completion
  in that directory costs zero requests, and so does a completion after `ls` there.
- The tokeniser is POSIX-shlex-like (single quotes, double quotes, backslash escape) and splits on
  ASCII whitespace only: U+3000 in `東京　大阪` is part of the name. Both `/` and `\` separate path
  components once a token is formed. Completion inserts names quoted or escaped so that the line
  parses back to the same bytes. Every name in `cjk/` and `special/` must survive
  name → completion text → tokeniser unchanged (a unit test over `testenv/samba/names.sh`).
- Names are passed through as the server sends them: no Unicode normalisation, no case folding.
  The NFC and NFD forms of `café` and of `한글` are different names.

### 4.9 Timeouts, dead connections, reconnect, interruption

`smb2` decides how this can work:

- **Request timeout.** rsmb uses the crate's response deadline (`set_response_timeout`), default
  30 s, settable with `--timeout`. It adds no timer of its own around requests: a second timer
  would drop the future, the request would stay outstanding (no CANCEL), and the connection would
  be in an unknown state. The crate's keepalive stays at its default of 5 s. The effective floor is
  therefore 15 s (3 × the keepalive interval): `--timeout 5` fails a silent link after 15.0 s.
  This floor is documented in `docs/commands.md` and `docs/known-limitations.md`. `set_keepalive`
  was not tested in Stage 0 and is not touched.
- **What a dead link looks like.** A closed connection fails the next request at once; a silent
  one (blackhole, paused server) fails after the deadline. Both arrive as `ConnectionLost`. A
  response deadline can also arrive as `TimedOut`; the two are treated the same.
- **No probe.** A dead connection is found when a request fails, never before.
- **Reconnect.** On a connection-level error rsmb builds a new connection with the same dialer and
  the same login, once, and empties the tree map; the working directory and share are kept and the
  share is tree-connected again on the next use. Then:
  - a command that only reads (`shares`, `use`, `cd`, `ls`, completion, `cat`, `tree`, `find`) is
    run again, once, from its start;
  - a command that changes something (`put`, `rm`, `mkdir`, `rmdir`) and `get` are not run again.
    The error is reported with what is known about the state (section 8), and the next
    command finds a working connection.
  - If the new connection cannot be made, the command fails; the shell stays up and the next
    command tries again.

  The reconnect uses only calls measured in Stage 0 (dial, negotiate, setup, tree connect). The
  crate also offers a reviver (`SessionReviver`, `set_reviver`, `reconnect_if_needed`) whose `dial`
  returns transport halves, so it would accept our dialer; it was not verified in Stage 0 and is
  not used unless the Stage 1 plan shows it is less code and tests it (question S1-4).
- **Interruption.** Dropping a request sends no CANCEL, and the answer still has to cross the link
  before the next one. rsmb therefore never has much in flight that it might abandon: transfers
  move in chunks of at most 512 KiB with at most 8 reads or 4 writes outstanding, so abandoning
  one costs at most about 4 MiB of link time (0.25 s at the slow endpoint's 17 MB/s). After an
  interrupted `ls` of a very large directory the next command may wait for up to one reply buffer.
  An interrupted upload uses `FileWriter::abort`, which waits out the writes already sent. Where
  that wait would be long (question S2-3) rsmb reconnects instead. `Connection::send_cancel`
  exists but was not measured and is not planned.

### 4.10 Concurrency bound and credits

- **One limit, L = 16**, a constant. It bounds the listings in flight in a walk and the small
  files in flight in a recursive transfer. Stage 0 measured exactly this width: 14.6× over
  sequential on `tree/`, peak 16 outstanding, no stall on edge with 16 credits. There is no flag
  for it.
- **One connection.** Concurrency means requests overlapping on the one connection (`Connection`
  is `Clone`, `execute` takes `&self`). Nothing opens a second connection, and nothing uses
  multichannel.
- **Credits.** The crate tracks credits and waits when the server has granted too few; rsmb sizes
  its requests so that the wait is rare. A fresh connection on Samba has 513 credits and an 8 MiB
  query charges 128, so 16 listings at 8 MiB cannot be in flight at once. Default: `ls` and
  completion use the full negotiated buffer; walk listings use at most 1 MiB (16 credits per
  query, so 16 listings of two queries fit into 512). The stage that builds the walk measures a
  16-wide walk on a fresh connection on `samba` and on edge and adjusts this number if it stalls
  (question S2-5).
- **Large transfers** use the crate's own window (at most 8 reads or 4 writes of 512 KiB), which
  reached the raw TCP ceiling at the slow endpoint. In a recursive transfer, files that need the
  streaming path go one after another; files small enough for one compound read or write go up to
  L at a time. Memory is bounded by L × the small-file threshold plus one stream window.
- **The walk** (one routine, `src/walk.rs`, used by `get -r`, `tree`, `find`): a semaphore of L
  around the listing call; directories that deny access are collected and reported at the end; the
  walk goes on.
  - **Link loops.** Samba follows symlinks, so `loops/self` and `loops/up` look like ordinary
    directories. A depth cap alone does not stop them: every level of `loops/` has two directory
    children, so a cap of d allows 2^d paths. The walk keeps a set of file ids it has entered,
    seeded with the id of the start directory and of its parent (the `.` and `..` entries of the
    first listing). A directory whose id is already in the set is not entered; it is reported as a
    link to a directory already visited, with the path of the first visit where known. This
    terminates on any tree with real ids and lists each directory once. Measured on Samba 4.23.8
    while planning (section 2).
  - A server that reports no file ids (id 0) gets a depth cap (default 100, above `chain/`'s 60)
    that is reported when hit. This is a known limitation, not a guarantee of termination.
  - `-L` and `-maxdepth` limit depth as the user asks (decision 3: 1 = direct children).
- **CLOSE is awaited.** `smb` was faster in the Stage 0 walk (13.9 s against 18.5 s) because it
  does not wait for the CLOSE, at the price of 32 requests outstanding at width 16 and of work that
  outlives the command. rsmb does not copy that. It lowers the flights per listing by compounding
  (4.6) instead: at 2 flights per listing the same walk is bounded by 9.2 s, at 1 flight by 4.6 s.

### 4.11 Errors, exit codes, JSON

One small enum of error kinds in `session.rs`, with one function that classifies an `smb2::Error`
(by `kind()` and, where needed, `status()`) and local I/O errors. Each kind has a message, a
stable lower-case name for the JSON `kind` field, and says whether the connection must be
replaced. Kinds needed across the stages: `not_found`, `access_denied`, `not_a_directory`,
`is_a_directory`, `already_exists`, `disk_full`, `invalid_name` (the mangled names with DOS
wildcards, `STATUS_OBJECT_NAME_INVALID`), `auth` (wrong password, guest refused),
`connect` (dial or proxy failed), `connection_lost` (also the response deadline), `interrupted`,
`local_io`, `usage`, `other` (with the NT status in the message). Exit codes are derived from
the kind and from where the error happened: `usage` → 2, `auth` and `connect` at start-up → 3,
`interrupted` → 130, everything else → 1.

### 4.12 Documents

Per AGENTS.md rule 1 and the documentation table of the approved plan. Each stage section below
lists what it writes or extends. Every new document is added to the index in `AGENTS.md` in the
same change; the "Working commands" table and the "Tests" section of `docs/test-environment.md`
are updated when `./test.sh` changes.

## 5. Speed targets and how they are enforced

The brief's four targets, each enforced by an integration test that runs the real binary with
`smbtap` between it and the server. Commands are fed one at a time on stdin in `--json` mode; the
harness waits for the `done` line, calls `tap.settle(…)`, reads the tap and resets it, so anything
sent late is charged to the command that caused it. The per-command counts must add up to the
session total.

| Brief target | Test | Fixture, endpoint | Must hold |
|---|---|---|---|
| After the initial connect, no command opens another TCP connection | a scripted session that runs every command the stage has, in `tests/serial.rs` | `share`, fast endpoint through the tap; again on edge (multichannel advertised) | `Stats::connections == 1` in the tap **and** `ctl.sh conns <service>` rose by exactly 1 (the count that does not know the tap; needs the exclusive lock) |
| `cd` into an existing directory costs at most two requests | `cd tree/d0/d1`, also a name from `cjk/` and `special/dir with space/…` | fast and slow, through the tap | exactly CREATE + CLOSE: 2 requests, **1 flight**, `chain_len == 2` in the request log |
| A second Tab completion in the same directory costs zero requests | the binary on a real PTY, tap read before and after each Tab | `many/` and `huge/`, slow endpoint | first Tab in an unlisted directory: one listing; second Tab: 0 requests; Tab after `ls`: 0 requests |
| Report `cd` and `ls` times for rsmb and smbclient-ng at 100 ms | timing rows in `tests/serial.rs`, direct to the endpoint (never through the tap), method of `docs/measuring.md`: (time for 10 commands − time for 0) ÷ 10 | `cd /tree/d0`, `ls` in `tree/d0`, `many/`, `huge/` (N = 1), slow endpoint | printed next to the recorded baseline; asserted as in the next table |

Targets this plan adds, from the Stage 0 numbers, so that a regression in round trips fails a test
(f = flights per small listing fixed by the measurement of section 4.6: 1, 2 or 4):

| Command | Fixture, endpoint | Must hold |
|---|---|---|
| start-up to first prompt, user login | fast and slow | NEGOTIATE + 2 SESSION_SETUP: 3 requests, as measured in Stage 0; nothing else |
| `use share`, first time / again | fast and slow | 1 request / 0 requests |
| `cd /`, `cd ..`, `pwd`, `help`, `refresh`, local commands | fast | 0 requests |
| `ls` small | `sizes/`, slow | flights ≤ f; 4 requests if compounded |
| `ls many/` | slow | flights ≤ max(f, 4); 10,000 entries equal to the server's set |
| `ls huge/` | slow | flights ≤ 7 (measured 5 through the crate); 100,000 entries; peak RSS ≤ 150 MB |
| idle for 12 s | fast | 0 requests |
| `cd` wall time | slow, direct | ≤ 1.25 × the TCP connect time measured just before (201.1 ms against 200.2 ms in Stage 0) |
| `cd` wall time | fast, direct | ≤ 20 ms per command by the (10 − 0) ÷ 10 method (0.3 ms in Stage 0; a missing `TCP_NODELAY` is not caught by this bound: with `set_nodelay(false)` in both dial paths the suite passed; the unit test `dial_sets_nodelay` guards it) |
| `ls` small wall time | slow, direct | ≤ 1.25 × f × connect time |

The wall-time bounds are *added beyond the approved plan*, which asserted flights only. They
are relative to the measured round trip (slow) or two orders of magnitude above the measurement
(fast), and they catch the one regression the brief names that flights cannot see.

**Commands that overlap requests** (walks, recursive transfers) cannot be bounded in the tap's
flights: a flight starts whenever a send follows a server frame, and with 16 listings in progress
almost every send does (838 flights for 1365 requests in the Stage 0 walk). For these the tests
assert three things instead:

- total requests: no more than (units of work × requests per unit), so nothing is listed or opened
  twice;
- `peak_outstanding` in the tap: no more than L units in flight, and clearly more than one, so the
  work overlaps and stays bounded;
- wall time, direct to the slow endpoint, relative to the connect time measured just before:
  at most 1.25 × waves × round trips per unit × round trip, with waves = Σ ⌈units per level ÷ L⌉.
  For `tree/` that is 23 waves (levels of 1, 5, 16, 64 and 256 directories). Stage 0 measured
  18.5 s against 18.4 s for this formula at 4 round trips per listing.

**Comparison with smbclient-ng.** `testenv/bench-rsmb.sh` (written in Stage 1) runs rsmb through
the rows of `testenv/bench-smbclientng.sh` with the same method (5 runs per row, connect time per
row, counts through the tap, netem drops checked before and after, shared lock) and prints both
clients side by side, taking the smbclient-ng column from a baseline run. It also times the first
`ls` on a fresh connection and a run with 1 s pauses between commands, because back-to-back runs
hide TCP slow start (`docs/measuring.md`). It is not part of the gate (the baseline half takes 18
minutes). The implementing agent runs it at the end of every stage and puts the table into
`docs/performance.md`; the gate's own timing rows print the same rsmb numbers on every full run
beside the baseline figures recorded in that document. Expected from Stage 0: `cd` about 0.2 s
against 1.61 s; `ls` small 0.2–0.8 s against 1.21 s; `many/` about 0.9 s warm against 4.5 s;
`huge/` about 2.3 s warm against 37.3 s.

## 6. Testing: how `./test.sh` grows

- **Modes.** `./test.sh fast`: environment up, `check.sh`, fmt, clippy, unit tests, `smbtap`'s
  tests, `tests/fast.rs`. `./test.sh` (full): the same plus `tests/slow.rs`, `tests/serial.rs`
  with one test thread, and the tshark cross-check. The meaning of `fast` changes here from "no
  tshark" to "fast endpoint only"; `AGENTS.md`, the README and `docs/test-environment.md` are
  updated in Stage 1.
- **Which binary a test goes into.** Fast endpoint and safe beside other tests → `fast`. Slow
  endpoint, edge, `wan-bad`, SOCKS, stress fixtures → `slow` (each test has its own tap, so they
  may run in parallel; counts are not disturbed by neighbours). Anything that pauses a server,
  uses the blackhole, resets or reads the server-side connection count, or asserts a wall time →
  `serial`.
- **The lock** (`docs/test-environment.md`, "The lock"): `./test.sh` wraps the `fast` and `slow`
  test runs in a shared `flock` on `/tmp/rsmb-env.lock` and the `serial` run in an exclusive one.
  `check.sh` keeps taking its own exclusive lock, so `./test.sh` does not hold one while calling
  it. The lock table in `docs/test-environment.md` gains these rows in Stage 1. A test that pauses
  a server or turns the blackhole on restores it in a guard that also runs on failure; `up.sh`
  already cleans up after a killed run.
- **Leftovers.** Tests write only below `upload/<unique run directory>` on the server and below
  cargo's test temp directory locally, and delete what they wrote. `upload/` is emptied only by
  recreating the container, so `./test.sh` removes stale run directories at its start
  (`docker exec`), and the multi-gigabyte upload test deletes its file even when it fails.
- **Output.** Every assertion message states the expectation in protocol terms: command, fixture,
  endpoint, expected and actual requests, flights, connections, the per-command breakdown from the
  tap, or both checksums.
- **The suite only grows.** Each stage re-runs everything from earlier stages.

Budget for the duration, with the environment already up and the binary built (today: about 30 s
for either mode):

| After stage | `./test.sh fast` | `./test.sh` (full) |
|---|---|---|
| 1 | ≤ 1.5 min | ≤ 4 min |
| 2 | ≤ 2 min | ≤ 7 min |
| 3 | ≤ 2 min | ≤ 9 min |
| 4 | ≤ 2.5 min | ≤ 10 min |

Rules that keep it there:

- `./test.sh` prints the duration of each step and the total. A full run over budget is a finding
  that is fixed before the stage closes (smaller subset of a fixture, a test moved to the bench).
- A single test longer than 30 s needs a line in `docs/testing.md` saying why it cannot be shorter.
- What is too slow for the gate and is a measurement rather than a check goes into
  `testenv/bench-rsmb.sh`: the 1.1 GiB and 4.3 GiB transfers at the slow endpoint (65 s and about
  270 s each way), the walk of `fanout/` at the slow endpoint (25–102 s), the full timing table.
  Their results are recorded in `docs/performance.md` at the stage's end.
- Known costs at the slow endpoint, for planning: a new rsmb process 1.0 s; `cd` 0.2 s; a small
  `ls` 0.2–0.8 s; each silent-link test at least 15 s; 50 MiB about 5 s each way. On `wan-bad`
  only navigation and files of at most 1 MiB are used (an 8 MiB read took 65.7 s).
- The 1,000,000-entry `xl/` fixture stays opt-in and outside the gate.

## 7. Stage 1: connection and navigation

**Scope (brief).** Connect with host, port, user, password and domain, plus guest and anonymous.
Interactive shell with history and tab completion for commands and remote paths. Non-interactive
mode with a meaningful exit status. Commands `shares`, `use`, `cd`, `pwd`, `ls` (sizes,
timestamps, directory markers), `help`, `exit`. All design requirements: one connection, one tree
connect per share, no liveness probe, reconnect when a request fails, `cd` by one open/close,
cached listings reused by completion, a manual refresh.

**Decisions of the approved plan that apply.** 1 (`ls` always asks; `refresh`), 2 (`pwd`), 7 (exit
codes, stop at first failure), 8 (flags), 9 (`cd` ≤ 2 requests, flights measured, no prefetch),
10 (`--json`, no prompting), 12 (`help all`, `--help-all`, one table).

**Deliverables.**

- Code: the workspace change (root package `rsmb`), `main.rs`, `shell.rs`, `session.rs`,
  `commands.rs` with `shares`, `use`, `cd`, `pwd`, `ls`, `refresh`, `help` (`help`, `help <cmd>`,
  `help all`), `exit`. The dialer with direct and SOCKS5 and the `--proxy` flag. The listing-shape
  measurement of section 4.6 as the first task.
- Tests: `tests/common`, `tests/fast.rs`, `tests/slow.rs`, `tests/serial.rs`, unit tests
  (tokeniser, path resolution, name quoting, error classification, share-list overflow, a name with
  an unpaired UTF-16 surrogate on the decoding path). `./test.sh` with the two modes and the lock.
  `testenv/bench-rsmb.sh` with the navigation rows.
- Documents: new `docs/architecture.md`, `docs/commands.md`, `docs/scripting.md`,
  `docs/development.md`, `docs/testing.md`, `docs/performance.md` (navigation table),
  `docs/plans/stage-1.md`; extended `docs/library-decision.md` (listing shape),
  `docs/known-limitations.md`, `docs/test-environment.md` (tests, lock table), `docs/reviews.md`,
  `README.md`, `AGENTS.md` (index, working commands, principles that proved themselves).

**Targets and tests.** In addition to every row of section 5:

| # | What | Fixture, endpoint | Must hold |
|---|---|---|---|
| 1.1 | logins | fast | user/password/domain works; `Guest` and anonymous list the `guest` share and are refused on `share`; wrong password → exit 3; unknown user with a password → exit 3 and the message names `-u Guest` |
| 1.2 | `cd` answers | `nope`, `sizes/1.bin`, `forbidden/`, `traverse-only/`, fast | not found, not a directory, access denied, success; working directory unchanged after a failure; each still 2 requests in 1 flight |
| 1.3 | `ls` content | `sizes/`, `many/`, `longnames/`, `cjk/`, `special/`, `mangled/`, fast | names equal the server's set as byte strings; sizes in `sizes/` exact; directories marked; `mangled/` shows 9 names; no crash on any |
| 1.4 | `ls` where listing is denied | `forbidden/`, `traverse-only/`, fast | access denied, exit 1, shell survives |
| 1.5 | names through the shell | `cjk/`, `special/`, PTY, fast | list → Tab → `cd` works for every directory name, the ideographic space is not split, the NFC and NFD Korean directories are told apart |
| 1.6 | `shares` | fast and edge | on edge all 306, 300 `edge*`, 31 with `$`, with type and comment; `use <Tab>` completes from it with 0 further requests |
| 1.7 | edge | `many/` on edge | `cd`, `ls`, Tab complete; names equal; no stall with 64 KiB and 16 credits |
| 1.8 | encrypted share | `encrypted`, fast | `use`, `cd`, `ls` work; the tap sees only opaque frames after login |
| 1.9 | reconnect | tap drops the connection while idle, fast | the next `ls` succeeds; the tap shows exactly one further connection; share and working directory unchanged |
| 1.10 | silent link | blackhole port 4450 and paused `samba`, `--timeout 5`, serial | the command fails between 15 and 20 s with kind `connection_lost`; in a PTY session the shell is still there, and after the fault is lifted the next command works |
| 1.11 | non-interactive contract | fast | with stdin not a terminal and no password flag: exit 2 at once, nothing read from the terminal; `-C` twice, `-S`, piped stdin all work; a failing command stops the run with exit 1; every `--json` line parses and has a `type` |
| 1.12 | help | no server, and fast | the test of section 3 |
| 1.13 | interactive basics | PTY, fast | prompt shows share and directory without any request; history recalls the previous line; Ctrl-C at the prompt does not exit; `exit` and Ctrl-D exit 0 |
| 1.14 | SOCKS5 smoke | `fileserver.rsmb.test` through 10.213.45.20:1080 | `use share` and `ls` work; the proxy log gains one `connected to fileserver.rsmb.test:445` line |
| 1.15 | `wan-bad` smoke | `tree/`, wan-bad | connect, `cd`, `ls` complete with the default timeout; times printed |
| 1.16 | cache cap | unit | the oldest directory is dropped at the cap; a directory above the cap is not cached |

**Not in Stage 1.** Transfers and file operations, `cat`, local commands, `tree`, `find`, the full
SOCKS5 test matrix, any command that changes the server (so cache invalidation by commands is
tested in Stage 2; `refresh` is tested here).

**Exit criteria.** Every row of section 5 and of the table above passes in `./test.sh`; the full
run is within 4 minutes; the listing-shape decision is recorded with its measurements;
`docs/performance.md` holds the navigation table from `bench-rsmb.sh` beside the baseline; the
documents listed exist and match the code; the review's confirmed findings are fixed and their
tests are in the suite.

## 8. Stage 2: transfers and file operations

**Scope (brief).** `get` and `put`, single file and recursive, keeping directory structure.
Streaming I/O. Progress display, correct byte counts, non-zero exit on failure. No file that looks
complete after a failed or interrupted transfer. No silent overwrite. `mkdir`, `rm`, `rmdir`,
`cat`, `lcd`, `lpwd`, `lls`.

**Decisions that apply.** 4 (`get <remote> [local]` writes under the remote base name into the
local working directory; `-r` keeps the structure below the named directory; `put` mirrors it),
5 (`-f` to overwrite; prompt when interactive, error otherwise), 6 (no `mv`; rename exists only as
the last step of `put`), 7, 10.

**Design specific to this stage.**

- **Data path.** Large files use the crate's `Tree::download` and `FileWriter`
  (`create_file_writer`), which Stage 0 measured: in order, 512 KiB chunks, memory 12–21 MB. rsmb
  writes no chunk scheduler of its own. Small files in a recursive transfer use the crate's
  compound read and write calls or our own compound, up to L at a time (section 4.10).
- **`get`.** Data goes to a temporary file in the destination directory and is renamed onto the
  final name after the last byte, when the byte count equals the size the server reported at open.
  The final name is checked first: it exists and no `-f` → prompt or error, nothing is touched.
- **`put`.** Data goes to a temporary name in the destination directory on the server and is
  renamed there, with replace only under `-f`. The target is checked before the upload starts, so
  a refusal costs one round trip, not the upload; the rename without replace still guards against
  a file that appeared meanwhile.
- **Temporary names** must fit where the final name fits. `longnames/` has 250-byte names, so a
  suffix on the final name would pass the 255-byte limit on both sides. The temporary name is
  short, recognisably rsmb's, and derived from the final name so that a retry reuses it
  (format: question S2-1).
- **Failure and interruption.** On an error, on Ctrl-C and on SIGTERM the temporary file is
  removed. If it cannot be removed (the connection is gone and one reconnect did not bring it
  back), the error names the file that was left. Nothing is ever left under the final name. An
  interrupted or failed transfer is not retried automatically.
- **Recursive.** Uses the walk of section 4.10 (built in this stage). Names from the server are
  validated before use as local path components: no empty name, `.`, `..`, separator, NUL or
  absolute path. A recursive transfer goes on after a file fails, reports every failure at the
  end and exits 1 if there was any. Before a recursive `put`, names in one directory that differ
  only in letter case are reported and not uploaded (the server would write one over the other).
- **`cat`** streams to stdout. `mkdir`, `rm`, `rmdir` (empty directories only) use the crate's
  calls and invalidate the cache as in section 4.7. `lcd`, `lpwd`, `lls` send nothing.
- **Progress.** A bar on stderr when stderr is a terminal; nothing otherwise. The transfer result
  (path, bytes, duration) is one line of output, a `transfer` object in `--json`.

**Deliverables.**

- Code: `src/transfer.rs`, `src/walk.rs` (the traversal core with the loop check and the denied
  list), the new commands in `commands.rs`, completion of local paths.
- Tests: the table below; transfer rows in `bench-rsmb.sh`.
- Documents: new `docs/transfers.md` (temporary names, overwrite rules, failure and interrupt
  handling, limits), `docs/plans/stage-2.md`; extended `docs/commands.md`, `docs/scripting.md`,
  `docs/architecture.md` (walk, concurrency), `docs/testing.md`, `docs/performance.md` (transfer
  table against the iperf3 ceiling), `docs/known-limitations.md`, `docs/reviews.md`, `README.md`,
  `AGENTS.md`.

**Targets and tests.** Checksums are sha256, the server's taken with `docker exec … sha256sum`.

| # | What | Fixture, endpoint | Must hold |
|---|---|---|---|
| 2.1 | every size | all 11 files of `sizes/`, down and up, fast | checksums equal; the byte count reported equals the size; the 0-byte file works |
| 2.2 | 1.1 GiB | `large/big.bin` down, up into `upload/`, fast | checksums equal; peak RSS of rsmb ≤ 64 MB; in the tap never more than 16 requests outstanding |
| 2.3 | above 4 GiB | `large/over4g.bin` down and up, fast | checksum `d0b7cf9b…` both ways, 4,617,089,843 bytes |
| 2.4 | at latency | `large/medium.bin` down and up, slow | checksums equal; rate printed beside the iperf3 figure |
| 2.5 | many small files | `smallfiles/` (2,000 files) down and up, fast | every checksum equal; structure equal |
| 2.6 | small files at latency | a subset of `smallfiles/` (N files, e.g. 2 directories = 200) down and up, slow: counted through the tap, timed direct | time ≤ 1.5 × (⌈N ÷ L⌉ × p + the round trips of the listings and `mkdir`s) × round trip, with p the round trips per file fixed in the stage plan (at most 3 for `get`, 4 for `put`); requests ≤ N × the requests per file plus the listings; never more than the documented bound outstanding; checksums equal. Sequential would need N × p round trips |
| 2.7 | structure | `tree/` (skipping what is denied), `deep/`, `chain/` (60 levels), `get -r` then `put -r`, fast | the local tree equals the server's (paths and checksums); the tree put back equals the one fetched; decision 4's layout |
| 2.8 | names | `cjk/`, `special/`, `longnames/`: `get -r`, then `put -r` back, fast | every name byte-identical on the server (`ls -A` in the container) and locally; the case pair is reported, not overwritten; `rm` and `rmdir` by those names work |
| 2.9 | names that cannot be opened | `get -r mangled/`, fast | 5 files fetched, 4 reported as `invalid_name`, exit 1, no crash |
| 2.10 | link loops | `get -r loops/`, fast | terminates; lists `loops/` once; reports `self` and `up` as links to visited directories; downloads nothing outside `loops/` |
| 2.11 | overwrite | local and remote targets that exist, fast | without `-f`: error, exit 1, the existing file's checksum unchanged; with `-f`: replaced; on a PTY: the prompt, answered yes and no |
| 2.12 | interrupted and failed | tap cut in the middle of `get` and of `put` of `big.bin`; SIGINT and SIGTERM in the middle of each; fast | nothing under the final name, locally or on the server; the temporary file is gone or named in the error; exit non-zero (130 for the signals); in a PTY session the next command works |
| 2.13 | disk full | `put` of 12 MiB to `tiny` | kind `disk_full`, exit 1, `tiny` holds nothing afterwards |
| 2.14 | read-only | `put`, `mkdir`, `rm` on `readonly` | `access_denied`, exit 1 |
| 2.15 | file operations and cache | `mkdir`, `rm`, `rmdir`, `cat` in `upload/`, fast, through the tap | effects verified in the container; after each, Tab in that directory costs exactly one listing and shows the new state; `cat` output equals the file |
| 2.16 | changing commands are not repeated | tap cut during `mkdir` and `put`, fast | the command reports the lost connection and is not sent twice; the next command works on one new connection |
| 2.17 | stingy server, encrypted share | `medium.bin` down and up on edge and on `encrypted` | checksums equal; no stall |
| 2.18 | silent link in a transfer | `samba` paused during `get`, serial | fails within the timeout floor; nothing under the final name |
| 2.19 | local path safety | unit | `..`, names with separators, absolute paths, NUL, empty are refused |
| 2.20 | one connection | the scripted session of section 5, extended by every Stage 2 command | still exactly 1 |
| 2.21 | progress and counts | PTY and pipe | a bar on a terminal, none on a pipe; the final byte count equals the size |
| 2.22 | `wan-bad` | `sizes/1048577.bin` down and up | checksums equal; time printed |

Measured once at the end of the stage with `bench-rsmb.sh`, recorded in `docs/performance.md`, not
in the gate: `big.bin` three times each way at the slow endpoint against iperf3 (no run above
three times the median, the Stage 0 rule), `over4g.bin` once each way at the slow endpoint, and
the raw TCP rate through `wan-bad` beside an 8 MiB transfer there.

**Not in Stage 2.** `tree` and `find` as commands, their filters and output; resuming a transfer;
`mv`; recursive `rm`; preserving timestamps or attributes; transfers of several large files at
once.

**Exit criteria.** All rows pass; full run within 7 minutes; the bench measurements above are
recorded; `docs/transfers.md` states every rule a reviewer can check against the code; the
documents match the code; confirmed review findings fixed.

## 9. Stage 3: `tree` and `find`

**Scope (brief).** `tree` with a depth limit. `find` with filters for name (glob), type (file or
directory), size and depth. Both traverse concurrently within the bounded limit and keep going
past directories that deny access, reporting those at the end.

**Decisions that apply.** 3 (depth as in Unix tools: `tree -L 1` and `find -maxdepth 1` mean
direct children), 7, 10.

**Design specific to this stage.** The walk exists since Stage 2. This stage adds the two commands
on top of it: `tree [path] [-L N]` and `find [path] [-name GLOB] [-iname GLOB] [-type f|d]
[-size [+|-]N[BKMGTP]] [-maxdepth N]`. Filters are applied to the entries the listings already
carry; no request is sent per entry. `tree` needs its output in tree order and collects before
printing; `find` prints matches as directories complete, in no fixed order (documented; tests
compare sets). Denied directories and skipped links are printed at the end and are objects of
their own in `--json`, followed by a summary object with the counts.

**Deliverables.**

- Code: the two commands and the filters in `commands.rs` and `walk.rs`.
- Tests: the table below; walk rows in `bench-rsmb.sh`.
- Documents: `docs/plans/stage-3.md`; extended `docs/commands.md`, `docs/scripting.md`,
  `docs/architecture.md`, `docs/testing.md`, `docs/performance.md` (walk times against the
  sequential figure), `docs/known-limitations.md`, `docs/reviews.md`, `README.md`, `AGENTS.md`.

**Targets and tests.** f = flights per small listing from Stage 1 (1, 2 or 4).

| # | What | Fixture, endpoint | Must hold |
|---|---|---|---|
| 3.1 | depth | `tree -L 1`, `-L 2`, no limit on `tree/`; `find -maxdepth` the same; fast | the sets equal `find` run in the container with the same depth; decision 3 |
| 3.2 | filters | `-name`, `-iname`, `-type f`, `-type d`, `-size` with `+`, `-`, exact and each unit, and combinations, on `sizes/`, `tree/`, `cjk/` (1,010 entries), `special/` (names with `[`, `{`, `*`-like characters); fast | each result set equals the container's `find` with the same predicate |
| 3.3 | concurrency at latency | `find` over `tree/` (342 listings in levels of 1, 5, 16, 64, 256 = 23 waves at width 16), slow: counted through the tap, timed direct in `tests/serial.rs` | time ≤ 1.25 × 23 × f × round trip (23 s at f = 4, 11.5 s at f = 2, 5.75 s at f = 1); requests ≤ 342 × the requests of one listing; listings in flight never above 16 (peak outstanding ≤ 16 × the requests of one listing step) and above 1; exactly 1 TCP connection; 341 directories listed and `tree/forbidden` reported |
| 3.4 | wide directory | `find` over `fanout/` (2,001 listings), fast through the tap and on edge | all 2,000 directories and files found; in-flight bound as in 3.3; completes on 16 credits |
| 3.5 | denied directories | `tree/` with `tree/forbidden`, `forbidden/`, `traverse-only/`; fast | the walk continues; each denied directory is reported once at the end; the exit status is the one the stage plan fixes (question S3-1) |
| 3.6 | link loops | `tree` and `find` on `loops/` and on the share root with `-maxdepth` large enough to reach them; fast | terminate; each real directory listed once; `self` and `up` reported as links to visited directories |
| 3.7 | depth that cannot be parallelised | `chain/` (60 levels) fast through the tap; `deep/` (12 levels) slow through the tap | `bottom.txt` found; the listings are strictly one after another here, so flights are meaningful: flights ≤ 1.25 × (levels + 1) × f |
| 3.8 | JSON | `--json` for both commands | every line parses; entry, denied, skipped and summary objects as documented |
| 3.9 | reconnect in a walk | tap cut in the middle of `find` over `fanout/`, fast | the walk is restarted once and completes on one new connection, or fails with `connection_lost` if the stage plan decides so; never a partial result reported as complete |
| 3.10 | one connection | the scripted session of section 5 with `tree` and `find` | still exactly 1 |
| 3.11 | glob matcher | unit | `*`, `?`, classes, a literal `[`, case folding for `-iname` with non-ASCII names |

Measured at the end of the stage with `bench-rsmb.sh`, recorded, not in the gate: `fanout/` at
the slow endpoint, 16 wide (126 waves: 25 s at f = 1, 102 s at f = 4), and `tree/` again, each
against the width-bounded figure (waves × f × round trip) and the sequential extrapolation.
Stage 0 as written asked for "levels × listing round trips", which no client can meet at width
16; the bound here is the one that can be met.

**Not in Stage 3.** Other `find` predicates (`-mtime`, `-exec`, `-delete`), content search,
sorting options, following or not following links as an option, a flag for the width.

**Exit criteria.** All rows pass; full run within 9 minutes; walk times recorded; documents match
the code; confirmed review findings fixed.

## 10. Stage 4: SOCKS5 proxy

**Scope (brief).** `--proxy socks5://[user:pass@]host:port`. All traffic to the server goes
through the proxy, including name resolution when a host name is given. Everything from Stages 1
to 3 works through the proxy.

**What is left to do.** The dialer and the flag exist since Stage 1 and a smoke test has kept them
working. This stage proves the claim "everything works through the proxy", handles the proxy's own
failure cases, and closes the project: final performance table, final README, final list of known
limitations. Its review covers the whole codebase (approved plan).

**Deliverables.**

- Code: whatever the tests below show is missing (proxy error mapping, URL edge cases). No new
  module.
- Tests: the table below. The test harness gains a way to run a test's session through a proxy.
- Documents: `docs/plans/stage-4.md`; extended `docs/commands.md` (flag), `docs/architecture.md`
  (dialer), `docs/scripting.md`, `docs/testing.md`; final `docs/performance.md` (the full
  comparison with smbclient-ng), final `docs/known-limitations.md`, `docs/reviews.md` (whole
  codebase), final `README.md` (headline numbers, the statement that only Samba was tested),
  `AGENTS.md`.

**Targets and tests.** The server is `samba-private`, named `fileserver.rsmb.test`, reachable only
through `socks` (10.213.45.20, port 1080 without authentication, 1081 with `tester`/`testpass`).

| # | What | Through | Must hold |
|---|---|---|---|
| 4.1 | everything from Stages 1–3 | port 1080 | the tests of `tests/fast.rs` pass again with the session going through the proxy: same outputs, exit codes and checksums (what is and is not re-asserted about request counts: question S4-1) |
| 4.2 | proxy with authentication | port 1081, credentials in the URL | navigation, one transfer each way with checksums, one walk |
| 4.3 | traffic really goes through | both ports | per rsmb session exactly one new `connected to fileserver.rsmb.test:445` line in the proxy log (the name as rsmb sent it, resolved by the proxy) and `ctl.sh conns samba-private` rises by exactly 1 |
| 4.4 | name resolution | precondition checked in the test: `fileserver.rsmb.test` does not resolve on the host | the session works anyway, so rsmb did not resolve the name itself |
| 4.5 | negative control | no `--proxy`, target 10.213.47.10 and target `fileserver.rsmb.test` | exit 3 within the connect timeout |
| 4.6 | proxy failures | wrong password on 1081, no credentials on 1081, proxy port closed, malformed URL | exit 3 (exit 2 for the malformed URL), a message that names the proxy, bounded time |
| 4.7 | reconnect through the proxy | a tap between rsmb and the proxy cuts the connection | the next reading command succeeds; the proxy log gains exactly one more line |
| 4.8 | large transfer through the proxy | `big.bin` down and up, port 1080 | checksums equal |
| 4.9 | URL parsing | unit | with and without credentials, credentials with special characters, IPv4, host name, missing port |

**Not in Stage 4.** SOCKS4, HTTP proxies, proxy chains, UDP, reading the proxy from environment
variables.

**Exit criteria.** All rows pass; the full run is within 10 minutes; `bench-rsmb.sh` has produced
the final comparison and `docs/performance.md` and the README carry it; the whole-codebase review
is done, its confirmed findings fixed; `docs/known-limitations.md` is complete; every document in
the `AGENTS.md` index matches the code.

## 11. Order

Stages run strictly one after another, 1 → 2 → 3 → 4, each through the loop in `AGENTS.md`. What
each hands to the next:

- Stage 1 → 2: the session, the listing routine with file ids, the cache with its invalidation
  entry points, the command table, the output sink, the test harness, the three test binaries.
- Stage 2 → 3: the walk (concurrency bound, loop check, denied list).
- Stages 1–3 → 4: nothing new is needed; Stage 4 re-runs their tests through the dialer's second
  variant.

Inside Stage 1 the listing-shape measurement comes first, because targets in Stages 1 and 3 take
their value from it.

## 12. What changed against the provisional design, and why

| Provisional design | Now | Why |
|---|---|---|
| "a small tokio runtime via `block_on`" | multi-thread runtime, 2 workers; main thread owns the terminal | `smb2` runs a receiver task per connection that must not stop between commands |
| `cd` compound "if the crate supports it, otherwise CLOSE in the background" | compound, fixed mask `FILE_READ_ATTRIBUTES \| SYNCHRONIZE` with the directory flag | measured: 2 requests, 1 flight, right for all five cases; the background variant is not needed |
| "Root and `..` to root cost nothing" | the root and any ancestor of the working directory cost nothing | the server proved the ancestors exist when it opened the working directory |
| `ls`: "smallest info class" | class 37 (`FileIdBothDirectoryInformation`) | the walk needs file ids; the crate's parser handles classes 3 and 37 only |
| `ls`: "output streamed for very large directories" | collected, sorted, then printed | the cache and sorting need the whole list; `huge/` takes 2.3 s at the slow endpoint |
| listing through the crate | our own routine on the raw API, compounded if the Stage 1 measurement passes; the crate's calls as fallback | 4 flights per listing through the crate; no crate call takes both a buffer size and file ids |
| cache without a limit; nothing about case | a cap on entries; keys compared case-insensitively; walks do not fill it | `xl/`-sized directories and whole-share walks; SMB lookup is case-insensitive |
| "a request timeout catches a silent link" | the crate's response deadline, default 30 s, `--timeout`; floor 15 s; no timer of our own | measured: max(timeout, 3 × keepalive); a second timer would abandon requests that cannot be cancelled |
| reconnect, unspecified mechanism | a new connection through the same dialer; the crate's reviver not used by default | only measured calls; the reviver was not verified in Stage 0 |
| transfers: "bounded window of concurrent chunk requests" (our own) | the crate's `download` and `FileWriter` | measured: raw TCP ceiling at the slow endpoint, 12–21 MB of memory, at most 8 / 4 outstanding |
| `get` writes `name.rsmb-part` | a short temporary name derived from the final one | `longnames/` (250-byte names) plus a suffix passes the 255-byte limit |
| "a depth cap that stops symlink loops" | a set of visited file ids, seeded with the start directory and its parent; depth cap only for servers without ids | in `loops/` every level has two directory children, so a cap of d allows 2^d paths; Samba reports the target's id for a link (measured while planning) |
| the walk appears with `tree` / `find` | the walk core is built in Stage 2 | `get -r` needs it and must terminate on `loops/` |
| (Stage 0 question) should CLOSE be detached as `smb` does | no; fewer flights per listing by compounding | detaching doubles the requests in flight and leaves work behind the command |
| SOCKS5 in Stage 4 | dialer and `--proxy` in Stage 1 with a smoke test; Stage 4 is the proof and the failure cases | so that Stage 4 is small and nothing above the dialer has to change |
| dependencies: "a PTY crate" for the Tab test | default: a python3 `pty` helper (question S1-3) | python3 is already a host requirement; one dependency fewer |
| dependencies | `async-trait` added | `smb2`'s transport traits are defined with it |
| tests: stress rows "at the slow endpoint" for all of `smallfiles/`, `fanout/`, `over4g.bin` | slow endpoint in the gate: `tree/` walk, a subset of `smallfiles/`, `medium.bin`; the rest at the fast endpoint in the gate and at the slow endpoint in `bench-rsmb.sh` | the gate must stay within 10 minutes: `fanout/` alone is up to 102 s, `over4g.bin` about 270 s each way, 2,000 small files several minutes |
| tests: bounds "in flights or terminates, not seconds" | plus wall-time bounds for `cd` and small `ls` (relative to the measured round trip; 20 ms for `cd` at the fast endpoint) | the bounds catch a slow path, not a missing `TCP_NODELAY` (the suite passed without it; the unit test `dial_sets_nodelay` guards it) |
| walk bound "flights near (listings ÷ concurrency limit)" | wall time ≤ 1.25 × waves × round trips per listing × round trip, waves = Σ ⌈directories per level ÷ 16⌉; plus total requests and peak outstanding | the lesson of gate 2 (a formula the setup can meet), and the tap's flights do not equal waves once requests overlap (838 flights for 1365 requests in Stage 0) |

Additions to the approved plan's decisions (none is changed): `--timeout`; the `done` object and
the `kind` field in `--json`; exit code 130 after a signal; `refresh` as the name of the manual
refresh of decision 1; `shares`' result kept for completing `use`.

## 13. Risks carried from Stage 0, and where each is handled

| Risk | Handled in | How |
|---|---|---|
| An unknown user mapped to guest is refused by the crate, with no opt-in | Stage 1 | exit 3 with a message that names `-u Guest --no-pass`; test 1.1; `docs/known-limitations.md`. Accepting such sessions would need a patched crate and is not planned |
| Mangled names whose original has a DOS wildcard cannot be opened (4 of 9 in `mangled/`) | Stages 1, 2 | listed like any name; opening gives kind `invalid_name`; a recursive `get` goes on and reports them; tests 1.3, 2.9 |
| Names stored in a legacy encoding are not listed by Samba | — | nothing a client can do; stays in the known limitations |
| 4 flights per listing | Stage 1 | the measurement and rule of section 4.6; Stage 3's bounds use the result |
| Effective timeout floor of 15 s | Stage 1 | documented with `--timeout`; silent-link tests expect 15–20 s; the keepalive is not changed |
| No CANCEL when a request is dropped | Stages 1, 2 | small units in flight (4.9); `FileWriter::abort` for uploads; reconnect where waiting would be long (S2-3); test 2.12 checks the next command works |
| Release cadence of `smb2`, raw layer may move | all | exact pin, upgrade as its own change with the spike as regression check, raw API in one file (4.2) |
| The crate's reconnect hooks and `set_keepalive` were not verified | Stage 1 | not used by default (4.9, S1-4) |
| No full 4.3 GiB transfer measured | Stage 2 | test 2.3 in the gate at the fast endpoint; once at the slow endpoint in the bench |
| One transfer run at the slow endpoint | Stage 2 | `medium.bin` in the gate (2.4); `big.bin` three times each way in the bench, with the three-times-median rule |
| No large file on an encrypted share | Stage 2 | test 2.17 |
| A fresh connection lists large directories 1.7–2.3× slower (TCP slow start, unconfirmed) | Stage 1 | not a client defect; `bench-rsmb.sh` reports the first listing and a run with pauses, so the published numbers do not hide it |
| `wan-bad` moved 8 MiB at 0.1 MB/s and has no measured TCP ceiling | Stage 2 | the gate uses files of at most 1 MiB there; the bench measures iperf3 through `wan-bad` beside an 8 MiB transfer, so a client defect can be told from the link |
| srvsvc answer limited to 65,535 bytes of stub (about 700 shares) | Stage 1 | a clear error instead of a short list; unit test; known limitation |
| Names that differ only in case (`Case.txt`, `case.txt`) | Stage 2 | a recursive `put` reports the pair and uploads neither over the other; test 2.8 |
| Names from the server used as local paths | Stage 2 | validation before use; test 2.19 |
| Samba follows symlinks: endless depth | Stage 2 (walk), Stage 3 | visited file ids (4.10); tests 2.10, 3.6; servers without ids documented |
| Unpaired UTF-16 surrogates (Windows only) | Stage 1 | unit test on the decoding path; if the crate's parser rejects the whole reply, that is recorded as a limitation |
| `upload/` is emptied only by recreating the container | Stage 1 (harness), Stage 2 | unique run directories, cleanup on failure, stale directories removed at the start of `./test.sh` |
| On encrypted connections the tap cannot name requests, and "outstanding" is approximate | Stages 1, 2 | tests on `encrypted` assert function and opaque frame counts only |
| The server-side connection count needs the environment to itself | Stage 1 | the one-connection test runs in `tests/serial.rs` under the exclusive lock |
| Windows servers untested | Stage 4 (README) | stated in the README and the known limitations; no claim |
| The gate grows too slow to be run after every feature | all | the budget and rules of section 6 |

## 14. Open questions for the stage plans

Each has a default. A stage plan either keeps the default or replaces it and says why.

**Stage 1**

- S1-1. Which listing shape (section 4.6)? Decided by the measurement under the rule there.
- S1-2. The JSON schema: field names per object type, how times and sizes are written. Default:
  sizes as integers, times as RFC 3339 in UTC, names as JSON strings.
- S1-3. How the tests get a PTY. Default: a small python3 `pty` helper under `testenv/` driven by
  the Rust tests; a dev-dependency only if the helper proves unreliable.
- S1-4. Reconnect by replacing the connection (default) or through the crate's reviver.
- S1-5. Where the history file lives and how tests keep it out of the home directory. Default:
  the XDG state directory, overridable by an environment variable that the tests set.
- S1-6. How `ls` shows times in text mode. Default: UTC, computed without a time crate.
- S1-7. What `-C`, `-S` and piped stdin do when combined, and when the interactive shell starts
  afterwards. Default: `-S` first, then every `-C` in order, then stdin if it is not a terminal;
  the shell starts only if stdin is a terminal and `-N` is absent.
- S1-8. A name the crate cannot decode in a listing (unpaired surrogate): replace the bad unit and
  list the rest (default), or fail the listing if the crate leaves no choice.
- S1-9. Whether `shares` can cost fewer than the measured 8 requests (keeping `IPC$` connected
  saves two). Default: keep `IPC$` in the tree map; no further work.

**Stage 2**

- S2-1. The format of temporary names, local and remote, within the 255-byte limit.
- S2-2. The flights per small file (p in test 2.6) and whether a `put` of a small file can be one
  compound of CREATE, WRITE, rename and CLOSE. Default: the crate's calls; p as measured, at most
  3 for `get` and 4 for `put`.
- S2-3. After an interruption, how long to wait for requests in flight before reconnecting
  instead. Default: wait up to 2 s, then reconnect.
- S2-4. `cat` in `--json` mode. Default: a text field for valid UTF-8; otherwise an error that
  points to `get`.
- S2-5. The buffer size of walk listings and the small-file threshold, from a 16-wide run on a
  fresh connection on `samba` and on edge. Defaults: 1 MiB and 1 MiB.
- S2-6. Confirm through `smb2` that listings carry the target's file id for `loops/self` and
  `loops/up`. If they do not: compare each directory's listing with those of its ancestors
  instead, and say so in the plan.
- S2-7. How the test binary is built so that the 4.3 GiB test fits the budget. Default: the
  integration tests run a release build.
- S2-8. `mkdir` of a nested path. Default: one level; `-p` creates the missing parents.

**Stage 3**

- S3-1. The exit status of a walk that skipped denied directories. Default: 0, with the denied
  directories reported, because such directories are normal on real shares and the runner stops
  at the first non-zero status. Any other error during the walk gives 1.
- S3-2. The glob matcher. Default: our own for `*`, `?` and classes with unit tests; a
  dependency-free crate if the classes need more than that.
- S3-3. Whether `find` prints the start directory itself, and the path form of results (relative
  to the argument, as `find` does, is the default).
- S3-4. A walk interrupted by a lost connection: restart once (default) or fail.

**Stage 4**

- S4-1. Request counts through the proxy. The tap cannot parse a connection that starts with a
  SOCKS5 handshake. Default: proxied tests assert function, checksums, the proxy log and the
  server-side connection count, and do not repeat request and flight counts (the code above the
  dialer is the same). Alternative: teach `smbtap` to pass a SOCKS5 preamble through before
  framing.
- S4-2. How much of `tests/slow.rs` is repeated through the proxy. Default: none; the proxy path
  has no latency of its own.

## 15. Questions only the user can decide

None blocks a stage; every point above has a default that stays inside the brief. Two offers from
the approved plan remain open and would add scope if taken:

- a `mv` command (decision 6: rename exists only inside `put`);
- one manual run against a Windows server (the documents otherwise say Samba only).
