# rsmb — fast Rust SMB shell: execution plan

> Approved by the user on 2026-11-17. Copied here unchanged except that machine-specific paths
> were made relative. "I" and "this session" mean the orchestrating session; "you" means the user.
> Stage 0 is specified in full here. For Stages 1–4 this file is input to
> `docs/plans/master-plan.md`, which supersedes the provisional design below once written.

## Context

smbclient-ng (Python/impacket) is slow on high-latency links because of round trips, not CPU.
Goal: a plain Rust client, binary `rsmb`, covering core navigation, reliable `get`/`put`, `tree`,
`find` and SOCKS5, built around one connection, one tree connect per share, a single open/close for
`cd`, cached listings and bounded concurrent requests.

Three ways it has to work (your words, restated):
1. **Developed by AI end to end**: an agent can build, start the environment, test and document
   everything with no human step and no root.
2. **Used interactively by you**: a shell with history, completion, progress, prompts.
3. **Used non-interactively by an AI**: scriptable, never blocks on a prompt, machine-readable
   output, stable exit codes.

Settled with you during planning:
- Project lives in this repository (replaces the hello-world skeleton). **Push to `origin` after
  each stage.**
  The remote is treated as a plain git server: no GitHub Actions, nothing GitHub-specific.
- Test environment must be hostile (very large directories, performance edge cases) and cover
  Chinese, Korean and other non-Latin names.
- Central `AGENTS.md` plus `docs/` for all documentation, kept current as a hard rule; no `CLAUDE.md`.
- Automated testing is local only: one command, run after every major feature addition unless you
  say otherwise; the rule is written in `AGENTS.md`.
- Process: this session is the orchestrator only. It briefs agents, checks what they return,
  validates findings, runs the test gate, commits, pushes and reports. It writes no plans and no
  code. One agent per task, never parallel agents on the same task. One master plan for
  Stages 1–4. Per stage: a separate Fable agent writes the stage plan → one Opus adversary attacks
  it → one Opus agent implements → one Opus agent reviews → every finding validated as true → one
  Sonnet agent fixes → tests → commit → push.

  | Task | Who |
  |---|---|
  | Orchestration, validation of findings, test gate, commit, push, report | this session |
  | Master plan; each stage plan | a separate Fable agent each time |
  | Adversary on a stage plan; implementation; review | one Opus agent each |
  | Fixing confirmed findings | one Sonnet agent |
- `help all` prints every command with examples so an AI that has never seen the program can
  learn it from the binary alone; keeping it complete is a rule for every new feature.

## Decisions I made where the brief left room (say so at approval if any is wrong)

1. `ls` always asks the server and refreshes the cache. Tab completion reads the cache. `refresh`
   drops it. (Rejected: serving repeated `ls` from cache; fast but can show stale data.)
2. `pwd` is added although smbclient-ng has none.
3. Depth counts like Unix tools: `tree -L 1` and `find -maxdepth 1` mean direct children.
   smbclient-ng's `-maxdepth 0` means the same thing, so this differs by one.
4. `get <remote> [local]` writes to the local cwd under the remote basename; `-r` keeps structure
   below the named directory. smbclient-ng recreates the path from the share root; not copied.
   `put <local> [remote]` mirrors this.
5. Overwrite needs `-f`. Without it: prompt in interactive mode, error in non-interactive mode.
6. No `mv` command. Rename exists only as the last step of `put`. One small command if wanted.
7. Non-interactive mode stops at the first failing command. Exit 0 ok, 1 command failed, 2 usage,
   3 connect/auth failed.
8. CLI flags follow smbclient-ng: `-H -P -d -u -p --no-pass -C -S -N`, plus `--proxy` and `--json`.
9. The `cd` target stays "at most two SMB requests" as you wrote it. I also measure round trips
   ("flights"), because wall time follows those. No prefetch of the listing on `cd`.
10. For AI use: `--json` prints one JSON object per line for `shares`, `ls`, `find`, `tree`,
    transfer results and errors. When stdin is not a terminal rsmb never prompts (missing password
    or existing target without `-f` is an error), and prints no colours or progress bars. Results
    go to stdout, diagnostics to stderr. Costs one dependency (`serde_json`).
