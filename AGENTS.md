# AGENTS.md

Entry point for any agent working in this repository. Read this file first, then the documents it
points to for your task. There is no `CLAUDE.md` and there must not be one; this file is the single
place for working rules.

## What this project is

`rsmb` is an SMB shell client in Rust that replaces the user's use of smbclient-ng. smbclient-ng is
slow on high-latency links because of network round trips, so the design is about round trips: one
TCP connection, one tree connect per share, one open/close for `cd`, cached directory listings,
bounded concurrent requests.

It has to work in three ways, and every change is judged against all three:

1. **Developed by AI end to end.** An agent can build, start the test environment, run the tests
   and update the documentation with no human step and no root access.
2. **Used interactively by a person.** A shell with history, completion, progress and prompts.
3. **Used non-interactively by an AI.** Scriptable, never waits for input, machine-readable output
   with `--json`, stable exit codes, and a complete manual in the binary (`help all`).

The requirements are in [`docs/brief.md`](docs/brief.md), including the amendments at its end.

## Rules

These are obligations, not advice. A task that breaks one of them is not finished.

### 1. Keep the documentation current

Everything about this project is documented in markdown under `docs/`. Anything new and any
substantial change updates the matching document **in the same change**: command syntax or
behaviour, a fixture, a test target, a dependency, a design decision, a measured number, a known
limitation. If you add, rename or remove a document, update the index below. If you find a document
that no longer matches the code, fix it or report it; do not leave it.

Work is not finished while the documentation describes something else than the code does.

### 2. Keep `help all` complete

`help all` (and `rsmb --help-all`, which needs no server) prints every command with its syntax,
flags and at least one example. It exists so that an AI which has never seen the program can learn
to use it from the binary alone. Every new command, flag or behaviour is added to the command table
with an example. A test fails when a command has no example, or is missing from `help all` or from
`docs/commands.md`.

### 3. Test after every major feature

Run the automated tests after every major feature addition and before any commit that closes a
stage, unless the user has said not to. A feature arrives together with a test that fails when the
feature breaks; without such a test it is not done. Testing is local only: no CI service.

The single command is `./test.sh`.

### 4. No root, no changes to the host

Nothing may need `sudo` or change the host's network, kernel or system configuration. Everything
privileged happens inside containers.

### 5. Keep the code plain

No plugin system, no abstraction layer for something that has a single implementation, nothing
built for features outside the stages in the brief. Few files, short dependency list.

### 6. Git

The remote is a plain git server. Use nothing that is specific to a hosting service: no hosted CI,
no service-specific tooling. Only the orchestrating session commits and pushes, once per stage,
after the test gate has passed. Agents do not commit or push unless their brief says so. From
Stage 5 on, each stage is also pushed to the public repository (remote `rsmb`) as one commit on top
of `rsmb/main` with the same tree; commit messages carry no AI attribution (no `Co-Authored-By`
trailer, no session link).

## Process and roles

One master plan describes Stages 1 to 4; Stage 5 (Kerberos) has its own plan. Each stage runs the
same loop. One agent per task; never several agents in parallel on the same task.

| Step | Task | Who |
|---|---|---|
| 1 | Write the stage plan in `docs/plans/stage-N.md` | a separate Fable agent |
| 2 | Attack the stage plan | one Opus agent (adversary) |
| 3 | Implement the revised plan, with tests and documentation | one Opus agent |
| 4 | Review the result against the code and the requirements | one Opus agent |
| 5 | Validate every finding as true before anything is fixed | orchestrating session |
| 6 | Fix the confirmed findings | one Sonnet agent |
| 7 | Run the test gate, check docs against code, commit, push, report | orchestrating session |

The orchestrating session briefs agents, checks what they return, validates findings, runs the
gate, commits, pushes and reports to the user. It writes no plans and no code.

A finding is validated by reproducing it: for behaviour, a test that fails because of it; for
design or documentation, the code itself. Findings that cannot be confirmed are recorded as
rejected with the evidence. Tests written to confirm a finding stay in the suite.

The master plan is written by a Fable agent as well, after Stage 0.

Effort levels set by the user: planning agents (master plan, stage plans) run at `high`; the fix
agent runs at `high`. The other roles run at their default effort.

## Documentation index

