# Testing

Every test runs locally against the containers of [`test-environment.md`](test-environment.md);
there is no CI service. `./test.sh` is the one command. The plans behind the rows below are
[`plans/stage-1.md`](plans/stage-1.md), [`plans/stage-2.md`](plans/stage-2.md),
[`plans/stage-3.md`](plans/stage-3.md) and [`plans/stage-4.md`](plans/stage-4.md), section 6 of
each, and [`plans/stage-5.md`](plans/stage-5.md), section 7; the numbered rows (F1, S3, X5 …, F21,
S21, X21 …, F41, S41, X41 …, F61, S61, X61 … and F71, S71, X71 …) refer to them.

## `./test.sh`

```
./test.sh fast    environment up · stale test directories · check.sh · fmt · clippy · build ·
                  unit tests · tests/fast · tests/fast through the proxy
./test.sh         the same, then tests/slow · tests/serial · smbtap against tshark
```

| Step | Command | Lock |
|---|---|---|
| test environment up | `./testenv/up.sh` | its own |
| stale test directories | `docker exec <c> sh -c 'rm -rf /srv/share/upload/rsmb-test-*'` on the three Samba containers and the DC | exclusive |
| test environment check | `./testenv/check.sh` | its own, exclusive |
| cargo fmt --check | `cargo fmt --all --check` | none |
| cargo clippy | `cargo clippy --workspace --all-targets -- -D warnings` | none |
| build | `cargo test --workspace --lib --bins --no-run`, then `cargo test --release --workspace --no-run` | none |
| unit tests | `cargo test --workspace --lib --bins` (dev profile: clap's `debug_assert` and overflow checks are active) | none |
| tests/fast | `flock -s /tmp/rsmb-env.lock cargo test --release --test fast` | shared |
| tests/fast through the proxy | the same with `RSMB_TEST_VIA=socks5`, between two counts of the proxy's log lines `connected to fileserver.rsmb.test:445` (`docker logs rsmb-socks`); fails unless the run added at least 100 | shared |
| tests/slow (full) | `flock -s /tmp/rsmb-env.lock cargo test --release --test slow` | shared |
| tests/serial (full) | `flock -x /tmp/rsmb-env.lock cargo test --release --test serial -- --test-threads=1` | exclusive |
| smbtap against tshark (full) | `./testenv/validate-smbtap.sh` | none |

Every lock is taken with a wait limit of 30 minutes (`locked` in `test.sh`, `flock -w`); when it
expires the step fails with a message that names `/tmp/rsmb-env.lock` and the mode.

Each step prints how long it took; at the end come all step durations, the total and the
**counted time** = total − "test environment up" − "build", against the budget: **150 s for
`fast`, 600 s for the full run** (master plan section 6, after Stage 4). Over budget prints
`OVER BUDGET`; the exit code does not change, but it is a finding to fix before the stage closes.

### The second run of `tests/fast`, through the proxy

`tests/fast.rs` names its endpoint only through `common::fast()`: `Ep::Fast` (direct to
`samba`), or `Ep::Proxied` when `RSMB_TEST_VIA=socks5` (any other value panics). `./test.sh`
runs the binary twice, in both modes: direct, then with the variable, when every test of the
file, the pty tests, the counts and the checksums included, runs through the proxy's port 1080
against the private server `fileserver.rsmb.test`, which this host cannot reach. Nothing is
skipped in the proxied run; the tests that need no server simply run twice.

**The check that the run went through the proxy.** If the variable did not reach the test binary,
`fast()` would give the direct endpoint and the step would be green without the proxy. So
`./test.sh` counts the proxy's log lines for the private server (`connected to
fileserver.rsmb.test:445`, one per connection the proxy made; `docker logs` captured first, then
`grep -c`) before and after the step, and fails below **100 new lines**. A proxied run adds 149 before the Stage 4 tests, 160 with
them and 165 after the review (measured 2026-11-18: one per TCP connection the binary opens; a direct run opens the same 149 on
`samba` and adds 0, apart from the five accepted URLs of `proxy_urls_at_startup`, which name the
private server on purpose). Taking the variable out of the step's line made it fail with
`added 0 lines`. Other holders of the shared lock can only add lines; the one case the check
cannot see is a second gate in this same step at the same moment (about 150 lines more), and the
project runs one gate at a time (`AGENTS.md`, one agent per task).

### Step times

The final gate of Stage 5 (2026-11-18), environment up (seven containers), binaries built:
**full 477 s counted of 600** (total 480 s); `./test.sh fast` alone **62 s of 150** (total 63 s).

| Step | Full | Fast |
|---|---|---|
| test environment up | 1 s (not counted) | 1 s (not counted) |
| stale test directories | 0 s | 0 s |
| test environment check | 31 s | 31 s |
| cargo fmt --check, cargo clippy | 0 s | 1 s |
| build | 2 s (not counted) | 0 s |
| unit tests | 2 s (95 in rsmb, 12 in smbtap) | 2 s |
| tests/fast | 13 s (78 tests) | 13 s |
| tests/fast through the proxy | 15 s (78 tests, 165 proxy log lines) | 15 s |
| tests/slow | 28 s (46 tests) | — |
| tests/serial | 385 s (27 tests) | — |
| smbtap against tshark | 3 s | — |

Earlier figures: full 304 s of 420 after Stage 2, 386 s of 540 after Stage 3, 445 s after Stage 4
(the plan expected about 458 s and 64 s), 456 s after the final review of Stage 4 (fast 57 s);
the Stage 5 plan expected about 482 s and 66 s.

The integration tests run in the release profile (the wall-time and memory rows measure the
binary that ships); the unit tests in the dev profile.

## What goes where

| Binary | What | Runs |
|---|---|---|
| unit tests (`src/*.rs`) | pure logic: tokeniser, quoting, paths, error mapping, listing plan, parser, cache, time format, the table and the manual, the receive half on a local socket, the walk on fake trees, the pattern matcher, sizes, the tree drawing, the end report, the ticket cache's rules, `krb5.conf`, the KDC relay against a fake KDC and fake proxies on local sockets | always |
| `tests/fast.rs` | fast endpoint (10.213.45.10) and, for Kerberos, the DC (10.213.45.12), safe beside other tests; run a second time through the proxy (`RSMB_TEST_VIA=socks5`) | `fast` and full, twice each |
| `tests/slow.rs` | slow endpoint (100 ms each way), edge, wan-bad, the proxy's failures, its login port, its cuts and signals; what proves the KDC's side of a Kerberos login (its log, its failures); each test has its own tap, so they run in parallel | full |
| `tests/serial.rs` | anything that pauses a server (the DC included), uses the blackhole, resets or reads the server-side connection count, counts the proxy's log exactly, or asserts a wall time or a number of new connections that depends on the 2 s of a cleanup | full, one at a time |

`tests/slow.rs` runs its tests side by side on one `wan` link, several of them streaming 50 MiB;
that is why a test that asserts a time belongs in `tests/serial.rs`. The only times `tests/slow.rs`
asserts are **no-hang bounds**: a command that should end promptly ends within `NO_HANG` (10 s,
at least five times what it takes alone; the edge `ls many` bound is 30 s), together with an exit
code or an error kind that already proves the behaviour (an interrupt gives 130 and `interrupted`,
which only an interrupt can). Interrupt latency as a number (4 s after one signal, 6 s in a
recursive transfer) is asserted in `tests/serial.rs` (`signals_mid_put`, `signal_in_the_small_file_phase`,
`signals_mid_get`). `tiny` is used by one test
only (`disk_full_tiny`), whose guard empties `/srv/tiny`.

`tests/serial.rs` also takes one process-wide mutex in every test (`common::serial()`), so it is
serial even without `--test-threads=1`. A test that pauses a server or turns the blackhole on
holds a `Guard` whose `Drop` undoes it, also when the test fails.

## The harness, `tests/common/mod.rs`

- `run(args, stdin)`: one rsmb process, at most 60 s; exit code, stdout, stderr, duration.
- `Client`: one rsmb process with `--json --keep-going`, fed line by line. `Client::tapped(ep)`
  puts a new `smbtap::Tap` in front of the endpoint; `tapped_at(addr, args)` does the same in front of any address (the blackhole endpoint); `direct(addr)` does not; `with(..)` gives
  full control. Every constructor sends `pwd` first and keeps its `Reply` in `startup`, so start-up
  is counted on its own. `cmd(line)` writes the line, reads to the `done` line (60 s limit),
  then `tap.settle(300 ms, 60 s)`, reads the tap's stats and resets it: a request sent late is
  charged to the command that caused it. `send` + `wait_requests("QUERY_DIRECTORY", 1, …)` lets a
  test act (cut, signal) exactly while a command is on the wire, without a fixed sleep. A command that ends on a blackholed link
  is read with `read_done` and the tap reset afterwards: `settle` waits for answers that never come.
- `Reply`: the objects before `done`, `ok`, the tap's `Stats`, the time. `expect(what, requests,
  flights)` panics with the expectation, the per-command counts and the request log.
- `Pty`: rsmb on a pseudo-terminal through `testenv/ptyrun.py` (200 × 50, `TERM=xterm`,
  `RSMB_HISTORY` in the test's own directory). `expect(needle, timeout)` returns the output up to
  the needle or panics with the escaped transcript. The end of a completion is seen in the output
  (the inserted text, or the BEL for several candidates); no test waits a fixed time for one. A
  second Tab in a row only lists what the first found, so "a second completion" in a test is a
  new line: Ctrl-U, new text, Tab (`common::tab`).
- `testenv/ptyrun.py`: `pty.fork()`, sets the window size, relays stdin → pty → stdout until the
  pty closes, exits with the child's code (128 + signal if killed). Not called `pty.py`: that
  would shadow python's own module.
- `tmp(test)`: an empty directory under `CARGO_TARGET_TMPDIR`, wiped at every call, so a history
  file from an earlier run cannot answer an Up key. The route is part of the path
  (`…/direct/<test>` or `…/socks5/<test>`), so a direct and a proxied run of the same test at the
  same time keep apart.
- **Stage 4: endpoints and routes.** `Ep` has `Fast`, `Slow`, `Edge`, `Bad`, `Proxied` (the
  private server through the proxy's port 1080) and `ProxiedLogin` (port 1081, `tester:testpass`
  at the proxy). `ep.addr()` is what a tap in front of it forwards to (the proxy's listener for the
  proxied two); `ep.container()` the container whose disk it serves (`rsmb-samba-private` for the
  proxied two); `ep.tap()` a new `Tap::start`, or `Tap::start_socks5` for the proxied two, which
  relays the SOCKS5 handshake and counts only what follows it (`docs/measuring.md`);
  `ep.route(tap)` the flags that reach it (`-H <host> -P <port>` of the server or the tap, or
  `--proxy socks5://[tester:testpass@]<proxy or tap> -H fileserver.rsmb.test -P 445`);
  `ep.args(tap)` the route and the login; `ep.host(tapped)` the host as the prompt and `pwd` show
  it. `fast()` is the endpoint of `tests/fast.rs` (above); a test there names `fast()` and its
  methods, never an address or a container. `Client::tapped(ep)`, `Pty::tapped`, `piped`,
  `Scratch::new` and the `check_*` bodies use them; `Client::to(ep, extra)` is the session without
  a tap; `Client::through(tap, args)` a session whose tap the test made itself, with the whole
  command line in `args` (dropping the tap from `take_tap()` closes its port); `Pty::finish`
  waits for the exit and returns the whole transcript; `proxy_lines(needle)` counts the proxy's
  log lines that hold `needle`. **A test that passes its own `--proxy` builds its whole command
  line and never adds to `route()`**: in the proxied run the route already holds `--proxy`, clap
  refuses a second one with exit 2, and a test expecting exit 2 for a bad URL would pass for that
  reason (`proxy_urls_at_startup` and `startup_failures` assert the parser's own `expected
  socks5://`). A failure in the proxied run names its route: `Client::what` and the pty
  transcript hold the arguments.
- **Stage 2.** `Scratch::new(ep)`: a directory of its own under `upload/` on the endpoint's
  server (`rsmb-test-<pid>-<n>`, owned by tester), removed on drop, also when the test fails;
  `remote()` is its path in the share, `server()` in the container, `sh(script)` runs a shell there
  as tester. Every test that writes to a server does it below a `Scratch`, and every test starts
  its session with `lcd <its own tmp directory>` (`ready(client, dir, cd)`); `./test.sh` still
  removes `upload/rsmb-test-*` at its start. `server_sha` / `local_sha`; `server_tree` /
  `local_tree` (the same pipeline on both sides, `find -type f -print0 | sort -z | xargs -0
  sha256sum` and the directories, compared as byte strings); `temps_on_server` / `temps_local`
  (every `.rsmb-*.part`); `run_in(dir, …)` (rsmb with that working directory); `run_sh` (bash, for
  a child with a file size limit); `Client::kill(signal)`, `Client::take_tap()`; `wait_file`,
  `wait_server_file` (a test that acts in the middle of a transfer waits for an observed event:
  the tap's request count, the local temporary file's size, the remote one's); `wait_no_open`
  (`smbstatus -L` in the container: a test that reuses a remote temporary file after a kill or a
  silent link waits for the server to let go of it); `fnv_temp` (the temporary name, computed by
  the test's own FNV-1a); `make_file`; `credit_balance` (over a whole connection's logs: the client
  never spent a credit it was not granted); `Remove` (a guard for the multi-gigabyte local files).
- **Stage 3.** `server_find(container, dir, args)`: the server's own `find` below `/srv/share`,
  run **as tester** (`docker exec -u tester -w /srv/share … find <dir> -mindepth 1 <args…>
  -print0`, an argument vector, never through a shell, so a pattern needs no quoting), so it sees
  what the SMB user sees; paths relative to the share root as byte strings; stderr and the exit
  status ignored (a directory it may not read). The containers' `find` is BusyBox on musl: it has
  `-name`, `-iname`, `-type`, `-maxdepth`, `-mindepth`, `-print0` and `-size N[bck]`, no `M` or
  `G`, so a test compares sizes as `-type f -size <bytes>c`, computed from the unit by the test.
  The 60 patterns of the plan's 2.7 are the ones known to agree with rsmb's matcher; a new pattern
  in a test is first checked by hand against it. `Reply::paths()` (the `path` of every `entry`,
  without the leading `/`), `same_paths(what, got, want)` (panics with the first differences),
  `skipped`, `denied`, `walk_summary`. `piped(ep, line, filter)`: a tap in front of `ep`, then
  `bash -c '<rsmb> -C "use share" -C <line> | <filter>; exit ${PIPESTATUS[0]}'`: rsmb's exit
  code, the filter's output, the time, the tap's stats. No test in `tests/fast.rs` or
  `tests/slow.rs` walks into `upload/` (other tests create and remove directories there); the
  walk from the share root (X44) is in `tests/serial.rs`. A test that acts in the middle of a walk
  waits for `wait_requests("QUERY_DIRECTORY", n, …)`.
- `server_names(container, dir)`: `docker exec … ls -A`, the server's names as byte strings.
  `fixture_names(var)`: the names of `testenv/samba/names.sh` (it fails when sourcing the file fails). `assert_named(what, temps, error)`: the temporaries found on a server are the ones the error's `left_behind` names. `ctl(..)`: `testenv/ctl.sh`.
  `connect_ms(addr)`: median of 5 TCP connects after a warm-up. `q(name)`: the quoting, written
  again on purpose.

- **Stage 5: Kerberos.** `Ep::Dc` (the DC by address, direct) and `Ep::DcProxied` (the DC as
  `dc.rsmb.test` through the proxy's port 1080); `dc()` is the Kerberos endpoint of
  `tests/fast.rs`, `Ep::Dc` or `Ep::DcProxied` with `RSMB_TEST_VIA=socks5`, and a test there names
  `dc()`, never an address. For both, `container()` is `rsmb-samba-dc`, `args(tap)` is the route
  and `-k` (no user, no password), `host(tapped)` is `127.0.0.1` tapped, `10.213.45.12` not,
  `dc.rsmb.test` proxied, and `spn(tapped)` is the service name a session asks for:
  `cifs/127.0.0.1` (direct, tapped), `cifs/10.213.45.12` (direct), `cifs/dc.rsmb.test` (proxied).
  `krb_cache(dir, name, flag, user, spns)` makes a cache in the DC (`docker exec rsmb-samba-dc
  mkcache -f|-F <user> <spns…>`, 0.1 s) and writes it to `dir/name` with mode 0600;
  `expired_copy(cache)` writes a copy in which every ticket ended an hour ago (each run of four
  big-endian 32-bit times near now gets `now - 3600` as its end); `krb5_conf(dir, kdc)` writes a
  `krb5.conf` that names `kdc` for `RSMB.TEST`; `kdc_requests(user, addr)` counts the DC's log
  lines `TGS-REQ <user>@RSMB.TEST from ipv4:<addr>:`; `Client::kerberos(ep, cache, extra, env)` is
  a tapped `-k` session with `KRB5CCNAME=<cache>`; `Client::build` and `Pty::command` take an
  `env` slice. **Every rsmb process the harness starts gets `KRB5_CONFIG=/dev/null` and
  `KRB5CCNAME=/nonexistent/rsmb-test/krb5cc` (`NO_CACHE`)** unless the test sets them (`run_env`,
  `run_in`, `run_sh`, `Client::build`, `Pty::command`): no test reads the host's `/etc/krb5.conf`,
  the developer's `/tmp/krb5cc_<uid>` or a `KCM:` the shell exports. **A direct TGT-only run names
  the KDC with `krb5_conf(dir, "10.213.45.12")`**: without it rsmb would ask `rsmb.test:88`,
  which the host cannot resolve. A proxied run may use `/dev/null` (the proxy resolves
  `rsmb.test`). **A tapped Kerberos session through the proxy holds the service ticket** for
  `spn(true)`: with a TGT only, the KDC's connection would pass the tap (in SOCKS5 mode) as a
  second connection of frames it cannot read; direct, the KDC's connection goes beside the tap,
  so a tapped direct session may be TGT-only. `Scratch::new` on the DC makes the directory with
  mode 1777 (a file written over SMB belongs to the id Samba maps the realm's user to, not to a
  Unix user); `Scratch::sh` is not used there.

Nothing waits without a limit; a timeout kills the child and panics with what was read.

## Counts

Per-command counts come from the tap (`docs/measuring.md`): requests are SMB2 headers (each
element of a compound counts), a flight is a send after an answer. On the `encrypted` share every
request is one opaque frame; only `encrypted_share` selects that share, because after it the
whole connection is encrypted. `counts_add_up` checks that per-command counts add up to the
session total.

What the tests assert, per command, on Samba (fast and slow endpoint alike):

| Command | Requests / flights | Test |
|---|---|---|
| start-up | 3 / 3 (NEGOTIATE, 2 × SESSION_SETUP), 1 connection | `startup_is_three_requests`, `startup_and_use_slow`, `interactive_basics` |
| start-up with `-k` (Kerberos), service ticket in the cache or (direct) a TGT only | 2 / 2 (NEGOTIATE, 1 × SESSION_SETUP), 1 connection | `kerberos_login` |
| after a cut with `-k`: `ls /` | 1 new connection, 8 requests (NEGOTIATE, SESSION_SETUP, TREE_CONNECT, the 5 of a listing); a cache that cannot be used: 0 connections | `kerberos_reconnect_reads_the_cache_again` |
| `use share` first / again | 1 / 1 (TREE_CONNECT) / 0 | `use_once_per_share`, `startup_and_use_slow` |
| `cd` to a sibling or below | 2 / 1, CREATE (chain 0 of 2) + CLOSE (1 of 2) | `cd_is_one_compound`, `cd_slow`, `cd_answers`, `timing_cd` (20 / 10 for 10) |
| `cd` to the root or above, `pwd`, `help`, `refresh` | 0 | `free_commands_send_nothing`, `reconnect_after_idle_cut` |
| `ls` (any fixture up to `huge/`) | 5 / 1: CREATE 1, QUERY_DIRECTORY 3, CLOSE 1 | `ls_equals_the_server`, `ls_small_slow`, `ls_many_slow`, `ls_huge_slow`, `timing_ls_small` (50 / 10 for 10) |
| `ls` on edge | `sizes/` 16 / 1; `many/` ≤ 44 / ≤ 4 | `edge_navigation` |
| `shares` first / again | 19 / 8, then 18 / 7: the list (7 / 7, then 6 / 6) and one compound of 6 TREE_CONNECT + TREE_DISCONNECT pairs; 7 then 6 TREE_CONNECT, 6 TREE_DISCONNECT each time; a later `use` still costs 1 | `shares_fast` |
| `shares` on edge (306 shares, 16 credits) | 307 TREE_CONNECT, 306 TREE_DISCONNECT, 1 connection (no wait over 10 s); anonymous: never a credit not granted, no encrypted answer | `shares_edge` |
| first Tab in an uncached directory | one listing: 5 / 1 | `completion_costs`, `completion_costs_slow` |
| second Tab, Tab after `ls`, Tab on a command name | 0 | the same, `edge_completion` |
| Tab in a directory that cannot be listed or does not exist: first / second and third / after `refresh` | one listing attempt (1 flight) / 0 / one attempt; `ls` there still asks the server | `failed_completion_is_remembered` |
| first Tab after `use` | 7 (the share list) | `completion_costs` |
| idle 12 s at the prompt | 0 | `idle_sends_nothing` |
| after a cut while idle: `cd d0` + `ls` | 1 new connection, 11 requests | `reconnect_after_idle_cut` |
| `get` of one file up to 512 KiB | 7 / 3: CREATE 1, QUERY_INFO 4, READ 1, CLOSE 1 | `transfer_costs` |
| `get` of 1 MiB + 1 byte | 9: READ 3 | `transfer_costs` |
| `put` of at most 256 KiB | 7 / 3: CREATE 2, WRITE 1, FLUSH 1, CLOSE 2, SET_INFO 1 | `transfer_costs`, `threshold_boundary` |
| `put` of 1 MiB + 1 byte, with and without `-f` | 14: CREATE 3, QUERY_INFO 3, WRITE 3, FLUSH 1, SET_INFO 1, CLOSE 3 | `transfer_costs` |
| `put` of 256 KiB + 1 byte | the stream path: CREATE 3, QUERY_INFO 3 | `threshold_boundary` |
| a small `put` refused because the target exists | 13 / 6 (7, the crate's CLOSE after the refused rename 1, the delete 3, the `stat` 2) | `transfer_costs` |
| a large `put -f` onto a directory | 2 / 1, no WRITE | `transfer_costs` |
| a `get` refused because the local file exists; `lcd`, `lpwd`, `lls` | 0 | `transfer_costs`, `local_commands` |
| `cat` of a small file | 7 / 3 | `transfer_costs` |
| `mkdir d` / `mkdir -p a/b/c` | 2 / 1 / 6 / 3 | `transfer_costs`, `mkdir_rules` |
| `rm`, `rmdir` | 3 / 1 | `transfer_costs` |
| `get -r /loops` | one listing and one small file: CREATE 2, QUERY_DIRECTORY 2, READ 1, CLOSE 2 | `link_loops` |
| `get -r /smallfiles` (21 listings, 2,000 files) / `put -r` of it | 6,084 (peak outstanding ≤ 64) / 14,042 (≤ 48) | `two_thousand_small_files` |
| `get -r /smallfiles/d01` / `put -r d01` at the slow endpoint | 304 / 702, peak outstanding 6 to 48 | `small_files_counts_slow` |
| the Tab after a command that changed the directory | one listing (5 / 1), then 0 | `file_ops_and_cache` |
| one walk listing (`tree`, `find`) | 4 / 1 (CREATE, QUERY_DIRECTORY of 1 MiB, the 64 KiB probe, CLOSE in one compound), 16 at a time | the rows below |
| `find /fanout` (2,001 listings) | 8,004, peak outstanding 5 to 64, 0 new connections; on edge 8,011 (the 2,000-entry root needs the listing's second phase) | `walk_counts_fast`, `walk_edge` |
| `find /tree`, `tree /tree` (342 listings, one denied) | 1,368, peak outstanding 8 to 64 (64 measured) at the slow endpoint; the same on edge | `walk_counts_slow`, `walk_edge` |
| `find /chain` (60 levels) | 244 / 61: nothing can overlap | `walk_counts_fast` |
| `find /deep` (12 levels) | 52 / 13 at the slow endpoint | `walk_deep_slow` |
| `find /many -name 'f0999?'` | 9 (CREATE 2, QUERY_DIRECTORY 5, CLOSE 2: `many/` needs the second phase in a 1 MiB walk); a filter costs nothing | `walk_counts_fast` |
| `find /huge`, `tree /huge` | 19 (15 flights) | `walk_counts_fast` |
| `find /loops`, `tree /loops` | 4 / 1 | `walk_link_loops` |
| `find /fanout \| head -n 5` at the slow endpoint | at most 200 after the first listing (measured 0) | `find_and_a_closed_pipe` |
| `find` or `tree` with a bad flag value | 0 | `walk_depth`, `find_filters` |
| Tab below a directory a walk listed / at a depth it only drew | 0 / one listing (5 / 1) | `walk_feeds_the_cache` |

## Every test

Unit tests:

| Test | File | Protects |
|---|---|---|
| `tokenize_rules`, `ideographic_space_stays`, `fixture_names_round_trip`, `partial_survives_any_cut` | shell | the tokeniser and quoting rules, on every fixture name (row 1.5) |
| `candidates_from_entries` | shell | completion candidates: directories only for `cd`, exact-byte prefix, NFC ≠ NFD, quoted replacements |
| `resolve_paths`, `wire_paths` | session | path resolution; the DFS prefix |
| `classify_statuses`, `guest_refusal_names_the_flag`, `exit_codes` | session | error kinds, messages, exit codes |
| `listing_plan` | session | (b, n) for Samba and edge; the compound never asks for more credits than there are, and fits a frame |
| `query_outcomes` | session | `NO_MORE_FILES`, `NO_SUCH_FILE` on the first query, errors |
| `entries_match_the_crates_packer`, `unpaired_surrogate_is_replaced`, `truncated_entry_is_an_error` | session | our class-37 parser |
| `share_list_overflow` | session | the 65,511-byte limit and its message |
| `overlapping_entries_are_malformed`, `empty_success_is_malformed`, `share_fragments_are_bounded` | session | a hostile server: an entry whose `next` is shorter than itself, a SUCCESS answer with no entry, a share-list fragment that adds nothing or passes the limit while collecting, are malformed (final review, finding 6) |
| `safe_escapes_controls_and_bidi` | session | `safe`: C0, DEL, C1 and the bidi controls become `\u{..}`, nothing else changes, a clean name is not copied |
| `local_temp_is_never_adopted` | transfer | a planted `.rsmb-….part` with mode 666 is replaced by a new inode with the default mode; a live one (lock held) is refused and not unlinked |
| `share_access_names` | session | `MaximalAccess` to `rw`, `r`, `w`, `none`: Samba's masks for `share` and `readonly` (DELETE and WRITE_DAC are not write), GENERIC_ALL, GENERIC_READ |
| `encrypted_answer_to_a_plain_frame` | smbtap | an encrypted answer while no encrypted request waits answers the oldest plain request frame still waiting (Samba's answer to the `shares` compound that reaches `encrypted`), so the tap settles |
| `proxy_urls` | session | `--proxy` parsing, percent-encoded credentials, bracketed IPv6; Stage 4: `socks5h`, one trailing `/`, 1 to 255 bytes per credential, every other scheme refused, and one message for every refused form that holds no part of the input (finding 5) |
| `cache_cap`, `cache_ignores_case` | session | the cache cap (row 1.16, in units since Stage 3) and keys |
| `receive_reports_progress` | session | the receive half reports progress inside a frame (a slow reply is not silence) |
| `abandon_marks_dead_only_in_flight` | session | an interrupt replaces the connection only if requests were in flight |
| `filetime_cases`, `ls_line_format` | commands | UTC time formatting, before 1970 and at `u64::MAX`; the `ls` line |
| `table_is_complete`, `help_all_has_everything`, `arguments_and_flags` | commands | the table (rule 2 of AGENTS.md), the manual's parts, flags against positionals |
| `args_are_sound` | main | clap's own checks, `-p` against `--no-pass`, `--help-all` without `-H`, every long flag in the manual |
| `manual_fits_80_columns`, `args_are_sound` | commands, main | every line of the text manual within 80 columns (a single word longer than a line excepted), `--json` not wrapped, wrapping changes nothing but white space and a flag's help continues under its column; every line of `--help` within 80 columns, the short help of `-k` without the details |

`tests/fast.rs`:

| Test | Row | Protects |
|---|---|---|
| `help_all_needs_no_server` | H1, 1.12 | the manual without a server, text and JSON |
| `docs_match_the_table` | H2, 1.12 | `docs/commands.md` has every command, syntax, example and flag |
| `examples_run` | H3, 1.12 | every example in the table runs on the test server |
| `help_in_a_session` | H4, 1.12 | help sends nothing; `help all` equals `--help-all --json` |
| `closed_stdout_is_not_a_panic` | N6 | no panic on a closed pipe (closed before rsmb starts, so every write fails whatever its size) |
| `closed_stdout_ends_a_keep_going_run` | N7 | with `--keep-going`, a closed stdout ends the run (exit 1, one message, one listing sent) |
| `startup_usage_errors` | final review 3, 13 | an argument that is not UTF-8 with `--json` is a `usage` error, exit 2 (it was a panic, 101); a missing `-H` with `--json` names `--host` |
| `rmdir_guard_folds_case` | final review 4 | `rmdir` refuses the working directory and the ones above it however the case is spelled; the directory is still there |
| `hostile_names_are_escaped_in_text` | final review 2 | a name with U+009B, one with U+202E and a directory with U+009B: `ls`, `find`, `tree`, `get` and an error line print them as `\u{..}` and no control or bidi character reaches stdout or stderr; `--json` has the exact name; `get` creates the exact local name |
| `flag_errors_in_json` | N8 | `--json` with a flag error: one `error` of kind `usage` on stdout, exit 2, empty stderr; text without `--json`; `--version` unchanged |
| `startup_is_three_requests` | F1 | start-up = 3 requests, 1 connection |
| `logins` | F10, 1.1 | user/domain, `DOMAIN\user`, guest, anonymous, wrong password, unknown user |
| `no_password_no_prompt` | N1, 1.11 | exit 2 without prompting when stdin is not a terminal |
| `ways_to_give_commands` | N2, 1.11 | `-C`, `-S`, stdin give the same output; `-S` before `-C`; an open stdin pipe is not read with `-C` |
| `startup_failures` | N5, 1.11 | connect refused, bad proxy URL, missing script |
| `stop_at_first_failure` | N3, 1.11 | exit 1 at the first failure, `--keep-going`, unknown command, `-N` alone |
| `every_json_line_has_a_type` | N4, 1.11 | every JSON line has a listed type; one `done` per command |
| `use_once_per_share` | F2 | one TREE_CONNECT per share |
| `free_commands_send_nothing` | F3 | the zero rows |
| `cd_is_one_compound` | F4 | `cd` = CREATE + CLOSE in one flight, also into CJK and spaced names |
| `cd_answers` | F5, 1.2 | not found, not a directory, access denied, enterable 0711; `pwd` unchanged |
| `ls_equals_the_server` | F6, 1.3 | names equal the server's as bytes, sizes, directory marks, `mangled/`, 5 / 1 each |
| `ls_text` | F7, 1.3 | the text format, sorted, `nested/` |
| `ls_denied` | F8, 1.4 | access denied; the session survives; exit 1 |
| `file_ids_follow_links` | F9 | file ids of symlinks (for Stage 2's walk) |
| `encrypted_share` | F12, 1.8 | `use`, `cd`, `ls` on the encrypted share, opaque frames only |
| `reconnect_after_idle_cut` | F13, 1.9 | no network for free commands on a dead connection; one reconnect, 11 requests |
| `shares_fast` | F11, 1.6 | the share list, hidden shares, the access of each share as `tester` and as anonymous (`none` on `share`), the text column, 19 / 8 then 18 / 7 requests / flights, the probes stay out of the tree map; also through the proxy |
| `interactive_basics` | I1, 1.13 | prompt without requests, history across processes, Ctrl-C, `exit`, Ctrl-D |
| `password_prompt` | I2, 1.1 | the prompt, no echo |
| `names_through_the_shell` | I3, 1.5 | Tab and `cd` for every CJK name, NFC against NFD, `dir\ with\ space/` |
| `completion_costs` | I4 | Tab costs: command names 0, first listing 5 / 1, second 0, after `refresh` 5, after `ls` 0, share list 7 then 0 |
| `failed_completion_is_remembered` | I6 | Tab in `traverse-only/` and in a missing path: one attempt, then 0 until `refresh`; `ls` still asks |
| `sigterm_in_the_shell` | I7 | SIGTERM at the prompt, empty and with the text of a completed Tab in the line (the answer comes too fast here to hit a Tab on the wire; that is `sigterm_in_a_tab_on_the_wire`): exit 130, terminal back to `icanon` and `echo` (`stty -a` after rsmb), history has the earlier lines |
| `idle_sends_nothing` | I5 | 0 requests in 12 s idle |
| `counts_add_up` | F14 | per-command counts add up to the session total |

`tests/slow.rs`:

| Test | Row | Protects |
|---|---|---|
| `startup_and_use_slow`, `cd_slow` | S1, S2 | F1, F2, F4 at 200 ms round trip |
| `ls_small_slow`, `ls_many_slow`, `ls_huge_slow` | S3–S5 | 1 flight per `ls`; 100,000 names; peak RSS ≤ 150 MB |
| `edge_navigation` | S7, 1.7 | 64 KiB buffers and 16 credits: `cd`, `ls many/` ≤ 4 flights, `ls sizes/` 16 / 1 |
| `edge_completion` | S7, S8 (pty parts), 1.6, 1.7 | Tab in `many/` on edge; `use edge300` Tab from the kept share list, 0 requests |
| `shares_edge` | S8, 1.6 | 306 shares, 300 `edge*`, 31 hidden; the access of all 306, one probe per share, one connection; on an anonymous session never a credit not granted |
| `completion_costs_slow` | S6 | the brief's Tab target at the slow endpoint, `many/` and `huge/` |
| `wan_bad_smoke` | S10, 1.15 | navigation and `ls /many` (one 1.2 MiB reply) on the lossy link |
| `interrupt_non_interactive` | S11 | SIGINT and SIGTERM during `ls huge`: `interrupted`, exit 130 (and not hung: within 10 s) |
| `sigterm_during_startup_lines` | S11b | an interactive run with start-up lines: SIGTERM during `ls huge` exits 130 |
| `interrupt_interactive` | S12 | Ctrl-C during `ls huge` (slow and edge): prompt back, `ls sizes` right after (not hung: within 10 s), 1 new connection |
| `sigterm_in_a_tab_on_the_wire` | I7, final review 1 | at the slow endpoint, SIGTERM while the share list of a Tab is on the wire (the tap has seen its first request, the Tab has not finished): exit 130, terminal back to `icanon` and `echo`, history kept |
| `shares_lost_during_the_probe` | final review 7 | the connection cut while the access probe is on the wire: the list is printed with `null` access, then one `connection_lost` error, no second try; the next `shares` costs 1 new connection and has every access |
| `cut_during_a_listing` | S13 | a cut mid-listing: the listing runs again on a new connection and succeeds |

`tests/serial.rs`:

| Test | Row | Protects |
|---|---|---|
| `silent_link_blackhole` | X3, 1.10 | a silent link fails with `connection_lost` 13–20 s after the request (`--timeout 5`); `pwd` is instant; the next `ls` (reconnect fails) takes 9–13 s with kind `connect`, one dial not two; recovery |
| `silent_link_paused_shell` | X4, 1.10 | the same with a paused server, in the shell |
| `tab_on_a_silent_link` | X8 | Tab on a paused server beeps after 4–8 s, the line is unchanged, recovery with exactly one new connection (through a tap; fails without `abandon()`) |
| `one_connection_fast`, `one_connection_edge` | X1, X2 | the brief's one-connection target, in the tap and on the server (`ctl.sh conns`) |
| `timing_cd` | X5 | `cd` ≤ 1.25 × connect time at slow, ≤ 20 ms at fast; 20 requests in 10 flights for 10 `cd` through the tap first |
| `timing_ls_small` | X6 | small `ls` ≤ 1.25 × connect time at slow; 50 in 10 through the tap first |
| `timing_ls_large` | X7 | `ls many/` and `huge/` times printed beside the baseline; netem dropped nothing |

Unit tests added in Stage 2:

| Test | File | Protects |
|---|---|---|
| `classify_stage2` | session | `not_empty`, a sharing violation and `CANNOT_DELETE` as `other`, fifteen kinds |
| `cache_invalidate` | session | exact entry, subtree, other shares untouched, the root, keys ignore case |
| `valid_names` | session | the names SMB forbids are refused, every fixture name accepted (row 2.19) |
| `registry_refuses_a_second_use` | session | two transfers with one temporary name are never in flight together |
| `cleanup_goes_to_a_new_connection` | session | which failed delete sends the cleanup to a new connection (finding 4) |
| `rename_errors_have_one_wording` | session | the four wordings of a refused rename (finding 11) |
| `fnv_vectors`, `temp_names` | transfer | the temporary name and `is_temp_name` (P16) |
| `local_components` | transfer | names a local file may have (row 2.19) |
| `local_temp_lifecycle` | transfer | lock, drop, commit with and without replace, stale file, a planted link, a directory, a second hard link (finding 5), a target that appears (P17) |
| `target_rules` | transfer | `local_target`, `remote_target`, `~` (decision 4) |
| `case_collisions` | transfer | `Case.txt`/`case.txt` flagged, NFC/NFD pairs not |
| `progress_format` | transfer | sizes and rates at the unit boundaries, the two progress lines |
| `local_walk_rules` | transfer | links and special files not followed, an unreadable directory, a temporary name, a loop by device and inode (finding 16) |
| `local_dirs_never_through_links` | transfer | the local directories of `get -r` (finding 7) |
| `same_local_file_twice` | transfer | two targets that are one local file (finding 6) |
| `walk_visits_each_directory_once`, `walk_depth_cap_without_ids`, `walk_breadth_cap_without_ids`, `walk_goes_on_past_denied`, `walk_width`, `walk_fatal_error_waits`, `walk_max_depth` | walk | the walk on fake trees: the loop check, both caps, errors per directory, 16 at a time, a fatal error waits for the listings in flight, `max_depth` for Stage 3 |
| `flags_of_get_and_mkdir` | commands | `-r`, `-f`, `-p`, `--`, `-rf` is an argument |
| `local_candidates` | shell | local completion: prefix, directories only, `/`, quoting |

`tests/fast.rs`, Stage 2:

| Test | Row | Protects |
|---|---|---|
| `get_put_every_size` | F21, 2.1 | each of the 11 `sizes/` files down and up, sha256 on both sides, `bytes` |
| `threshold_boundary` | F22, 2.1 | 262,143 / 262,144 / 262,145 bytes: the path each took, the round trip |
| `transfer_costs` | F23, 2.15 | the per-command counts of the table above |
| `structure_round_trip` | F24, 2.7 | `tree/` (two items denied), `deep/`, `chain/` down and up; an empty directory; a second `put -r` without and with `-f`; leftovers of the temporary form are never transferred (finding 1); `cat` and `rm` of one |
| `names_round_trip` | F25, 2.8 | `cjk/`, `special/`, `longnames/` byte for byte; the case pair refused on the way up; `rm` and `rmdir` by name until the tree is empty |
| `mangled_names` | F26, 2.9 | `mangled/`: 5 files, 4 `invalid_name` |
| `link_loops` | F27, 2.10 | `loops/` ends, one listing, two item errors |
| `overwrite_rules` | F28, 2.11 | every row of the overwrite table without a prompt |
| `overwrite_prompt` | I21, 2.11 | `Overwrite? [y/N]`: n, Ctrl-C, y; the answer is not in the history |
| `disk_full_tiny` | F29, 2.13, review 3 | `disk_full`, the share empty afterwards and usable; `-f` leaves the old file; `put -r` of three 4 MiB files: two arrive, summary `failed` 1 (the root directory is not an item) |
| `read_only_share` | F30, 2.14, review 8 | every changing command `access_denied`, nothing changed (the names the test tried are absent; other tests' directories may be there); `get` and `cat` work |
| `file_ops_and_cache` | I22, 2.15 | the Tab after `mkdir`, `put`, `rm`, `rmdir`, `put -r` lists once and shows the new state; `not_empty`; `rmdir .` |
| `cat_output` | F31 | 50 MiB binary, peak RSS ≤ 64 MB; `cat \| head`; `--json` objects and refusals |
| `local_commands` | F32 | `lcd`, `lpwd`, `lls`, 0 requests, before `use`; `$HOME` |
| `local_failures` | F33 | a write that fails (`ulimit -f`, bash): `local_io`, nothing left; a read-only local directory |
| `get_r_summary_after_a_fatal_error` | review 3 | the connection cut during `get -r /large`: the summary's `files` + `failed` = the three files |
| `refusal_before_data_keeps_the_connection` | review 7 | `cat --json` of a file over 1 MiB and `get` into a directory that cannot be written: 0 new connections, also for the next command |
| `link_at_the_temporary_name` | review 2 | a link to an existing file at the temporary name, small and stream path: the target untouched, the final name a regular file with the new bytes; a plain stale file replaced; counts of the collision printed |
| `recursive_put_refuses_and_goes_on` | F34, P5, P6 | links, a FIFO, forbidden names, an unreadable directory and file: seven item errors, the rest uploaded, `locked` not created; an unreadable root is the command's `local_io` and creates nothing (review 6) |
| `single_transfer_arguments` | F35, review 4 | where the second argument puts the file; `get` and `put` of a directory; a local link uploads under its own name with its target's content; `put -r .` |
| `progress_on_terminal_only` | I23, 2.21 | `100%` and `MiB/s` on a terminal; nothing on a piped stderr or with `--json` |
| `mkdir_rules` | F36, S2-8 | `mkdir` and `-p` errors and counts; `put -r` onto a remote file |
| `get_r_keeps_inside_its_destination` | F37, finding 7 | a symbolic link in the destination is never followed |
| `every_json_line_has_a_type`, `examples_run`, `docs_match_the_table`, `help_all_needs_no_server`, `counts_add_up` | N4, H3, H2, H1, F14 | extended to the 17 commands; the examples run in the scratch context `help all` states; flights add up except for the two recursive lines (with 16 in flight the count depends on the order of the answers; requests add up exactly) |

`tests/slow.rs`, Stage 2:

| Test | Row | Protects |
|---|---|---|
| `big_file` | S21, 2.2 | 1.1 GiB each way at the fast endpoint, sha256, ≤ 16 outstanding, peak RSS ≤ 64 MB, 0 new connections on the `get` and on the `put` |
| `over_4g` | S22, 2.3 | 4.3 GiB each way (offsets above 2^32), sha256 |
| `two_thousand_small_files` | S23, 2.5 | `smallfiles/` both ways: trees, request counts, peak outstanding, RSS, 0 new connections on each |
| `small_files_counts_slow` | S24, 2.6 | `d01` at the slow endpoint: 304 and 702 requests, 0 new connections on each of the two commands |
| `cut_mid_get`, `cut_mid_put` | S25, S26, 2.12, 2.16 | a cut in the middle: nothing under the final name, no temp; the put's cleanup on exactly one new connection with only the delete on it |
| `kill_9_leaves_only_temporaries` | S30 | `kill -9` leaves exactly the temporary file `temp_name` gives; the next run cleans it up |
| `cut_during_mkdir` | S31, 2.16 | not sent twice; "may or may not" |
| `edge_and_encrypted` | S32, 2.17 | edge (16 credits): medium.bin, `sizes/`, `fanout/`; the client never overspends its credits; `encrypted`: only opaque frames |
| `wan_bad_transfer` | S33, 2.22 | 1 MiB each way on the lossy link |
| `file_shrinks_during_get`, `file_vanishes_during_get_r`, `source_changes_during_put` | S34, S35, S39 | changes during a transfer (5.8, finding 15) |
| `two_processes_one_target` | S36, P15 | two rsmb on one target: the second fails at once and touches nothing |
| `target_appears_during_put`, `target_appears_during_get` | S37, S38, finding 2 | the rename itself refuses a target that appeared; `-f` replaces only what was there at the start |

`tests/serial.rs`, Stage 2:

| Test | Row | Protects |
|---|---|---|
| `timing_small_files` | X21, 2.6 | `d01` down ≤ 1.5 × 8 round trips, up ≤ 1.5 × 22, beside one-after-another |
| `timing_medium_slow` | X22, 2.4 | medium.bin rates at the slow endpoint, printed beside iperf3 |
| `silent_link_mid_get`, `silent_link_mid_put` | X23, X24, 2.18 | a blackhole in the middle: 13 to 20 s; the put names its temp in `left_behind`; the server keeps it "in use" until it notices (finding 3) |
| `leftover_temp_is_reported_once` | review 1 | after that failure (through a tap at the blackhole endpoint, `Client::tapped_at`) the leftover is dropped from the registry: `mkdir`/`rmdir` cost at most the one reconnect, carry no `left_behind`, the same name again is "in use" and never "running", and succeeds once the server lets go |
| `signal_mid_put_edge` | X25, finding 4 | SIGINT at the 50th WRITE on edge, five rounds: nothing left, nothing named, the abort and the delete on the first connection (0 new connections, asserted each round) |
| `signal_in_the_small_file_phase` | X26, findings 9, 13 | SIGINT among 16 small files each way: no summary, every final file complete, the temporaries on the server and the ones `left_behind` names are the same files (`assert_named`) |
| `signals_mid_get`, `signals_mid_put` | X27, X28, 2.12 | SIGINT, SIGTERM, two SIGINTs 10 ms apart: exit 130 within 3 / 4 s; the put's cleanup on the first connection; after the two SIGINTs either nothing is left and nothing named, or the temporary is left and `left_behind` names exactly it |
| `ctrl_c_in_the_shell` | X29, 2.12 | Ctrl-C in the shell during `get` and `put`: prompt back, nothing left, one new connection |
| `one_connection_fast`, `one_connection_edge` | X1, X2, 2.20 | extended by every Stage 2 command: still one TCP connection |

Unit tests added in Stage 3:

| Test | File | Protects |
|---|---|---|
| `walk_stops_when_told` | walk | `on_dir` returning false: nothing started after it, the listings in flight return, `Ok` |
| `walk_takes_results_in_queue_order` | walk | on a fake tree whose earlier listings are slower, `on_dir` in breadth-first order in 20 runs, never more than 16 in flight or waiting; a fake `links/` lists `a/to-c` and reports `b/c` every time (finding 1); fails with completion order |
| `walk_root_parent_is_not_seeded` | walk | a root whose `..` has a real id: no panic, nothing reported; below the root the parent is seeded (finding 7) |
| `walk_lists_the_second_path_when_the_first_fails` | walk | a directory with two paths (the failing one sorted before and after the real one; gone, denied, other): the second path is listed and nothing is reported as already listed (review finding 1) |
| `walk_does_not_enter_a_name_it_could_not_decode` | walk | a directory name with U+FFFD is never sent to the server and is a failure of kind `invalid_name`, not `gone`; the end report makes the walk incomplete (review finding 3) |
| `glob_basics`, `glob_classes`, `glob_unicode`, `glob_fold`, `glob_is_linear` | walk | the pattern table of `commands.md`, `[:class:]` and `/` refused, every fixture name escaped matches itself only, `-iname` folding (`ı`, sigmas, `İ`, `ß`) and its symmetry, no slow pattern (row 3.11) |
| `size_parse`, `filter_rules` | walk | the `-size` forms and refusals (`10b` names 512), strict boundaries, a directory never matches `-size`, the tests ANDed, bad values |
| `tree_drawing`, `end_report_rules` | walk | the exact drawing, notes, counts; the end report's order, reasons (`already_listed` with the path listed, `outside` for the start's parent without one) and `incomplete` |
| `cache_feed_collision_seen_in_the_parent` | session | `Foo` and `foo` in one parent listing, only `foo` listed (and its subdirectory): neither is stored, nor what is below (review finding 4) |
| `cache_units_and_order`, `cache_feed_collision` | session | `len + 1` units, oldest first through the index, no stale index entry, 200,000 puts into 100,000 within 2 s (D3); two directories with one folded key stored by neither (finding 4). `cache_cap` and `cache_invalidate` count units |
| `tree_past_its_limit_draws_nothing` | commands | `TreeNodes` with a limit of 5: the walk is told to stop at 6, `draw_tree_output` fails with `other` naming `-L` and `find`, text and `--json`, and writes nothing (no drawing, no `walk_summary`); at the limit exactly it draws (review finding 5). The product's limit is `walk::TREE_MAX`; the test passes a smaller one |
| `lost_connection_tails` | commands | the tail after a lost connection: `find` that printed says the results are incomplete, `find` that printed nothing says nothing, `put` says it may or may not have taken effect, `ls` nothing (review finding 6) |
| `out_written`, `flags_with_values` | commands | the rerun rule's flag (finding 5); flags that take a value, `--`, twice, open at the end, `positional_index` |
| `no_candidates_for_a_flag_value` | shell | Tab after `find -name ` has no argument kind |

`tests/fast.rs`, Stage 3:

| Test | Row | Protects |
|---|---|---|
| `walk_depth` | F41, 3.1 | `-maxdepth` / `-L` 1, 2, 3 and none equal the server's sets (6, 26, 106, 682); `depth`; depth 0 refused with 0 requests and exit 2; paths from the share root after `cd`; the exact drawing of `tree /tree/d0/d0/d0`; `tree /tree` 685 lines; `find /tree` text with `/` after directories |
| `find_filters` | F42, 3.2 | `-type`, every `-size` row on `sizes/` and `large/` against the server's bytes; `10b`, `[[:digit:]]` refused (0 requests); `-size` with `-type d`; the 60 patterns of the plan's 2.7 on `special/` and `cjk/` equal the server's `find` |
| `walk_counts_fast` | F43, 3.4, 3.7 | `fanout/` 8,004 requests, peak outstanding, one connection; `chain/` 244 / 61 and `bottom.txt` at depth 61; a filter costs nothing; `huge/` 19 requests and peak RSS ≤ 64 MB for `find` and `tree` |
| `walk_past_denied` | F44, 3.5 | one `denied`, exit 0, the denied directory is an entry; the start's own errors exit 1 without `walk_summary`; text lines on stderr, `[access denied]` on the tree line; before `use` exit 2 |
| `walk_link_loops` | F45, 3.6 | `loops/`: 4 / 1, two `skipped`: `/loops/self` `already_listed` with `same_as` `/loops`, `/loops/up` `outside` with `same_as` null (the share root is not listed by a walk of `/loops`); `find` and `tree /loops/self`: `self` names `/loops/self`, `up` is `outside`; exit 0; `-maxdepth 1` skips nothing; no line says "link" |
| `walk_objects` | F46, 3.8 | every object's fields and types, `matched`, `entries`, the order entries · denied · skipped · summary |
| `names_in_patterns` | F47, 3.2 | U+3000 quoted and not, `it's`, three ways to write `[abc]`, `-rf`, `--help*`, a spaced directory, NFC against NFD, `a/b` refused |
| `walk_is_the_same_every_run` | F50, finding 1 | `find /links` and `tree /links` ten times each: the same lines in the same order, `/links/b/c` skipped as already listed as `/links/a/to-c`; `find /fanout` twice in the order of the queue |
| `walk_encrypted` | F49 | `find` and `tree -L 1` on the encrypted share |
| `walk_feeds_the_cache` | I41, U5, finding 4 | Tab below a walked directory 0 requests, at a drawn-only depth one listing, after `find -name ` nothing; a `mkdir` drops the walk's listing; `cs/Foo` and `cs/foo` are stored by neither |
| `every_json_line_has_a_type`, `examples_run`, `docs_match_the_table`, `help_all_needs_no_server`, `counts_add_up` | N4, H1–H3, F14 | extended to 19 commands, `denied`, `skipped`, `walk_summary`; flights add up except for the recursive transfers and the walks |

`tests/slow.rs`, Stage 3:

| Test | Row | Protects |
|---|---|---|
| `walk_counts_slow` | S41, 3.3 | `find /tree` and `tree /tree` 1,368 requests, peak 8 to 64, one connection; the second `find /tree` prints the same lines in the same order |
| `walk_deep_slow` | S42, 3.7 | `deep/` 52 / 13 |
| `walk_edge` | S43, 3.4 | `fanout/` on 16 credits within 60 s, `tree/` 1,368, `tree /many`; the credit balance never below 0 |
| `cut_mid_walk` | S44, 3.9 | a cut at the 100th QUERY_DIRECTORY: `tree` runs again (1 new connection); `find` fails `incomplete` without `walk_summary`; a cut at the first: `find` runs again (finding 5) |
| `find_and_a_closed_pipe` | S46, P13 | `find /fanout \| head -n 5`: five lines, exit not 0 and not hung (10 s), at most 200 requests after the reader left (finding 2) |
| `walk_wan_bad` | S45 | `find /tree/d0` on the lossy link |

`tests/serial.rs`, Stage 3:

| Test | Row | Protects |
|---|---|---|
| `timing_walk_slow` | X41, 3.3 | `find /tree` and `tree /tree` ≤ 1.25 × 23 waves × round trip (measured 4.6 to 4.9 s against 5.75 s), printed beside 342 round trips and Stage 0's 18.5 s; netem dropped nothing |
| `signals_mid_walk` | X42 | SIGINT and SIGTERM at the 200th QUERY_DIRECTORY of `find /fanout`, SIGINT in `tree /fanout`: `interrupted`, exit 130 within 3 s, no `walk_summary`, whole lines; `tree` printed nothing |
| `ctrl_c_mid_walk` | X43 | Ctrl-C in the shell: prompt within 3 s, `ls /sizes` on one new connection, `find /tree/d0` 169 lines on none |
| `walk_root_loops` | X44, 3.6 | `find / -maxdepth 4 -name self` and `tree -L 3 /` end; `skipped` and `denied` exactly; `directories` = 1 + the server's directories above the depth − 3 |
| `one_connection_fast`, `one_connection_edge` | X1, X2, 3.10 | extended by `tree -L 2 /tree`, `find /tree -name file.txt`, `find /fanout -maxdepth 1 -type d` |

Not in the gate (Stage 3), and why: a walk of a million entries (`xl/` is opt-in; the `tree` limit
is tested without a server by `tree_past_its_limit_draws_nothing` with a smaller limit, the walk's
stop by `walk_stops_when_told`, and the memory numbers at 500,000 entries are synthetic); a server without file ids (the unit tests cover the caps); two unrelated directories
with one file id (no fixture can make them on one file system; `links/` covers two paths to one
directory); the `-iname` names of the plan's 2.10 against the server (not in the image; the unit
test has them); `fanout/` at the slow endpoint (26 s; in the bench); a silent link during a walk
(Stage 1's path, tested for `ls`); a Windows server.

The longest tests: `over_4g` (4.3 GiB down and up and two checksums over it: it cannot be shorter
without leaving out the offsets above 2^32 it exists for), the silent-link tests and `silent_proxy`
(15 to 36 s, by design of the deadline and the 10 s dial), X25 (five rounds of a 300 MiB upload
on edge), `timing_cd` / `timing_ls_small` (about 20 s each), `idle_sends_nothing` (12 s, in both
runs of `tests/fast`). Not in the gate, and why: the read-only attribute (this Samba does not
present it), a real DFS share, a Windows server, a local file system that folds case or
normalises names (the rule is tested with hard links in `same_local_file_twice` and by
`target_appears_during_get`), two uploaders of **different** files to one name (a documented
limitation), a `put` whose rename is cut before its answer (it cannot be hit on purpose).

No gate test shows a slow reply surviving end to end: on wan-bad `many/` arrives in a few seconds,
too close to both limits (15 s without progress, 6 × `--timeout` with it) for a test that neither
passes blindly nor fails by chance, and `huge/` takes two minutes. The unit test
`receive_reports_progress` covers our half; the wan-bad row of `testenv/bench-rsmb.sh` covers the
whole (`docs/performance.md`).

## Stage 4: the proxy

Unit tests:

| Test | File | Protects |
|---|---|---|
| `socks5_handshake_bytes` | session | the bytes rsmb sends, read at fake proxies on 127.0.0.1 and `[::1]`: greeting `05 01 00` without credentials; for `fileserver.invalid` the request `05 01 00 03`, the length, the name, the port (a name, not an address); `10.1.2.3` as type 1, `::1` as type 4; with `socks5://us%40er:p%3Ass%2F%25w%C3%B6rd@…` the greeting `05 02 00 02` and the login `01 05 us@er 0b p:ss/%wörd`; `TCP_NODELAY` direct and through the proxy (replaces `dial_sets_nodelay`) |
| `proxy_errors` | session | each answer of a fake proxy in rsmb's words (method `ff` without and with credentials, login status 1, replies 1 to 8, an HTTP answer, a close after the greeting): kind `connect`, `proxy <host>:<port>: `, the target named, never the password, exit 3 at start-up |
| `dial_says_how_far_it_got` | session | a proxy that accepts and never answers: `connected to the proxy, but the SOCKS5 handshake did not finish within 200 ms` with the three causes and the target, within a second; `DIAL_TIMEOUT` is 10 s; `hostport` brackets IPv6 |
| `banner_names_the_proxy` | session | the banner holds the proxy's address and neither its user nor its password; no `proxy` without one |
| `args_are_sound`, `table_is_complete`, `help_all_has_everything` | main, commands | the help of `--proxy` says the proxy resolves the name, the help of `--timeout` names the 10 s dial; 8 topics, the topic `PROXY` ("never a reason to connect directly"), its example, the JSON paragraph without a prompt on stdout and with the non-interactive rules |
| `socks5_preamble_is_relayed_and_not_counted` | smbtap | the tap's SOCKS5 relay: each message in one write (a fake proxy reads each with one `read`), 2 requests in 1 flight on 1 connection with and without a login, a refusal (`05 ff`) closes with 0 frames and no live connection, `drop_connections` closes a handshake that stalls |

`tests/fast.rs` (both runs):

| Test | Row | Protects |
|---|---|---|
| every test above | 4.1 | everything of Stages 1 to 3 through the proxy, with the same assertions, counts and checksums |
| `proxy_urls_at_startup` | F61, 4.6, 4.9 | each refused URL form: exit 2, kind `usage`, the parser's `expected socks5://` (not clap's refusal of a second `--proxy`), empty stderr with `--json`; `socks5://user1:secretpw@…` and `socks5://alice:hunter2` show no part of the credentials; `-H` of 256 bytes with `--proxy` is `usage` naming 255, of 255 bytes reaches the proxy (exit 3, `connect`); the message of a 255-byte `-H` that reached the proxy starts with `proxy `; five accepted forms work against the private server; `help all` has `PROXY` and its example |
| `environment_is_not_read` | F62, P8 | `ALL_PROXY`, `all_proxy`, `SOCKS_PROXY`, `RSMB_PROXY` pointing at a closed port change nothing |
| `json_never_starts_the_shell` | I61, P12 | `--json` on a pty: no ESC byte, no prompt, every `{` line parses; typed lines then Ctrl-D exit 0 without a banner; a failing typed line ends the run with 1 before the next, with `--keep-going` both run and Ctrl-D exits 1; `get` onto a local file is `already_exists` without `Overwrite?` |
| `interactive_basics` (extended) | I1, P11 | the banner names `through proxy 127.0.0.1:<tap port>` in the proxied run and holds no `proxy` direct |
| `startup_failures` (extended) | N5 | `--proxy http://x` with its own command line: exit 2, `usage`, `expected socks5://` |
| `docs_match_the_table` (extended) | H2 | `docs/commands.md` has the section "Proxy" with every message of a proxy failure |

`tests/slow.rs`:

| Test | Row | Protects |
|---|---|---|
| `proxy_with_login` | S61, 4.2 | every command of the session script through port 1081: the downloaded and uploaded `65537.bin` equal the server's, `put -r d01` equals `smallfiles/d01`, `find /tree -name file.txt` 340 entries equal the server's `find`; 1 TCP connection in the tap |
| `big_file_through_the_proxy` | S62, 4.8 | `big.bin` both ways: sha256 on both sides, 1,181,116,007 bytes, at most 16 outstanding, peak RSS ≤ 64 MB, 0 new connections |
| `proxy_failures` | S63, 4.6 | every failure of the plan's 2.5 with its message: wrong password and user, no credentials, a closed proxy port (also with `-H 10.213.45.10`, a server reachable without the proxy: it must not be dialled), target port closed, a name the proxy cannot resolve, an absent address (≤ 12 s), an SMB server taken for a proxy (≤ 12 s, the three causes); every other case within 10 s (a no-hang bound); exit 3, one `error` with `command` null and no `done`, the text line `rsmb: <message>`, no output holds the password; a wrong SMB password is `auth` and does not name the proxy |
| `no_way_around_the_proxy` | S64, 4.4, 4.5 | the private name does not resolve on this host; direct by name and by address fails (`connect`, ≤ 12 s, no lower bound); through the proxy by name, by address and in mixed case it lists 11 names; the loader probe: with `LD_DEBUG=files` the control (direct to the name) loads `libnss_` modules (6 here) and the proxied run by name none (prints `leak probe blind on this host` where the control loads none) |
| `cut_through_the_proxy` | S65, 4.7, 4.8 | an idle cut (`pwd` 0, `ls` 9 requests on 1 new connection); a cut at the first QUERY_DIRECTORY of `ls /huge` (run again, 100,000 names, 1 new connection); at the 100th READ of `get big.bin` (`connection_lost`, the "not run again" tail, nothing local, 0 new connections); at the 100th WRITE of `put big.bin up.bin` (nothing under the name or the temporary name on the server); then, through a tap to the proxy and on to `samba`, the tap dropped: `ls` fails twice with `reconnect failed: proxy 127.0.0.1:<port>` and no entries (no fallback, finding 1), `pwd` works |
| `signals_through_the_proxy` | S66, 4.8 | SIGINT and SIGTERM at the 100th READ of `get big.bin`, SIGINT at the 100th WRITE of a `put`: `interrupted`, exit 130 and not hung (10 s), nothing left on either side, no `left_behind` |

`tests/serial.rs`:

| Test | Row | Protects |
|---|---|---|
| `one_connection_through_the_proxy` | X61, 4.1, 4.3, 4.7 | for ports 1080 and 1081 the whole session script: 1 connection in the tap, 1 on the private server (`ctl.sh conns samba-private`), 1 more line in the proxy's log; with `-H FileServer.RSMB.Test` the log line has that spelling; a cut and `ls`: 1 more line and 2 connections on the server |
| `silent_proxy` | X62, 4.6 | `ctl.sh pause socks` under a guard, `--timeout 5`: `ls` fails after 13 to 20 s with `connection_lost`, `pwd` at once, the next `ls` after 9 to 13 s with kind `connect` and `reconnect failed: proxy 10.213.45.20:1080 … the SOCKS5 handshake did not finish within 10 s … is silent`; after the unpause 11 names |
| `ctrl_c_through_the_proxy` | X63, 4.8 | Ctrl-C in the shell at the 100th READ of `get big.bin`: `interrupted`, the prompt back within 3 s, nothing local, `ls /sizes` on exactly 1 new connection, `pwd` unchanged |

Every proxy log count and every server-side connection count is in `tests/serial.rs`: both are
shared by everything running; a parallel test counts connections in its own tap.

Not in the gate (Stage 4), and why: a proxy other than microsocks (none in the environment);
latency between client and proxy (the environment has none; computed in
`docs/performance.md`); a proxy that refuses a login on a reconnect (the credentials of a session
do not change); an IPv6 server; the packet capture of the plan's 2.7 (repeated by hand at the end
of the stage, `docs/performance.md`); `tests/slow.rs` through the proxy (single commands cost the
same, plan 2.10); the wall time of anything through the proxy (it measures microsocks;
`docs/performance.md` records the planning measurement); a target whose SYN is dropped behind the
proxy, and a proxy paused at start-up (the same dial and message as `silent_proxy`'s second
`ls`).

## Stage 5: Kerberos

Unit tests (all in `src/session.rs` but the last two):

| Test | Protects |
|---|---|
| `ccache_paths` | `KRB5CCNAME`: unset and empty give `/tmp/krb5cc_1000` for uid 1000 and say "default"; `FILE:/tmp/x`, `/tmp/a:b`, `./c`, `plain` are paths and not default; `KCM:`, `KCM:1000`, `KEYRING:persistent:1000`, `DIR:/run/x`, `MEMORY:y`, `API:z` are `auth` errors naming the type and `cache file`; a name with byte `\xe9` (plain, and after `FILE:`) is the variable's, not the default (review finding 2) |
| `ticket_choice` | `pick_ticket` on caches built in the test: a valid service ticket (user and realm as the cache spells them; `-H` in another case finds it); a TGT only; an expired service ticket beside a valid TGT (the KDC is asked, the entry is gone); everything expired: the server's name and the literal `expired at 2026-11-18T19:25:28Z` for the end time 1,795,029,928 (written by hand, not computed); another host and no TGT: `holds no ticket for cifs/<host>@<realm>`; the hint `KRB5CCNAME is not set, so the default file` only for the default file; configuration entries (end time 0) never count; `-u TESTER -d rsmb.test` pass and keep the cache's spelling; `-u other` and `-d OTHER.TEST` name both sides; a principal with two parts. Every error `auth`, starting `Kerberos: ` |
| `krb5_conf_kdc` | `krb5_kdc`: two realms, the first `kdc` of several, `kdc=h`, comments and tabs, `h:750`, `[::1]:88`, `[2001:db8::1]`, a bare `2001:db8::1` (the whole value, port 88), `tcp/h`, `udp/h:88`; a `kdc` under `[libdefaults]`, in `[domain_realm]`, in another realm's block or in a nested block (`auth_to_local_names = {`) is not taken, nor the next realm's after a nested block; the realm compared exactly; an HTTP KDC skipped |
| `kdc_files` | `krb5_files(None)` and `(Some(""))` are `/etc/krb5.conf`, `"/a:/b"` is both in order; `kdc_for` on files the test wrote: the first that names a kdc wins and is returned with it, a missing path and a directory count as absent, none: `("rsmb.test", 88, None)`; a `KRB5_CONFIG` with a non-UTF-8 part is that part, never `/etc/krb5.conf`. Nothing reads the environment or `/etc` |
| `non_regular_files_are_not_read` | a FIFO with no writer: `open_regular` says `not a regular file` and `kdc_for` treats it (and a directory) as absent, on a thread that must answer within 5 s (without `O_NONBLOCK` it blocks and the test fails on the bound; checked by removing the flag) |
| `cache_owner_check` | the owner check: equal passes, different is `auth` naming both uids and `own user only` |
| `kerberos_refusals` | `kerberos_refusal`: `STATUS_LOGON_FAILURE` keeps `login refused`; `STATUS_INVALID_PARAMETER` is `the server did not accept the ticket` with the status; the crate's own `Auth` and `InvalidData` (no status) are `could not be used` with the crate's text and no word about the server; `Disconnected` is a lost connection |
| `kdc_errors` | `kdc_error` for 7, 13, 32, 36, 37 and 99 in the words of `commands.md` |
| `kdc_relay_carries_the_exchange` | the relay against a fake KDC on a local TCP port and a UDP client (one socket, as the crate's) standing for the crate: each datagram arrives as 4 bytes of length and the same bytes, the answer comes back as one datagram, a second one too; a KRB-ERROR 7 ends it with `auth` and `has no service`, and the client gets nothing; a KDC that accepts and never answers, limit 200 ms, 20 at once: `connect`, `connected to the KDC, but it did not answer within 200 ms` every time (one timer at a time), within 2 s; a closed port: `connect`, `Connection refused`; a KDC that closes after the request: `connect`, `closed the connection without an answer`; through a fake proxy the KDC's name goes as a name (`05 01 00 03`, the length, `kdc.invalid`, port 88); a proxy that answers reply 5: `connect`, `proxy <p>: cannot reach <kdc>:<port>: connection refused`, for a name and for the fake KDC's own address, and the fake KDC accepted nothing (no way around the proxy); a silent proxy, 20 at once: `open_tcp`'s handshake message every time; the tails (`the KDC is the one <file> names`; `so the realm's name was tried`, `KRB5_CONFIG`, `kvno cifs/`, never `/etc/krb5.conf`). Every message starts `Kerberos: asking the KDC` |
| `kdc_relay_serves_the_first_sender_only` | a second local socket's datagram is not forwarded (the fake KDC logs 2 requests, both the first sender's), gets no answer, and the first sender's exchanges go on (fails with the sender check removed) |
| `classify_statuses`, `banner_names_the_proxy` (extended) | `SessionExpired` is `connection_lost` and says `ended the session`; the banner says `'RSMB.TEST\tester' (Kerberos) on` with `-k` and no `Kerberos` without |
| `args_are_sound`, `table_is_complete`, `help_all_has_everything` (extended) | main, commands: `-k` sets `kerberos`, `-k -p x` and `-k --no-pass` are errors, `-k -u u -d D` parses, `--keep-going` has no short form, the help of `-k` names `KRB5CCNAME`; 9 topics, the topic `KERBEROS` with its words (and that the end of a ticket is a reading command run again, a changing one `connection_lost`, not the old "the next command fails"), the example line, no `unless -k` and no `-k (--keep-going)` left, the synopsis with `[-k] [-u <user>]` and `[--keep-going]` |

`tests/fast.rs` (both runs: `dc()` is the DC by address, then by name through port 1080):

| Test | Row | Protects |
|---|---|---|
| `kerberos_login` | F71 | a tapped session with the service ticket for `dc().spn(true)`: start-up 2 requests in 2 flights on 1 connection (NEGOTIATE 1, SESSION_SETUP 1); `use share` 1 / 1; `ls /` is `hello.txt`, `sub`, `upload`; `get /sub/1m.bin` and a `put` of 300,000 bytes (a signed write) with sha256 on both sides; `cat /hello.txt` on `encrypted`; exit 0, stderr empty. Direct also a TGT-only session with `krb5_conf`: again 2 / 2 on 1 connection in the tap, `ls /` works |
| `kerberos_tgt_only` | F72 | one-shot, not tapped, a TGT-only cache (`-f`): direct with `krb5_conf`, proxied with `KRB5_CONFIG=/dev/null` (the KDC is `rsmb.test`, which only the proxy resolves): exit 0, 3 entries. A cache made with `-F`: exit 3, `auth`, `command` null, no `done`, `KDC_ERR_BADOPTION`, `kinit -f`, `kvno cifs/` |
| `kerberos_cache_errors` | F73 | through a tap, text and `--json`: a missing file (the path, `No such file`, no default-file hint), `KCM:`, `DIR:/x`, a text file, an expired copy, `-u nobody`, `-d OTHER.TEST`, `/etc/hostname` (a file of another owner: `belongs to uid <owner>`, `own user only`; skipped with a printed line where it is the test's own): each exit 3, `auth`, starting `Kerberos: `; **the tap saw 0 connections**. Then `-u TESTER -d rsmb.test` logs in |
| `kerberos_cache_odd_files` | F73b | one-shot, not tapped: a service-ticket cache copied to a path with byte `\xe9` and named by `KRB5CCNAME` (bytes, not UTF-8) logs in and lists 3 entries (it failed with the default file `/tmp/krb5cc_<uid>` and the "not set" hint before); a FIFO and a directory as the cache: exit 3, `auth`, `ticket cache <path>`, `not a regular file`, within 5 s (before, the FIFO hung until the harness's 60 s limit) |
| `kerberos_usage` | F74 | `-k -p x`, `-k --no-pass`: exit 2, `usage`; `rsmb -H 127.0.0.1 -P 1 -k --json -N -C pwd` with the harness's `KRB5CCNAME`: exit 3, `auth`, `ticket cache /nonexistent/rsmb-test/krb5cc` and `No such file` (no password asked, the cache read before port 1 was dialled); `help all` has `KERBEROS`, `-k, --kerberos` and the option `--keep-going` without a short form |
| `kerberos_reconnect_reads_the_cache_again` | F75 | the cache renamed away and the connection cut: `ls /` fails, `auth`, `reconnect failed: Kerberos: ticket cache <path>`, 0 connections; `pwd` works; the file back: `ls /` on exactly 1 new connection with 8 requests; the file replaced by a cache of `kother`, a cut: `ls /` fails, `auth`, naming `kother@RSMB.TEST` and `tester` (the session keeps its principal) |
| `kerberos_banner` | F76 | on a pty with `-k` and a cache: the banner `logged in as 'RSMB.TEST\tester' (Kerberos) on`, no `Password`, `through proxy` exactly in the proxied run |
| `docs_match_the_table` (extended) | H2 | the section `## Kerberos` of `commands.md` holds every message stem of the code (the unit tests assert the same words in the code) and both kinds, `auth` and `connect` |

`tests/slow.rs`:

| Test | Row | Protects |
|---|---|---|
| `kerberos_kdc_is_asked_through_the_proxy` | S71 | TGT-only caches of `kdirect` and `kproxy`, which no other test uses; `kdc_requests` waited for, then exact. The control: direct to 10.213.45.12 with `krb5_conf`, `kdirect` from 10.213.45.1 rose by exactly 1. Through port 1080 by name, with a `krb5_conf` whose KDC the host reaches directly: `kproxy` from 10.213.45.20 +1, from 10.213.45.1 unchanged from the count taken before (the log is the container's whole log, so a count of 0 would fail for one old direct request; now and at the end), the proxy logged `connected to 10.213.45.12:88`; again with `KRB5_CONFIG=/dev/null`: +1 from the proxy, the proxy logged `connected to rsmb.test:88`; again through port 1081 with a login. With the service-ticket cache of `kproxy`: no new line at all |
| `kerberos_failures` | S72 | text and `--json`, exit 3, one `error`, `command` null, no `done`: a KDC with port 88 closed (`connect`, within 2 s, `Kerberos: asking the KDC 10.213.45.10:88`, `refused`, the file's name); no kdc in the files direct (`connect`, `KDC rsmb.test:88`, `so the realm's name was tried`, `KRB5_CONFIG`, `kvno cifs/10.213.45.12`); an SPN the realm lacks (`-H 10.213.45.11`: `auth`, `has no service`); a valid ticket at `samba`, which does no Kerberos (`auth`, `did not accept the ticket`); through port 1080 a KDC name the proxy cannot resolve and an absent address (`connect`, `proxy 10.213.45.20:1080: cannot reach …`, within 12 s); a closed proxy port with a server and KDC the host reaches directly (`connect`, the message starts `proxy 10.213.45.20:1082`) |
| `no_way_around_the_proxy` (extended) | S64 | a TGT-only Kerberos login through the proxy by name with `KRB5_CONFIG=/dev/null` (KDC `rsmb.test`) works and loads no `libnss_` module, beside the control that loads some |

`tests/serial.rs`:

| Test | Row | Protects |
|---|---|---|
| `silent_kdc` | X71 | `ctl.sh pause samba-dc` under a guard, a TGT-only login at `samba` with `krb5_conf`: exit 3 after 9 to 13 s (10.0 s measured), `connect`, `KDC 10.213.45.12:88`, `connected to the KDC, but it did not answer within 10 s`; after the unpause a login at the DC works |
| `kerberos_connections_through_the_proxy` | X72 | through port 1080 by name with `KRB5_CONFIG=/dev/null`: a TGT-only login is exactly 1 more `connected to rsmb.test:88` and 1 more `connected to dc.rsmb.test:445` in the proxy's log; with the service ticket 1 more `:445` and 0 more `:88` |

Not in the gate (Stage 5), and why: a session that outlives its ticket (the shortest ticket this
KDC issues for a service is 121 s, and the server accepts it five minutes past its end; measured
by hand while planning; the message's mapping is a unit test); a Windows KDC or server, an MIT
KDC; a cache written by impacket (it could not make one against this KDC); latency in front of
the DC (the round trips are counted); a TGT that is forwardable and not renewable (Heimdal's
default; measured by hand; the gate has the not-forwardable case); the cache file that aborts the
crate's parser (decision K13); the default file `/tmp/krb5cc_<uid>` through the binary (the
harness keeps every test away from it; `ccache_paths` and `ticket_choice` hold the rule and the
hint); a KDC found by the realm's name without a proxy (the host cannot resolve `rsmb.test`; S72
asserts the failure and its message); a realm with more than one KDC, a `krb5.conf` with
`include`; a clock difference between client and KDC (the containers share the host's clock).