11. The two Stage 0 spikes are two tasks with one agent each and run side by side in separate
    directories; I re-run both one after the other for the numbers that count.
12. `help all` works inside the shell and as `rsmb --help-all` without a server, so it can be read
    before any connection exists. It covers every command (syntax, flags, at least one example
    each), the start-up flags, non-interactive use, `--json` and the exit codes. With `--json` it
    prints the same table as data. `help`, `help <cmd>`, `help all` and command completion all read
    one command table in the code, so there is a single place to update.

## Established facts (read-only recon, 2026-11-17)

Machine
- Docker 29.9.3 + Compose 5.7.1 usable without sudo; no Podman. `sudo` needs a password, so
  nothing may need host root. Latency injection lives inside container network namespaces.
- Host reaches container IPs on a user-defined bridge directly (checked on an existing network).
  `sch_netem` is on disk for the running kernel 7.3.4 and can autoload; the NAT modules a router
  container needs are already loaded.
- cargo/rustc 1.99.0. Host has Samba `smbclient` 4.25 (cross-check), `smbclientng` 3.1.0
  (baseline), `tshark`. Test networks use 10.213.45–47.0/24 (unused).
- At approval the repository held only a hello-world `Cargo.toml` + `src/`, no commits.

smbclient-ng v3.1.0 (reference, `../smbclient-ng`, read-only)
- Always exits 0, even on auth failure: the baseline benchmark parses output.
- Per scripted line: 1–2 × (new TCP connect + ECHO); interactive: 3. `cd` adds CREATE + ≥2
  QUERY_DIRECTORY + CLOSE, sequential. About 8 RTT scripted, 10 interactive. Tab = uncached listing.
- Transfers there are unsafe (local file truncated before remote open, partials kept under the
  final name, silent remote overwrite). Not copied. No Samba or Docker test assets to reuse.

Candidate crates (from source reading; nothing compiled yet)