| Document | Answers |
|---|---|
| [`docs/brief.md`](docs/brief.md) | What the user asked for, and the later amendments that override parts of it |
| [`docs/plans/approved-plan.md`](docs/plans/approved-plan.md) | The plan the user approved: decisions taken, facts established before any code, Stage 0 in full (test environment, fixtures, measurement method, spike protocol, decision rule), the documents expected from each stage, and a provisional design for Stages 1–4 that the master plan has superseded |
| [`docs/plans/master-plan.md`](docs/plans/master-plan.md) | Stages 1 to 4: what the three uses demand of the design, the shared design (files, dependencies and the `smb2` pin policy, runtime, session and dialer, `cd`, listing, cache, completion, timeouts and reconnect, concurrency and credits, errors), the speed targets and the test that enforces each, how `./test.sh` grows and its time budget, per stage the scope, deliverables, numbered targets with fixture and endpoint, exclusions and exit criteria, the risks carried from Stage 0, the open questions each stage plan must settle, and what changed against the provisional design. A stage plan starts from this document |
| [`docs/plans/stage-1.md`](docs/plans/stage-1.md) | The plan for Stage 1 (connection and navigation): what was measured while planning (listing shapes with requests, flights and times; file ids; reconnect, share list, SOCKS5 and line-editor facts), the decision on every Stage 1 open question, the departures from the master plan, the design down to types and signatures (flags and start-up, runtime and signals, dialer, login, tree map, errors and exit codes, `cd`, the listing routine and its buffer plan, cache, share list, timeouts and reconnect, command table, the `--json` objects, `help all`, tokeniser and completion, the interactive shell), the test harness, `./test.sh` and its budget, every unit and integration test with what it asserts and the master-plan row it covers, `bench-rsmb.sh`, the ordered tasks, the documents to write, what Stage 2 receives, the assumptions that were not verified, and the adversary's twelve findings with what changed for each |
| [`docs/plans/stage-2.md`](docs/plans/stage-2.md) | The plan for Stage 2 (transfers and file operations): what was measured while planning (requests and flights per file, rename and delete answers on Samba, sixteen small files and sixteen walk listings in flight on a new connection, what a dropped, cut, disk-full or changing transfer does, the cost of a local sync), the decision on every Stage 2 open question, the departures from the master plan, the design down to types and signatures (`Remote`, the registry of temporary files and its cleanup, the stream path and the small path, temporary names and the rename on each side, `get`, `put`, the overwrite rules, the checks at the end, the walk core, recursive transfers, the failure matrix, timeouts and credits in a transfer, `cat`, `mkdir`, `rm`, `rmdir`, the local commands, cache invalidation, the table entries and the prompt, progress, the `--json` objects), the harness additions, the budget, every unit and integration test with what it asserts and the master-plan row it covers, the `transfers` mode of `bench-rsmb.sh`, the ordered tasks, the documents to write, what Stage 3 receives, the assumptions that were not verified, and the adversary's seventeen findings with what changed for each |
| [`docs/plans/stage-3.md`](docs/plans/stage-3.md) | The plan for Stage 3 (`tree` and `find`): what was measured while planning (what a walk costs in requests, flights and time at every endpoint, the whole share against the server's own `find`, results in queue order against completion order, an interrupt and a cut in the middle, memory of a kept and of a streamed walk, the cache's eviction at its cap, a prototype of the pattern matcher against the server's `find` on 60 patterns, fixture counts, what `tree` costs in smbclient-ng), the user's decisions and the decision on every Stage 3 open question, the departures from the master plan, the design down to types and signatures (flags that take a value, the walk's stop and its queue order, what a walk stores in the cache and the cache's new accounting, depth, the pattern matcher with case, Unicode and quoting, sizes, `find`, `tree` and its drawing, the end of a walk with denied and skipped directories and the exit status, interrupt, reconnect, credits, memory bounds, the table entries, the `--json` objects), the harness additions, the budget, every unit and integration test with what it asserts and the master-plan row it covers, the `walk` mode of `bench-rsmb.sh`, the ordered tasks, the documents to update, the new fixture `links/`, what Stage 4 receives, the assumptions that were not verified, the adversary's eight findings with what changed for each, and the deviations during implementation with their evidence |
| [`docs/plans/stage-4.md`](docs/plans/stage-4.md) | The plan for Stage 4 (SOCKS5 proxy, and closing the project): what was measured while planning (what exists since Stage 1 and what is tested, every command through both proxy listeners, requests and flights through the proxy beside direct with a tap that relays the SOCKS5 handshake, cut, reconnect, interrupt and the 1.1 GiB file through the proxy, every proxy failure with time, kind, exit code and message, a silent proxy, a silent server and a blackholed target, who resolves the name and the two probes that show it, IP and IPv6 targets, encoded credentials, schemes, where the password shows, what the proxy costs at the fast and the slow endpoint and why microsocks slows bursts, `--json` on a terminal, smbclient-ng's transfers, the stale statements found, and after the review the connections one run of `tests/fast` opens and that rsmb does not fall back to a direct connection), the decision on both Stage 4 open questions and on sixteen further points, the departures from the master plan, the design down to signatures (the URL and a usage message that never repeats it, the dial limit as designed, max(10 s, `--timeout`), which was **not built** (the dial keeps a fixed 10 s, deviation V1 at the end of the plan), its stage and the proxy's errors in our words, no fallback, the banner, `--json` never starting the line editor, the manual's topic, `smbtap` with a proxy as upstream), the harness (`Ep::Proxied`, `route`, `fast()` and the second run of `tests/fast.rs`), `./test.sh` with the log count that proves that run went through the proxy, its budgets of 150 and 600 s and the estimate, every unit and integration test with what it asserts and the master-plan row it covers, what is not in the gate, the final benchmark run and the final comparison table (the proxied figures as measured while planning), the cleanup before the review of the whole codebase, the ordered tasks, the documents in their final form, the assumptions that were not verified, and the adversary's eleven findings with what changed for each; after the stages, the `shares` access column (the request, what Samba reports per share, the cost before and after, keep or disconnect, no flag) |
| [`docs/plans/stage-5.md`](docs/plans/stage-5.md) | The plan for Stage 5 (Kerberos login from the ticket cache, `-k`): what was measured while planning (the DC as a container and its provisioning, a Kerberos login through the crate direct and through the proxy, which ticket-granting tickets the crate can use, reaching the KDC with the crate alone and with a loopback relay, what each failure returns, a ticket's end as the session's end, what caches look like, smbclient-ng with the same caches, what the stage adds to the gate), the decisions K1 to K18 (the relay, the forwardable-and-renewable limit, kinds, where the KDC is, who the session is, expiry, the cache file and its owner, `-k` renamed from `--keep-going`), the departures from the orchestrator's design, the design down to signatures (flags, the cache and `pick_ticket`, the KDC's address and `krb5.conf`, the branch in `login`, the relay with one timer at a time, the KDC's failures in our words, a session whose ticket ends, the server's name, banner and `--json`, the manual), the test environment (`samba-dc`, `mkcache`, `check.sh`), the harness, every unit and integration test (F71–F76, S71, S72, X71, X72, H2), the budget, the ordered tasks, the documents, the assumptions not verified, the adversary's seventeen findings with what changed, and the deviations during implementation with their evidence |
| [`docs/architecture.md`](docs/architecture.md) | How rsmb is built: the six source files and what each holds, threads, runtime and signals (`BUSY` held across the cleanup), start-up and the environment variables read, the session (dialer, the receive half and why it reports progress, login and the send timeout, Kerberos: the cache read before the dial, `pick_ticket` and expiry, the pinned principal, `kerberos_setup`, the KDC relay over `open_tcp` with one timer at a time, `kdc_for`, the error wording, tree map, paths and DFS), `cd`, the listing (plan, two phases, parser, cost, memory), the cache (units, the ordered index, what a walk stores, two directories with one folded key), the share list and the access probes of `shares`, timeouts, dead connections, reconnect (and `find`'s rule), a Kerberos session that ends with its ticket, interruption and `abandon`, the completer's bound, errors and exit codes, the command layer (`Args`, flags with a value, `Out::diag` and `written`) and the overwrite prompt, the shell, `Remote` with the registry, the writer slot, `Alive`, `finish` and `cleanup`, the transfer engine and `LocalTemp`, the walk (queue order, the stop, the parent seeded below the root only), `tree` and `find` (`run_walk`, memory, the matcher, the end report), cache invalidation; the dialer through a SOCKS5 proxy (the 10 s for the whole dial, how far it got, the proxy's errors in our words, no fallback, the name resolved by the proxy), the banner, `--json` never starting the line editor |
| [`docs/transfers.md`](docs/transfers.md) | Every rule of `get`, `put` and `cat`, each with the test that enforces it (a Kerberos ticket that ends in a transfer included): the two paths and the threshold, memory and requests in flight, temporary names and the lock, the rename on each side, the overwrite table, what "complete" means and what is not detected, recursive transfers (phases, what is not transferred, exit status, fatal errors, interrupts), the cleanup and the failure matrix, leftovers and how to remove them, timeouts and credits in a transfer, the limits |
| [`docs/commands.md`](docs/commands.md) | Every start-up flag (`-k` for Kerberos, `--keep-going` in its long form only, and what that does to an old command line); the scratch context of the examples; how commands are given and the overwrite prompt; quoting, paths, flags inside commands and local paths; completion (remote and local); one section per command (19) with syntax, cost in requests, output, errors and examples (`tree` with its drawing and limit; `find` with its tests, the pattern table and how it differs from the server's `find`, the size rules, the order, the end of a walk and its exit status; `shares` with its access column and what it does not tell); flags that take a value; exit codes; timeouts and reconnect, which commands are not run again (`find` once it has printed); the proxy (the URL and its one usage message, names resolved by the proxy, every connection through it and no fallback, credentials, every failure message with its kind and exit code, time, cost, the banner); `--json` on a terminal; Kerberos (the cache and its owner, who the session is, the server's name, with a service ticket or a TGT only, where the KDC is, the forwardable-and-renewable rule, the proxy, expiry and a session's end, every failure message with its kind, cost, the banner, the two environment variables); differences from smbclient-ng. A test ties it to the command table and to the proxy's and Kerberos' messages |
| [`docs/scripting.md`](docs/scripting.md) | Driving rsmb from a program: the three ways to give commands and their order, when stdin is read, `-N`, `--keep-going`, never prompting, that `-k` changed its meaning, exit codes, every `--json` object with its fields and example lines (`transfer`, `summary`, `content`, `local_entry`, `lpwd`, `path` and `left_behind` on `error`, `depth` on `entry`, `denied`, `skipped`, `walk_summary`), the order of objects in a recursive transfer and in a walk, "no `walk_summary` means not whole", the order of `find`'s lines, the `done` rule, rsmb as a coprocess, bash and python examples, finding files by pattern and fetching them, uploading a directory and checking the summary; `--json` on a terminal (no line editor, the non-interactive rules for typed lines); a proxy failure as an `error` object and a run through a proxy; with Kerberos: `KRB5CCNAME` and `KRB5_CONFIG`, a failure as an `error` object and what a script does with each of its two kinds |
| [`docs/development.md`](docs/development.md) | Build and test commands, a Kerberos login by hand, running one test, reading a failure, where a new command (and its examples' scratch context), flag (with or without a value), JSON type, sending method, `Remote` method or `find` test goes and the shape of every command that uses a `Remote`, where `smb2` may be used, that a test in `tests/fast.rs` names `fast()` (or `dc()`) and never an address, running one test through the proxy, the pin policy, the dependencies, where throwaway measurements go |
| [`docs/testing.md`](docs/testing.md) | `./test.sh` and its two modes, steps, locks, budgets (150 and 600 s), counted time and the measured step times; the second run of `tests/fast` through the proxy (`RSMB_TEST_VIA`, `fast()`) and the log count that proves it went through; what goes into which test binary; the harness (`Client`, `Reply`, `Pty`, `ptyrun.py`, scratch directories, trees and checksums on both sides, waits on observed events, the credit balance); how counts are taken; the per-command counts the tests assert, transfers and walks included; every test with what it protects and which row it enforces (the proxy's and Kerberos' unit, fast, slow and serial tests included); `server_find` and the BusyBox reference; the harness's endpoints and routes (`Ep::Proxied`, `route`, `host`, `Client::through`) and the rule that a test with its own `--proxy` never uses the route; the Kerberos harness (`Ep::Dc`, `Ep::DcProxied`, `dc()`, `spn`, `krb_cache`, `expired_copy`, `krb5_conf`, `kdc_requests`, `Client::kerberos`, the `KRB5_CONFIG` and `KRB5CCNAME` every rsmb process of the tests gets) and why a tapped Kerberos session through the proxy holds the service ticket; what is not in the gate and why |
| [`docs/performance.md`](docs/performance.md) | The speed targets with the measured value of each (a Kerberos start-up included); the navigation table of `testenv/bench-rsmb.sh` beside the smbclient-ng baseline, with fresh and paused columns and the wan-bad row; start-up and `shares` costs (with and without the access column); listing memory; per-file transfer costs, small files 16 wide against one after another, the transfer table of `bench-rsmb.sh transfers` beside iperf3, the cost of the sync before a rename, peak RSS; the walk table of `bench-rsmb.sh walk` beside the wave bound, one listing after another and smbclient-ng, what a walk costs per fixture, the price of the queue order, memory of `find` and `tree`; the method; what was not done |
| [`docs/test-environment.md`](docs/test-environment.md) | The test containers (seven): topology and addresses, Samba configuration, how latency is injected, every fixture with its exact bytes and what it targets (`links/` for two paths to one directory), how Samba presents the odd names, `ctl.sh` controls, the lock convention (who takes it shared or exclusive, the test binaries, the proxied step and `bench-rsmb.sh transfers` and `walk` included), that the proxy also reaches `samba` and `wan` and what the tests use that for, what microsocks does (one `recv` per message, no `TCP_NODELAY`), the independent connection count, what `check.sh` proves, fallbacks, the two modes of `./test.sh` and the scratch directories tests write to, the server's BusyBox `find` run as tester; the Kerberos server `samba-dc` (provisioning at build and why it needs `xattr_tdb`, what is in the realm and why, `mkcache`, the KDC's log lines that name the source address, the shortest usable ticket, the aliases `dc.rsmb.test` and `rsmb.test`), and `check.sh` exporting `KRB5_CONFIG=/dev/null` |
| [`docs/measuring.md`](docs/measuring.md) | `smbtap` (library and CLI), the definitions of request, flight, interim and outstanding (and an encrypted answer to plain requests), how per-command counts and timings are taken, why a TGT-only Kerberos login is not tapped through the proxy, the smbclient-ng baseline (method, why each command costs what it does, measured numbers, the baseline file `bench-rsmb.sh` reads), what `tree` and `find` cost in smbclient-ng and how the walk row is taken, the tap in front of a SOCKS5 proxy (`Tap::start_socks5`: where it sits, what it relays uncounted, one write per message, its cross-check), how the smbclient-ng transfer rows are taken, the validation of the tap against tshark |
| [`docs/library-decision.md`](docs/library-decision.md) | Why `smb2` 0.43.0: the decision rule, every gate for both crates with its evidence row, the comparison with smbclient-ng, what rsmb builds on top of `smb2` (the stream and compound calls of Stage 2, `open_close`), the library behaviours later stages must design around (a shrinking download ends silently, a full disk shows at `finish`, the private-use name mapping rsmb refuses; Kerberos: what works, the KDC client with its own sockets, the fixed KDC options, the cache lookup that ignores the end time, the parser's allocation, `load_ccache(None)`), the listing shape chosen in Stage 1 with its candidates, rule and the table measured through the product, how to re-run the spikes, what was not verified |
| [`docs/known-limitations.md`](docs/known-limitations.md) | rsmb's own limits (timeouts, reconnect, interrupts, Tab bound, encryption, DFS, credits, names, output; Kerberos: a cache file only, `-k` changed its meaning, the service name as given, a forwardable and renewable TGT, one KDC, no ticket kept, 10 s, expiry and a session's end, the loopback socket, the owner rule, the cache that aborts the process, Samba's KDC only; transfers: nothing resumed, what is not detected or kept, names refused, case folding, links, leftovers, the two-uploader window; walks: waiting behind a slow listing, `tree`'s limit, memory per directory, which of two paths is listed, links out of the subtree, patterns and `-size` against Unix and the server's `find`, `find` not run again once it printed), what the access column of `shares` does not tell and the tree it leaves on a share that requires encryption, Samba-only testing, what is out of scope, what Samba does with odd names, known limits of `smb2` and of the test environment |
| [`docs/reviews.md`](docs/reviews.md) | The review of each stage: findings, how each was validated, what was done, what was rejected and why, what was checked and found correct |

Documents are added as the work produces them; there are no empty placeholders. The approved plan
lists the documents expected from each stage.

## Principles that have proved good here

Each entry is something that has actually worked in this project, with one line on why and where
the evidence is. Add an entry when a way of working has proved itself; correct or remove one when
it stops holding. Nothing goes in on theory.

- **Inject latency at a router container, never on the host.** A container with `NET_ADMIN`
  forwards with DNAT/SNAT and delays both interfaces with netem: no root, no host change, and the
  delay sits at a forwarding hop, so TCP is not split. Evidence: Stage 0, connect time to the slow
  endpoint 200.1–200.2 ms against 0.03 ms direct (`testenv/check.sh`).
- **Prove the environment with tools that do not share the code under test.** `check.sh` uses
  smbclient, bash `/dev/tcp`, python and `docker exec`, and fails per assumption. Evidence: with
  netem removed from `wan` by hand, the healthcheck went unhealthy within 4 s and `check.sh` failed
  at once.
- **Healthchecks: short interval, `retries: 2`, a `start_period`.** A high retry count hides a
  broken service for a minute. Evidence: with `retries: 30` the router stayed "healthy" for well
  over 5 s after losing its netem qdisc; with `retries: 2` and `start_period: 60s` it turned
  unhealthy within 4 s and start-up did not get slower (all six healthy in 4.6–4.7 s).
- **Cross-check a counter against an independent one, and explain any difference before trusting
  either.** Evidence: smbtap against tshark first differed by 3 responses; the capture had dropped
  segments (`dumpcap` buffer too small), not the tap. With `-B 1024` every run since matched on every row
  (`docs/measuring.md`).
- **Bake fixtures into the image; keep the multi-gigabyte files in their own layer.** Evidence:
  a 5.96 GB image with 100,000-entry directories starts in 4.6–4.7 s, and `up.sh` takes 1 s
  when nothing changed.
- **In bash with `pipefail`, capture output before grepping it** (`out=$(cmd); grep … <<<"$out"`)
  when `cmd` is expected to fail. Evidence: the first `check.sh` run reported 10 false failures
  because smbclient's non-zero exit failed `smbclient … | grep -q`.
- **Fix the decision rule before measuring.** Hard gates first, the comparison only after them.
  Evidence: the library choice did not depend on the rows where the losing crate was faster (fast
  download 1459–1518 against 878–1054 MB/s, 16-wide walk 13.9 against 18.5 s). The rule was not
  followed without argument, though: gate 2 as written ("close to levels × listing round trips",
  4.0 s) was met by neither crate (18.5 s and 13.9 s) and was reinterpreted after measuring as
  "requests overlap, bounded by the width", equally for both; and excluding `smb` was a judgement
  (an unfixed anonymous login in a pinned dependency, an unreleased pin, hangs), because its
  patches were within the allowance. Write gate formulas that can be met by the setup you test with
  (`docs/library-decision.md`).
- **Count flights, not seconds.** Evidence: at the slow endpoint both crates list `huge/` in exactly
  5 flights, yet their times were 3774.5 and 2141.4 ms. The difference came from how the timing was
  taken, not from the libraries: re-measured, `smb2` takes 2337.0 ms on a warm connection (median of
  3) and 3973.6 ms for the first listing on a fresh one (`many/`: 901.1 against 2103.3 ms). The
  plan's (10 − 0) ÷ 10 method was not used for the crates. TCP slow start fits the numbers but was
  not confirmed separately (`spikes/smb2/results-step4-warm-2026-11-18.txt`).
- **Run every measurement step under a timeout, so a hang becomes a recorded result.** Evidence:
  the smb-rs run recorded `TIMEOUT after 120s` for the 1.1 GiB download on the pin and
  `TIMEOUT after 30s` for an over-charged IOCTL, and then went on with the remaining steps.
- **Re-measure alone what was measured on a shared environment.** Evidence: the counts were
  identical, but the slow `many/` listing with smb-rs moved from 879.3 ms (shared) to 1078.8 ms
  (alone), which is more than the 10 % tie margin of the decision rule.
- **Measure the candidate before writing the plan around it.** Evidence: the Stage 1 listing
  shape was measured on the raw API while planning (5 requests in 1 flight on Samba, 16 / 1, 44 /
  4 and 213 / 17 on edge); the product, built from the plan, gave exactly the same counts on all
  36 rows of the confirmation and kept every name (`docs/library-decision.md`, "Listing shape").
- **Install signal handlers before the line editor first takes the terminal.** rustyline saves
  the SIGINT handler it finds and restores it after each line. Evidence: in the planning
  prototype a handler installed after the first prompt let Ctrl-C kill the process; installed
  before, Ctrl-C interrupts the command and the shell goes on (`interrupt_interactive`).
- **Never name a helper script after a standard module.** Evidence: a pty helper called `pty.py`
  shadowed python's own `pty` and failed at `pty.fork`; it is `testenv/ptyrun.py`.
- **Act on an observed event, never on a sleep, when a test must hit a command in flight.**
  Evidence: the cut and interrupt tests wait until the tap has seen the listing's first
  QUERY_DIRECTORY (`Client::wait_requests`), and `cut_during_a_listing` then proves the listing
  was on the wire on both connections; a fixed sleep could pass without the command having
  started (adversary finding 7, `docs/plans/stage-1.md`).
- **Derive a temporary name from the final one, so the next attempt cleans up.** Evidence: after
  `kill -9` in the middle of a `get` and of a `put` exactly one `.rsmb-<hash>.part` was left on
  each side, and the next transfer of the same name removed it (`kill_9_leaves_only_temporaries`).
- **Let the operation that commits refuse, rather than a check before it.** Evidence: a target
  planted during an upload is refused only by the rename itself (`target_appears_during_put`); a
  check before the data cannot see it, and on the small path the refusal costs 6 more requests
  only when it happens (7 when accepted, 13 when refused, `transfer_costs`).
- **Keep what must outlive a dropped future outside the future.** Evidence: the upload's writer
  lives in a slot of the session, so after Ctrl-C the cleanup ends it with `abort()`; on the
  16-credit server 5 of 5 interrupted uploads left nothing, the delete each time on the first
  connection (`signal_mid_put_edge`), where the plan's first design lost the handle in 4 of 11.
- **Check a concurrency bound against what the server granted, not a configured number.**
  Evidence: Samba with `smb2 max credits = 16` let 17 to 19 requests be outstanding while the
  client never spent a credit it had not been granted (`credit_balance` in `edge_and_encrypted`).
- **When a library declares a connection dead, check that every blocked call wakes up.**
  Evidence: after a blackhole an upload failed at 20.4–20.6 s instead of about 15 s, because a
  frame stuck in a full TCP window waited for the crate's 20 s send timeout; with the send timeout
  set to the same bound as silence it fails at 15.0–15.6 s (`silent_link_mid_put`).
- **Take results that run side by side in the order they were asked for, when the outcome must
  not depend on timing.** Evidence: a walk that took its listings as they completed listed one or
  the other of two paths to one directory (`links/`: 7 of 10 runs one way) and printed in a new
  order every run; taken in queue order, 10 of 10 alike, and F50 is identical in 20 of 20. The unit
  test `walk_takes_results_in_queue_order` fails when that one line takes completion order. Cost:
  nothing on `tree/` and `fanout/`, 7 % on the whole share (`docs/plans/stage-3.md` 2.3, 2.10).
- **Compare with the server's own tool, and take expected numbers from it when the test runs.**
  Evidence: the pattern matcher agreed with the server's `find` on 60 patterns before any test was
  written, and `find_filters` checks all 60 against `server_find` on every run; the plan's
  hand-written count for `find /cjk -maxdepth 1 -name '東京　大阪*'` (114) was wrong, and the
  server's `find` said 112 (114 is without `-maxdepth`) (`docs/plans/stage-3.md`, deviations).
- **Run the reference tool as the user the client logs in as.** Evidence: `find tree -mindepth 1`
  in the container gives 683 entries as root and 682 as `tester`, because root sees into the
  directory the SMB user may not list; `server_find` runs `docker exec -u tester`.

- **Forward a protocol message in one write when the peer may parse one read per message.**
  Evidence: a tap that relayed the SOCKS5 CONNECT request in three writes got `General SOCKS
  server failure` from microsocks; relayed whole, all 33 commands counted the same as direct
  (`docs/plans/stage-4.md` 2.3; `socks5_preamble_is_relayed_and_not_counted` fails a split).
- **A test that something goes through X needs a case where going around X would succeed.**
  Evidence: every proxy failure first tried used a server the host cannot reach anyway, so a
  fallback to a direct connection would have passed unseen; with the proxy port closed or the
  proxy gone mid-session and `-H 10.213.45.10` (reachable directly) the tests now see the
  failure itself (`proxy_failures`, `cut_through_the_proxy`).
- **A rerun selected by a variable must prove the variable arrived.** Evidence: `tests/fast`
  passes in full without `RSMB_TEST_VIA` reaching it; the proxied step counts the proxy's log
  lines (149 added through the proxy, 0 direct) and failed with `added 0 lines` when the
  variable was taken out of its line (`docs/testing.md`).
- **Prove a negative with a control that shows the probe can see.** Evidence: "no name lookup on
  the host" is shown by `LD_DEBUG=files` loading no `libnss_` module through the proxy, beside a
  control (direct to the same name) that loads six; without the control a host that loads none
  would pass blindly (`no_way_around_the_proxy`).
- **Point a library that dials by itself at a loopback relay of your own when its traffic must
  take your route.** Evidence: `smb2`'s KDC client opens its own sockets, so through the proxy
  the KDC logged the request from the host's own address, and a silent KDC took 66 s; given
  `127.0.0.1:<port>` of a UDP relay that dials with rsmb's `open_tcp`, the KDC logs exactly one
  request from the proxy's address and none from the host (`kerberos_kdc_is_asked_through_the_proxy`),
  and a paused KDC fails in 10.02 s (`silent_kdc`); no fork of the library, no second Kerberos
  implementation (`docs/plans/stage-5.md` 2.5).
- **Give a test its own principal when a shared log must show who asked.** Evidence: the KDC's
  log is shared by every test that logs in at the DC, side by side; the users `kdirect` and
  `kproxy` belong to `kerberos_kdc_is_asked_through_the_proxy` alone, so its counts can be exact
  (+1 from the proxy, 0 from the host) while the other Kerberos tests run beside it as `tester`.
- **A server may accept what a client should refuse: check a credential's end yourself.**
  Evidence: Samba accepted a service ticket 13 s past its end and then failed every request with
  `STATUS_NETWORK_SESSION_EXPIRED`, and answered one more than five minutes past its end with
  `STATUS_INVALID_PARAMETER`, which says nothing a person can act on; the crate takes an expired
  entry from the cache. rsmb removes expired entries itself and says which ticket expired when
  (`docs/plans/stage-5.md` 2.6, 2.7; `ticket_choice`, `kerberos_cache_errors`).

## Working commands

| Command | What it does |
|---|---|
| `./test.sh` | full gate: environment up, stale test directories removed, `testenv/check.sh`, fmt, clippy, build, unit tests, `tests/fast`, `tests/fast` again through the proxy (`RSMB_TEST_VIA=socks5`; fails unless the proxy logged at least 100 new connections), `tests/slow`, `tests/serial`, smbtap against tshark; prints every step's duration and the counted time against the 600 s budget (about 8 min with the environment up: 477 s counted after Stage 5) |
| `./test.sh fast` | the same up to `tests/fast` and its run through the proxy (everything without injected latency); budget 150 s (about 62 s) |
| `cargo build --release && target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM` | rsmb against the test server (`10.213.45.30` = 100 ms each way); `--help-all` prints the manual |
| `target/release/rsmb --proxy socks5://10.213.45.20:1080 -H fileserver.rsmb.test -u tester -p testpass -d TESTDOM` | rsmb through the SOCKS5 proxy to the private server (port 1081 wants `tester:testpass@`) |
| `(umask 077; docker exec rsmb-samba-dc mkcache -f tester cifs/10.213.45.12 > "$XDG_RUNTIME_DIR/rsmb-k.cc")` | a ticket cache from the DC: user `tester`, a forwardable TGT (`-F`: not forwardable) and a service ticket per name given; nothing of the host's Kerberos is used |
| `KRB5_CONFIG=/dev/null KRB5CCNAME="$XDG_RUNTIME_DIR/rsmb-k.cc" target/release/rsmb -k -H 10.213.45.12` | rsmb with Kerberos at the DC (the service ticket is in the cache, so no KDC is asked); through the proxy `--proxy socks5://10.213.45.20:1080 -H dc.rsmb.test` with a cache for `cifs/dc.rsmb.test` |
| `RSMB_TEST_VIA=socks5 flock -s /tmp/rsmb-env.lock cargo test --release --test fast <name>` | one test of `tests/fast.rs` through the proxy |
| `./testenv/bench-rsmb.sh [quick]` | rsmb beside the smbclient-ng baseline: time, connections, requests, flights per `cd` and `ls`, fresh and paused columns, a wan-bad row (about 10 min; `quick` about 1.5 min), see `docs/performance.md` |
| `./testenv/bench-rsmb.sh transfers` | the transfer table (exclusive lock): `big.bin` ×3 and `over4g.bin` each way at the slow endpoint beside iperf3, `smallfiles/` both ways beside the wave bound, `get -r /tree`, wan-bad beside iperf3, the sync cost at the fast endpoint, `medium.bin` (50 MiB) each way for rsmb and smbclient-ng by process time; checksums on both sides; removes what it wrote (about 27 min), see `docs/performance.md` |
| `./testenv/bench-rsmb.sh walk` | the walk table (shared lock): `tree /tree/d0` beside smbclient-ng at the slow endpoint (time and counts), `find /tree`, `/fanout`, `/chain`, `/huge`, `tree /huge`, the whole share, wan-bad; the product's own seconds beside the wave bound (about 6 min), see `docs/performance.md` |
| `./testenv/up.sh` | build images where needed and start the environment (seven containers); returns when all is healthy (about 150 s from nothing, the DC's image 84.7 s of it; seconds with the images built) |
| `./testenv/check.sh` | verify the environment with independent tools (about 30 s) |
| `./testenv/down.sh` | remove containers and networks (images stay) |
| `./testenv/ctl.sh blackhole on\|off`, `evict`, `conns`, `conns-reset`, `drops`, `pause`, `unpause` | controls for tests and spikes, see `docs/test-environment.md` |
| `./testenv/validate-smbtap.sh [ip]` | one smbclient session through smbtap, compared with a tshark capture |
| `./testenv/bench-smbclientng.sh [quick]` | smbclient-ng baseline: time, connections, requests, flights per `cd` and `ls` at both endpoints (about 18 min; `quick` about 4 min), see `docs/measuring.md` |
| `spikes/smb2/run.sh [step…]` | the `smb2` spike: all 13 steps about 15 min, prints the results table, see `docs/library-decision.md` |
| `spikes/smb-rs/run.sh [step…]` | the `smb` (smb-rs) spike on the git pin and the patched build, about 20 min; clones upstream on first use |
| `cargo run -q -p smbtap -- 10.213.45.30:445` | the tap as a CLI, see `docs/measuring.md` |
| `smbclient -s /dev/null //10.213.45.10/share -U 'TESTDOM\tester%testpass'` | poke at the server by hand (`10.213.45.30` = 100 ms each way) |

Commit and push once per stage, by the orchestrating session only (rule 6). Keep the code plain
(rule 5); treat the remote as plain git.