| | `smb` 0.12.1 (smb-rs) | `smb2` 0.43.0 |
|---|---|---|
| Caller-supplied transport | public `Connection::from_transport` | public `Connection::from_transport` |
| `TCP_NODELAY` | not set, header and body written separately (#184 open); fixable in our own transport | set |
| Concurrent requests, one connection | yes, `&self` + message-id multiplexing | yes, `Connection: Clone`, `execute(&self)` |
| Compound requests | none | public `execute_compound`; `stat` is 1 round trip; listing is not compounded |
| Known hangs | #185 credits on interim responses dropped (open); #182 directory stream (fixed on git main only) | none found open |
| Guest login on Samba | reported broken (PR #187 open) | supported |
| Reconnect | none | hooks available |
| Maintenance risk | slow; fixes unreviewed for weeks; exact `sspi` pin | breaking release every 4–5 days; raw layer may move behind an `unstable-raw` feature at 1.0 |

Both need the low-level API (not the convenience client) once the transport is ours. Research
leans to `smb2`; the spikes decide, under the rule fixed below.

## Repo layout

```
AGENTS.md         entry point for any agent: index of docs/, the rules, proven principles
README.md         for a human user: what it is, how to run it, headline numbers, Samba-only note
docs/             all documentation, markdown only (plans in docs/plans/)
test.sh           the one command: environment up, environment check, fmt, clippy, all tests
Cargo.toml        workspace: package rsmb (root) + smbtap; spikes excluded
src/              few files: main, session (connection, trees, cache), commands, transfer, walk
tests/            integration tests running the real binary against testenv
smbtap/           request-counting TCP forwarder: library + thin CLI (tokio only)
testenv/          compose.yaml, samba/, wan/, socks/, up.sh, down.sh, check.sh, bench-smbclientng.sh
spikes/smb-rs/    standalone cargo projects
spikes/smb2/
```

## Documentation: `AGENTS.md` + `docs/` (no `CLAUDE.md`)

`AGENTS.md` (standard upper-case spelling) is in the first commit and holds:
1. **Index of `docs/`**: every file with one line on what it answers. A doc missing from the index
   does not exist as far as an agent is concerned.
2. **The documentation rule**, as an obligation: anything new or any substantial change (command
   syntax or behaviour, a fixture, a test target, a dependency, a design decision, a measured
   number) updates the matching file in `docs/` in the same commit; adding, renaming or removing a
   doc updates the index. Work is not finished while the docs describe something else.
3. **The testing rule**: run `./test.sh` after every major feature addition and before any commit
   that closes a stage, unless the user has said not to. A feature arrives with a test that fails
   when the feature breaks; without one it is not done. Local only, no CI service.
4. **The help rule**: every new command, flag or behaviour is added to the command table with at
   least one example, so `help all` stays a complete manual for an AI that does not know the
   program. A test fails when a command has no example or is missing from `help all` or from
   `docs/commands.md`.
5. **The process**: the per-stage loop below, one agent per task, findings validated before fixing.
6. **Principles that have proved good here**, each with one line on why and the evidence. Added
   when something has actually worked in this project, corrected or removed when it stops holding;
   nothing goes in on theory. First candidates come out of Stage 0, e.g. how to set up a test
   environment (no host root, latency at a forwarding hop, fixtures baked at build, environment
   checked with an independent tool before testing against it, fixtures chosen to hurt), fix the
   decision rule before measuring, count round trips rather than seconds.
7. **Working rules**: start/check the environment, fast and full test runs, commit and push per
   stage, keep the code plain, treat the remote as plain git.

`docs/` grows with the work; no empty placeholders.

| File | Written in | Answers |
|---|---|---|
| `test-environment.md` | Stage 0 | topology, endpoints, every fixture and what it targets, start/check, how latency is injected, fallbacks |
| `measuring.md` | Stage 0 | `smbtap`, requests vs flights, how per-command counts and timings are taken, the baseline method |
| `library-decision.md` | Stage 0 | decision rule, full spike tables, the choice and its reasons, patches or pins carried |
| `known-limitations.md` | from Stage 0 | Samba-only testing, what is out of scope, known rough edges |
| `plans/master-plan.md` | after Stage 0 | Stages 1–4: scope, design, order, targets per stage |
| `plans/stage-N.md` | start of each stage | the stage plan, the adversary's findings, what was accepted or rejected and why |
| `reviews.md` | each stage | every review finding, how it was validated, what was done |
| `architecture.md` | Stage 1, extended | session, trees, cache and invalidation, concurrency bound, reconnect |
| `commands.md` | Stage 1, extended | every command: syntax, behaviour, differences from smbclient-ng |
| `scripting.md` | Stage 1, extended | using rsmb from scripts and agents: `-C`/`-S`/stdin, `--json` schema, exit codes, no-prompt rules |
| `development.md` | Stage 1, extended | how to add a command or feature: where code goes, which tests, which docs |
| `testing.md` | Stage 1, extended | test layout, what each target test asserts, fast vs full run, serial tests |
| `transfers.md` | Stage 2 | temp names, overwrite rules, failure and interrupt handling, limits |
| `performance.md` | Stage 1, final at end | targets, measured comparison with smbclient-ng, method |

The README keeps what the brief asks of it (library choice with key measurements and reasons, the
Samba-only statement) in short form and links into `docs/`.

## Automated testing (local)

- `./test.sh` is the single entry: brings the environment up if it is not, runs `testenv/check.sh`,
  `cargo fmt --check`, `cargo clippy`, then every test. `./test.sh fast` runs unit tests and
  fast-endpoint integration tests only (about a minute) for quick feedback while developing; the
  full run adds slow-link, stress, SOCKS and disruptive tests.
- The suite only grows: each stage adds tests for its features and re-runs all earlier ones, so a
  new feature that breaks an old one fails the gate.
- Tests print what they expected in protocol terms (requests, flights, connections, checksums), so
  an agent reading a failure knows what regressed.
- No stage is committed with a failing or skipped test unless the report to you says which and why.

## Stage 0 — test environment, spikes, library decision

Runs from this plan once approved (its design has already been attacked; changes listed below).
Order: one Opus agent builds environment + `smbtap` → I verify → two Opus agents write the spikes →
I re-run the measurements → one Opus review of environment and measurements → findings validated →
one Sonnet agent fixes → `./test.sh` → commit → push.

### Test environment: `./testenv/up.sh` (build once, then `docker compose up -d --wait`)

| Service | Address | Role |
|---|---|---|
| `samba` | 10.213.45.10:445 | fast endpoint; multichannel off so nothing can bypass the tap |
| `wan` | 10.213.45.30:445 | slow endpoint: router container (DNAT/SNAT to samba on an internal net, `netem delay 100ms` on both interfaces) = 100 ms each way at a forwarding hop, no TCP split. One extra port that can be blackholed on demand |
| `wan-bad` | 10.213.45.31:445 | same image: `delay 150ms 40ms loss 1% rate 20mbit`, a bad VPN |
| `samba-edge` | 10.213.45.11:445 | same image as samba: 16 credits, 64 KiB read/write/transact, 300 shares, multichannel on |
| `socks` | 10.213.45.20:1080 / :1081 | microsocks (pinned tag), no-auth / user+password; second leg on `priv` |
| `samba-private` | `fileserver.rsmb.test`, `priv` only | reachable only through `socks` |

- Host talks to container IPs directly, no published ports: no userspace TCP split hiding
  Nagle/delayed-ACK effects. Fast and slow endpoints exist at once; tests pick by address.
- `priv` and the router's back network are `internal` with `gateway_mode_ipv4: isolated`, so the
  host has no route in. Proof of proxying is positive and negative: the microsocks log line
  `connected to fileserver.rsmb.test:445` (name as sent by the client, resolved by the proxy); the
  name must not resolve on the host (precondition checked); `hosts allow` admits only the proxy;
  a direct attempt by IP must fail in the expected way.
- Samba: user `tester`/`testpass`, workgroup `TESTDOM`, guest mapping on, SMB2 minimum. Shares
  `share` (rw), `readonly`, `guest`, `encrypted` (`smb encrypt = required`), `tiny` (10 MiB tmpfs).
- Fixtures are created by `testenv/samba/fixtures.sh` at image build, so all three Samba containers
  share one layer and start in seconds. Alpine tag pinned, `smbd -V` recorded with results.
  Checksums always taken server-side via `docker exec … sha256sum`.
- Fallbacks, none needing host root: router fails → extra network on `samba` with `netem 200ms` on
  that interface (same round-trip time); netem cannot load → `up.sh` says why (usually a kernel
  upgrade without reboot); last resort a delay option in the tap, documented as optimistic.

### Fixtures: built to hurt

Each fixture targets one way a client loses time or data. Under `share` unless noted.

Scale (round trips, memory, bounded concurrency)
- `many/` 10,000 files. `huge/` 100,000 files. Opt-in build arg: `xl/` 1,000,000 files.
  Shows whether listings use large query buffers (about 220 round trips at 64 KiB vs 2–3 at
  8 MiB for `huge/`), whether `ls` and Tab stay usable, and what the cache costs in memory.
- `longnames/` 2,000 entries with ~250-byte names: most bytes per entry.
- `fanout/` 2,000 sibling directories with one file each: unbounded traversal or credit
  exhaustion shows here.
- `tree/` breadth 4 × depth 4 (340 dirs) for routine traversal timing. `deep/` 12 levels.
  `chain/` 60 levels: depth cannot be parallelised.
- `smallfiles/` 2,000 files of 1–4 KiB in 20 directories: per-file overhead of recursive `get`
  and `put` at latency (sequential would take about 20 minutes at 100 ms each way).

Sizes (chunking and offsets)
- `large/big.bin` 1.1 GiB and `large/over4g.bin` 4.3 GiB, deterministic pseudo-random data with
  lengths not divisible by any chunk size: a wrapped 32-bit offset or a misordered chunk changes
  the checksum. `medium.bin` 50 MiB.
- `sizes/`: 0, 1, 65535, 65536, 65537, 1 MiB ± 1, 8 MiB ± 1 bytes.

Names — files and directories, also nested several levels deep
- East Asian: Chinese simplified and traditional, Korean Hangul both precomposed and as
  decomposed jamo (same visible name, different bytes), Japanese kana with combining marks, rare
  Han characters outside the Basic Multilingual Plane (surrogate pairs, e.g. 𠮷), full-width
  Latin, and a name containing the ideographic space U+3000 (must not be split as whitespace).
  `cjk/` holds 1,000 such entries so sorting, completion and `find -iname` run at size.
- `special/`: spaces, umlauts, Cyrillic, emoji, the same name in NFC and NFD, apostrophe and
  double quote, leading dash, brackets and other glob characters, two names differing only in case.
- Everything in `cjk/` and `special/` must survive list → Tab-complete → `cd`/`get` → local name →
  `put` back with the name byte-identical on both sides.
- `mangled/`: names Samba cannot present as-is: on-disk bytes in EUC-KR, GBK and Shift-JIS,
  characters Windows forbids, trailing dot. The server returns mangled `~` names; the client must
  list and fetch what it is given without crashing. Unpaired UTF-16 surrogates (possible from
  Windows, impossible on Samba) get a unit test on the name-decoding path.

Hostile structure
- `forbidden/` (root, 0700) at top level and inside `tree/`; a root 0711 directory (can be
  entered, cannot be listed); an unreadable file inside `tree/`.
- `loops/self -> .` and `loops/up -> ..`: Samba follows them, so a recursive walk sees endless
  depth. `tree`, `find`, `get -r` must terminate and say why. A dangling symlink as well.

Hostile server and link: `samba-edge`, `tiny`, `wan-bad`, the blackhole port, `docker pause samba`
(server accepts TCP but never answers), and the tap cutting a connection at any moment.

Cost: one image layer of about 6 GiB, about a minute to build once.

### Keeping the measurements honest

- `wan` has a healthcheck that fails when netem or NAT is missing, so the slow endpoint cannot
  silently run at 0 ms. `testenv/check.sh` asserts TCP connect time of about 200 ms slow and under
  5 ms fast, and checks fixture behaviour with the host's `smbclient` before any Rust code touches
  it (`forbidden/` denies, `readonly` refuses writes, entry counts match).
- Every timing row carries the measured connect RTT; `tc -s qdisc` must show no drops after a run.
- Timings use one method for both clients: (time for 10 commands − time for 0) ÷ 10, median of 5.
  rsmb is also measured with 1 s pauses between commands, because TCP restarts slow after idle and
  a large listing then costs extra round trips that back-to-back runs hide.
- Bulk throughput at the slow endpoint is reported against a raw TCP ceiling through the same
  router (iperf3), so a kernel buffer limit is not blamed on the client.
- Large-read runs evict the server page cache first; the tap counts interim `STATUS_PENDING`
  responses. Zero interims means the stall test for issue #185 was inconclusive, not passed.
  Crate order alternates so neither always reads from a warm cache.
- TCP connections are counted twice: by the tap and independently on the server side, and against
  `samba-edge` where multichannel is advertised.
- The tap's numbers are compared once with a `tshark` capture of the same run.

### `smbtap`: counting without trusting the client

TCP forwarder on 127.0.0.1:0 → upstream, `TCP_NODELAY` on both sockets. Parses both directions:
4-byte framing, SMB2 header chains (command, message id, compound position, status, credits);
SMB1, encrypted and compressed frames counted as opaque. Reports TCP connections, requests,
**flights** (client sends that follow a server response: the number wall time follows), interim
responses, and whether anything is still outstanding. Can drop live connections. CLI for spikes and
the baseline; library used in-process by rsmb's integration tests. Counting runs through the tap at
the fast and the slow endpoint (a library may behave differently when responses are slow); wall
times never go through the tap. It has its own unit tests on recorded frames.

### Spikes (`spikes/smb-rs`, `spikes/smb2`)

Each uses the crate's custom-transport entry point with our own transport (tokio `TcpStream`,
`TCP_NODELAY`, one write per frame). Every step runs under a timeout, so a hang is recorded as
`TIMEOUT`. Gates first; comparison tables only for a crate that passes them.
1. Connect + NTLM with user/password/domain; guest; anonymous; one run on the `encrypted` share;
   one run through SOCKS5 to `fileserver.rsmb.test`.
2. Share list on `samba` and on `samba-edge` (300 shares, hidden ones included, type and comment).
3. Directory-exists check: which access mask gives the right answer for an existing directory, a
   missing one, a file, `forbidden/` and the 0711 directory; plain CREATE+CLOSE and compound;
   flights until the call returns and requests once settled.
4. Listings: small, `many/`, `huge/`, `longnames/`, default and largest accepted buffer; result
   compared as a set with the server's own listing; requests, flights, time, peak RSS; whether a
   large multi-credit query works on the first `ls` after connect.
5. Read: every file in `sizes/`; `big.bin` streamed with concurrent chunks, five runs, sha256
   against the server, peak RSS, throughput, interims seen; a block above 4 GiB from `over4g.bin`.
6. Write: 1.1 GiB streamed upload, three runs, checksum; rename with and without replace, delete,
   mkdir (works or not, no timing).
7. Concurrency: breadth-first walk of `tree/` and `fanout/` at the slow endpoint with 16 in flight
   versus sequential, after credits have warmed up; exactly one TCP connection.
8. `samba-edge`: 64 parallel requests and a large transfer; slower is fine, stuck is not.
9. Names: `cjk/` and `special/` through list, open and create; `mangled/` through list and open.
10. Failure: tap cut while idle and mid-transfer; blackhole; paused server; each must give an
    error in bounded time. Dropping a request mid-read, then timing the next one. Idle for twice
    the keepalive interval: does the library send anything unasked.
11. Error typing: access denied, not found, write to read-only share, disk full on `tiny`.
12. Cost: crate count, clean build time, size of any patch or git pin; the `smb2` spike compiled
    against 0.40–0.43 to count lines broken by API changes; a note from the docs on whether
    NT-hash and Kerberos login exist, for later.
13. Smoke run over `wan-bad`.

Each spike leaves one re-runnable command printing its table. `testenv/bench-smbclientng.sh`
produces the baseline: times as above, plus TCP connections and requests through the tap.

### Decision rule, fixed before measuring

Hard gates. Failing one puts a crate out unless a cumulative patch of about 100 lines against
upstream fixes all its failures (a pin to an unreleased upstream commit counts as zero lines but is
recorded as a risk):
- custom transport works end to end through SOCKS5 with a proxy-resolved hostname;
- requests overlap on one connection: the 16-wide walk of `tree/` at the slow endpoint takes close
  to (levels × listing round trips), not (directories × listing round trips);
- 1.1 GiB download ×5 and upload ×3 finish, checksums match, no run exceeds three times the median;
- directory check returns after one flight and settles at ≤ 2 requests, with the right answer for
  all five cases in step 3;
- user/password/domain, guest and anonymous logins work against Samba;
- all 300 shares returned, hidden ones included;
- no hang on `samba-edge`; an error within bounded time under cut, blackhole and paused server;
- no requests sent unasked while idle, or the keepalive can be switched off.

Then compare, in this order, differences under 10 % counting as a tie: flights for `cd`, small
`ls`, `many/`, `huge/`; transfer throughput and peak memory; patch burden; API-stability risk;
dependency weight. Both fail the gates → I stop and report to you.

### Stage 0 output

`AGENTS.md`, `test.sh`, `docs/test-environment.md`, `docs/measuring.md`,
`docs/library-decision.md`, `docs/known-limitations.md`; README with the choice, key measurements
and reasons, and the statement "tested against Samba only, no Windows claim". Commit, push, report
to you with real output.

## After Stage 0 — master plan, then the same loop for every stage

**Master plan** (`docs/plans/master-plan.md`), written by one Fable agent from the Stage 0
measurements, your brief and the provisional design below: what each of Stages 1–4 delivers, the
design they share, the order, the targets and tests each stage must meet. One document, no detail
that belongs in a stage plan. I check it against your brief before any stage starts.

**Per stage (1 to 4), one agent per task, in sequence:**
1. **Stage plan** — a separate Fable agent writes `docs/plans/stage-N.md` from the master plan,
   `AGENTS.md`, the docs and the code as it stands. It starts fresh from the repo, which also
   tests whether the docs are good enough for an agent that was not here before.
2. **Adversary** — one Opus agent gets the stage plan and the evidence (measurements, tap logs,
   requirements), not my opinion, and tries to break it. I validate each finding (reproduce it,
   or show from code or measurements that it holds). The Fable planning agent revises the plan
   with the true ones; the rest are recorded with the reason for rejection.
3. **Implementation** — one Opus agent implements the revised plan, tests and docs included.
4. **Review** — one Opus agent gets the code and the requirements, not a summary of how it went.
5. **Validation of findings** — nothing is fixed on a reviewer's word. Each finding is confirmed
   first: for behaviour, by a test that fails because of it; for design or docs, by checking the
   code. Unconfirmed findings are recorded as rejected with the evidence.
6. **Fix** — one Sonnet agent fixes the confirmed findings; the failing tests from step 5 stay in
   the suite.
7. **Gate** — I run `./test.sh` (full) myself, read the diff, check `docs/` and the `AGENTS.md`
   index against the code, add to the principles what the stage showed, commit, push.
8. **Report to you** — what works, real test output, findings and what was done about each,
   anything skipped or failing. I carry on to the next stage unless the stage fails its own tests.

Stage 4's review covers the whole codebase, not only the SOCKS5 change, since it is the last look.
I stop and ask only if neither crate passes the gates or something needs a scope change.

## Provisional design for Stages 1–4 (input to the master plan)

- **Runtime**: main thread runs `rustyline`; SMB work runs on a small tokio runtime via `block_on`.
- **Session**: one transport (we dial: direct TCP with `TCP_NODELAY` and keepalive, or SOCKS5),
  one authenticated session, a map share → tree connect kept for the session (IPC$ included for
  `shares`). No liveness probe; a request timeout catches a silent link. A connection-level
  failure triggers one reconnect; read-only commands are retried once, mutating ones report it.
- **`cd`**: resolve path locally, then one directory-only CREATE + CLOSE, compound if the crate
  supports it, otherwise the prompt returns after the CREATE and the CLOSE follows in the
  background. Never a listing. Root and `..` to root cost nothing.
- **`ls`**: one open, QUERY_DIRECTORY with the largest buffer the server grants, smallest info
  class carrying size, times and attributes. Output streamed for very large directories.
- **Cache**: `(share, path) → entries`. Filled by `ls` and by Tab. Parent invalidated on `put`,
  `rm`, `mkdir`, `rmdir`, rename; subtree dropped on `rmdir`. `refresh` clears all.
- **Completion**: commands, remote paths from the cache (one listing on a miss), local paths for
  `put`/`lcd`/`lls`. Tokeniser is POSIX-shlex-like and splits on ASCII whitespace only.
- **Two front ends, one command layer**: the interactive shell and the non-interactive runner
  (`-C`, `-S`, piped stdin) call the same command functions; only prompting, progress and output
  format differ. `--json` is an output mode of that layer, not a second implementation.
- **Transfers**: bounded window of concurrent chunk requests, written in order, memory bounded by
  window × chunk; small files in a recursive transfer run several at a time under the same bound.
  `get` writes `name.rsmb-part` locally and renames on success; `put` writes a remote temp name
  and renames server-side (replace only with `-f`). Size checked at the end. Failure or Ctrl-C
  removes the temp file, or names it in the error when it cannot be removed. In recursive `get`,
  server-supplied names are validated before use as local paths (no `..`, separators, absolute).
- **`tree` / `find`**: one traversal routine, a semaphore bounding in-flight listings, a depth cap
  that stops symlink loops and is reported when hit, denied directories collected and reported at
  the end. `find`: `-name`/`-iname` glob, `-type f|d`, `-size [+|-]N[BKMGTP]`, `-maxdepth`.
- **SOCKS5**: `--proxy socks5://[user:pass@]host:port` via `tokio-socks`, target passed as a
  hostname so the proxy resolves it; reconnects use the same dialer.
- **Dependencies**: the SMB crate, `tokio`, `clap`, `rustyline`, `tokio-socks`, `indicatif`,
  `serde_json`, a password prompt crate. Dev only: `smbtap`, a PTY crate for the Tab test.

Tests that enforce the speed targets
- One TCP connection: a scripted session touching every command; one accept in the tap and one on
  the independent server-side count.
- `cd` ≤ 2 requests: commands fed one at a time on stdin; the tap is read once nothing is
  outstanding and the line has been quiet, so a background CLOSE is charged to `cd`, not to the
  next command. Per-command counts must add up to the session total.
- Second Tab = 0 requests: real PTY, tap read before and after.
- `cd` and `ls` at the slow endpoint: asserted as a bound in flights, times printed and set beside
  the smbclient-ng baseline.
- Transfers: sha256 of source vs result taken server-side; interrupted and failed transfers leave no
  file under the final name; overwrite refused without `-f`; non-zero exit on failure.
- Reconnect: tap drops the connection; next read-only command succeeds with exactly one new connect.
- Non-interactive contract: with stdin not a terminal nothing ever waits for input; `--json` output
  parses line by line; exit codes match `docs/scripting.md`.
- Help completeness: every command in the table has an example, appears in `help all` and in
  `docs/commands.md`; `rsmb --help-all` runs with no server; every example in `help all` is
  executed against the test server and must succeed, so the manual cannot drift from the program.
- SOCKS5: the Stage 1–3 tests re-run against `fileserver.rsmb.test` through the proxy, both
  listeners, the proxy log checked, plus the negative control.

Stress tests against the hostile fixtures (bounds in flights or "terminates", not seconds)
- `ls huge` at the slow endpoint stays within a small fixed number of flights; Tab in `huge/`
  answers from cache the second time; peak RSS bounded.
- `tree`/`find` over `fanout/` and `tree/` at the slow endpoint: flights near (listings ÷
  concurrency limit); in-flight requests never above the limit (checked in the tap).
- `tree`, `find`, `get -r` over `loops/` terminate and report the cut-off.
- `get -r` and `put -r` of `smallfiles/` at the slow endpoint finish within a bound sequential
  transfer cannot meet; every checksum matches.
- `over4g.bin` down and up: checksum matches; every file in `sizes/` round-trips exactly.
- `cjk/` and `special/`: listed, completed, entered, fetched, uploaded, deleted by name; names
  byte-identical server-side and locally. `mangled/`: listed and fetched without error.
- `samba-edge`: navigation, a traversal and a transfer complete without stall.
- `tiny`: upload fails with disk-full, nothing left under the final name, exit status non-zero.
- Blackhole and paused server: the command fails within the request timeout, the shell survives.
- `wan-bad`: smoke run, numbers printed.
Tests that disturb the whole environment (pause, blackhole) run serially in their own test binary.
The 1,000,000-entry directory is opt-in; everything else runs in the full suite.

## First actions after approval

1. Git plumbing by me: branch renamed to `main`, `origin` added.
2. `AGENTS.md` with the rules you gave in this conversation (documentation, testing, help, process,
   roles), written by me because they are your instructions and only this session heard them.
   Everything agents learn afterwards (docs index, principles) they add themselves.
3. Session memory note: this project uses `AGENTS.md` (not `CLAUDE.md`), read it first, and the
   roles and per-stage process above.
4. Stage 0 starts with the Opus agent for environment + `smbtap`, which also replaces the
   hello-world skeleton with the workspace and writes `test.sh`. First commit and push follow my
   check of its work.

## What the design review of Stage 0 changed

Taken: multichannel off on the main server and an independent connection count (a multichannel
client could have bypassed the tap and passed the one-connection test falsely); tap parses both
directions and reports flights; router healthcheck and RTT assertion; isolated gateway mode and the
proxy-log proof for SOCKS; quiescence instead of a `pwd` marker for per-command counts; think-time
measurement; raw-TCP ceiling; cache eviction and interim counting for the stall test; blackhole and
paused-server cases; fixtures baked at build; access-mask check for `cd`; neutral gates so the rule
does not only test the known defects of one crate.

Rejected: driving commands through an in-process API instead of the binary (the brief asks for
tests on the real binary); measuring interactive latency during a running transfer (one command
runs at a time); dropping the anonymous-login check (the brief asks for guest and anonymous).

Open offer, not planned: a `windows_default` Docker network exists on this machine. If that is a
Windows guest and you want one manual run against it, say so; otherwise the docs say Samba only.

## Verification (end to end)

```
./test.sh                                # environment up + check, fmt, clippy, every test
./test.sh fast                           # quick loop while developing
./testenv/bench-smbclientng.sh           # baseline numbers for the comparison table
cargo run -- -H 10.213.45.30 -u tester -p testpass -d TESTDOM            # interactive, 100 ms each way
cargo run -- -H 10.213.45.10 -u tester -p testpass -N --json -C 'use share' -C 'ls many'   # as an agent would
```

Final report: measured comparison with smbclient-ng, every review finding with its validation and
outcome, known limitations.
