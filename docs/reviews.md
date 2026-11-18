# Reviews

One section per stage review. Every finding was reproduced by the orchestrating session before
anything was fixed (AGENTS.md, process step 5). Severity is the fixing agent's rating from the
finding's effect; the review did not assign one.

## Stage 0 (2026-11-18)

| # | Severity | Finding | How validated | Outcome |
|---|---|---|---|---|
| 1 | high | `validate-smbtap.sh` passes with no SMB traffic | Run: `./testenv/validate-smbtap.sh 10.213.45.20` exited 0 with 0/0 counts | Fails when smbclient fails or fewer than 20 requests are captured; `dumpcap` killed by a trap. The same command now exits 1; the default address still passes (86/86) |
| 2 | high | smb2 slow-endpoint listing times taken on a fresh connection, median of 1; smb2 looks about 2x slower | Read `spikes/smb2/src/main.rs` ~750-758; the reviewer's own warm measurement agreed | Step 4 reports warm (median of 3) and fresh; re-run saved as `spikes/smb2/results-step4-warm-2026-11-18.txt`; README, `docs/library-decision.md` and AGENTS.md updated |
| 3 | high | Gate 2 reinterpreted after measuring, documents say "pass" | Read the plan and `docs/library-decision.md` | Gate table, README and AGENTS.md say neither crate meets it as written and that it was reinterpreted equally for both |
| 4 | high | Gate 5 inputs differed between crates; exclusion of smb-rs implied mechanical; gate 7 for smb-rs unstated | Read `docs/library-decision.md` and the result files | Both cases per crate and untested cases stated; actual reasoning for excluding smb-rs given; gate 7 stated as failing read literally |
| 5 | medium | `check.sh` used `conns-reset` and the blackhole without the lock that `docs/measuring.md` describes | Read `check.sh` and `docs/measuring.md` ~150 | `check.sh` takes the exclusive lock; one "The lock" section in `docs/test-environment.md` |
| 6 | low | Docs claim `check.sh` checks that `samba-private` admits only the proxy; it does not | Read `docs/test-environment.md` 128-137 and `check.sh` | Sentence corrected: configured, not checked |
| 7 | medium | Neither spike checks netem drops after a run; failing `docker exec` read as 0 drops | Read both `run.sh`, `check.sh` 189, `bench-smbclientng.sh` 83 | New `ctl.sh drops` (fails when `wan` cannot be asked), used by `check.sh`, the benchmark and both spikes, which also record `smbd -V` |
| 8 | medium | Directory-check verdict prints "right x5" although `forbidden/` accepts SUCCESS or ACCESS_DENIED | Read both `main.rs` (~679, ~884) | Label shows the forbidden class; masks that deny `forbidden/` documented |
| 9 | low | smb2 API-compatibility check ignores `cargo check`'s exit status | Read `spikes/smb2/run.sh` 38-44 | FAIL recorded when cargo exits non-zero with zero counted errors (not exercised) |
| 10 | low | Stale or inconsistent docs: "about 220 round trips", `PATCHED=1` comment, "about 110 lines", start-up times | Read the cited lines | All corrected; the result file left untouched, the difference noted in `docs/library-decision.md` |
| 11 | medium | An interrupted spike step 10 can leave samba paused or the blackhole on | Read both `run.sh` | Traps in both `run.sh`; `up.sh` unpauses and turns the blackhole off. Checked by interrupting step 10 |
| 12 | medium | smb2 step 7 does not assert one TCP connection (`conns 0`) | Read `spikes/smb2/src/main.rs` step 7 and its result row | Spike reports distinct connection ids and live connections for the walk; see below |

Item 12: step 7 was re-run by the orchestrator; result in
`spikes/smb2/results-step7-conns-2026-11-18.txt`.

### Rejected

- Autoload of `sch_netem` by the routers: accepted in the approved plan.
- Router healthcheck does not check the delay value: covered by the RTT assertion in `check.sh`.

### Checked and found correct

- smbtap counting: compound chains, interim responses, split frames.
- Fixtures against the brief and its amendments.
- Every documented number that has a result row.
- No root needed.
- No machine-specific paths.

## Stage 1 (2026-11-18)

| # | Severity | Finding | How validated | Outcome |
|---|---|---|---|---|
| 1 | high | A failed listing is never remembered: every Tab in `traverse-only/` or in a missing directory sends a full listing (5 requests) | Reviewer's reproduction; read `Session::list_cached` and `ShellHelper::bounded`; the new pty test failed first (second Tab cost 5 requests) | `list_cached` stores an empty listing for a refused listing (not for a lost connection, and not for a failed reconnect: it reconnects first); `refresh` and the next `ls` replace it. `failed_completion_is_remembered`: 1 attempt, then 0, 0, one again after `refresh`; `ls` still asks and reports `access denied` |
| 2 | medium | A failed reconnect is retried (`ls` on a dead link takes 20 s, not 10 s) and a refused login on reconnect is reported as `connection_lost`, not `auth` | Reviewer's reproduction; read `ensure_connected` and `run_parsed`; the extended `silent_link_blackhole` failed first (kind `connection_lost`, about 20 s) | `ensure_connected` keeps the kind of the login error; `run_parsed` does not rerun a command that started on a dead connection (`Session::is_dead`). The test now sees kind `connect` after 9–13 s. The `auth` case is not exercised end to end (no test server rejects a login after the first) |
| 3 | medium | SIGTERM at the prompt or in a Tab exits with the terminal raw and the history unsaved | Reviewer's reproduction; read `install_signals` and `shell::run`; the new test failed first | `shell::run` saves the terminal attributes before the first prompt and the signal task restores them before `exit(130)`; each line is appended to the history file as it is entered. `sigterm_in_the_shell` (prompt and Tab): exit 130, `icanon` and `echo` back, history has `pwd`. `sigterm_during_startup_lines` added for the untested start-up case (passes: that path already exited 130) |
| 4 | medium | `tab_on_a_silent_link` passes without `sess.abandon()` in the Tab-timeout branch | Reviewer's mutation; repeated here | The test runs through a tap and asserts exactly one new connection for the `ls` after unpause. With `abandon()` commented out it failed (0 new connections); restored, it passes |
| 5 | low | Nothing protects `TCP_NODELAY`: `set_nodelay(false)` in both dial paths passes the suite | Reviewer's mutation; repeated here | The socket code is `open_tcp`; unit test `dial_sets_nodelay` covers the direct path and the SOCKS path (a local fake proxy of 15 lines) and fails with `set_nodelay(false)`. The "about 40 ms" rationale in `docs/plans/master-plan.md` is corrected: the wall-time bounds do not catch it |
| 6 | medium | `rsmb --json --bogus` prints clap's text on stderr and nothing on stdout, against `docs/scripting.md` and `help all` | Run: `rsmb --json --bogus` | `try_parse`; with `--json` among the arguments one JSON `error` of kind `usage`, exit 2, empty stderr; `--help`, `--version` unchanged. `--json` in a terminal session is documented as unsupported (prompt on stdout), not redesigned. Test `flag_errors_in_json` |
| 7 | low | `help all` shows no defaults (`rsmb --help-all \| grep -c 445` was 0) | Run | Options that take a value show `[default: 445]` and `[default: 30]`, in text and in the `option` objects; `docs_match_the_table` checks the documents |
| 8 | low | Exit-code tables incomplete: "no share selected" and unknown commands exit 2; 130 also for SIGTERM in the shell and an interrupted start-up connect | Run (`rsmb -H … -C ls`: 2; `-C bogus`: 2) and read `Error::exit_code` | One wording in `help all`, `docs/commands.md` and `docs/scripting.md`; `docs_match_the_table` fails when a row differs |
| 9 | low | With `-k`, a closed stdout does not stop the run | Run: `rsmb -k -C 'use share' -C 'ls /many' -C 'ls /many' -C pwd \| head -1` printed the error repeatedly | `run_blocking` ends the run with exit 1 once a stdout write has failed, whatever `-k` says; documented in `help all`, `commands.md`, `scripting.md`. Test `closed_stdout_ends_a_keep_going_run` (failed first: four messages, three listings) |
| 10 | low | `docs/architecture.md`: `list_dir` signature, `mark_dead` caller, "no duplicates", who creates the interrupt future | Read against the code | All four were true and are corrected (`what` parameter; `run_parsed`; only consecutive duplicates are dropped; `interruptible()`) |
| 11 | low | `AGENTS.md` step list omits "stale test directories"; `docs/development.md` says every count assertion prints endpoint, counts and log, which three places did not | Read | `AGENTS.md` lists the step; `tapped_cost`, the edge branch of `check_tab_many` and the `shares_fast` counts print through `common::context`; the sentence in `development.md` now says which helpers print what |
| 12 | low | `--proxy` with percent-encoded credentials and an IPv6 host were never checked | Read `parse_proxy`; unit test | Neither worked (no decoding; `[::1]` kept its brackets and could not be dialled). Now credentials are percent-decoded and `socks5://[::1]:1080` is parsed; bad escapes and an unbracketed IPv6 are usage errors. Unit test `proxy_urls`; not tried end to end (the test proxy is IPv4 without login), noted in `docs/known-limitations.md` |

### Checked and found correct

- Path handling: `..`, absolute paths, spaces, quotes, U+3000, NFC against NFD, case.
- Error kinds and their messages.
- Costs on the edge endpoint.
- Recovery after a share is closed.
- Completion with quotes and nested spaces.
- Every example of `help all` runs.
- Mutations the suite catches: `cd` doing a listing, a dropped cache, `ls` answered from the cache,
  a retry after a silent server.

## Stage 2 (2026-11-18)

| # | Severity | Finding | How validated | Outcome |
|---|---|---|---|---|
| 1 | medium | A temporary file that the cleanup could not delete stays in the session registry: every later command reruns the cleanup, gets a sharing violation and replaces the connection (connections 1 → 3 → 4 → 5 → 6 → 7 over a retried `put -f`, `cat`, `mkdir`, `rmdir`); a retry of the same name is refused as "running"; unrelated errors carry the old `left_behind`; the registry has no share | Read `register`, `finish`, `cleanup_inner`; the reviewer's tap counts | `cleanup` ends by emptying the registry (`take_left`, formerly `temps_left`, also used after an interrupt whose cleanup was cut short): a leftover is named once, by the command that left it. The registry is empty between commands, so a cleanup only acts on its own command's share. `leftover_temp_is_reported_once` (serial; a tap in front of the blackhole endpoint, `Client::tapped_at`) failed first with "mkdir a: 2 new connections since the failure"; it now asserts at most one reconnect over four commands, no `left_behind` on them, "in use" and never "running" for the retry, success once the server lets go. `silent_link_mid_put` (X24) unchanged and passing |
| 2 | medium | A symbolic link at the temporary name is followed: `put x.txt` wrote through `.rsmb-….part -> victim.txt` into `victim.txt` and the final name became a link | The orchestrator reproduced it on the server | The temporary file is created create-only (`write_file_compound_exclusive`, `create_file_writer_exclusive`); a collision removes the object and repeats the create once (`Remote::take_name`). Measured on Samba 4.23.8: a plain delete of the link deletes its **target**; opening with `FILE_OPEN_REPARSE_POINT` (and delete-on-close) removes the link itself. The common case is unchanged (small 7 / 3, large 14); a collision costs a failed CREATE compound, the removal and the repeat (14 requests for a small file, printed by the test). `link_at_the_temporary_name` (small path, stream path, a plain stale file) failed first (the victim was overwritten). **Not fixed:** a *dangling* link at the temporary name: Samba creates its target even with `FileCreate` (measured), and a create with `FILE_OPEN_REPARSE_POINT` that would refuse it costs one more request and round trip on every `put`, against the brief's cost rule. Documented in `docs/transfers.md` and `docs/known-limitations.md` |
| 3 | medium | The summary's `failed` is one too low after a fatal error in `put -r` and `get -r`: `total` leaves out the root directory, `directories` counts it | The orchestrator reproduced it (`use tiny`, three 4 MiB files: `"failed":0,"files":2`) | `finish_tree` adds the root back. `disk_full_tiny` (extended) failed first with `failed` 0 and now asserts files 2, failed 1; `get_r_summary_after_a_fatal_error` (a cut connection during `get -r /large`) failed first with `files + failed` = 2 of 3 and shows the download side was wrong too. A *local* disk full cannot be provoked without privileges (`ulimit -f` gives `EFBIG`, which is not fatal), hence the cut |
| 4 | medium/low | `put` of a local symbolic link uploads under the target's name (`link.txt -> real.txt` replaced the remote `real.txt`) | The orchestrator reproduced it | The remote name is the one the user gave; only a path without a name (`.`, `..`) is resolved. `single_transfer_arguments` (extended: link, `put -r .`) failed first (`link.txt` absent on the server). `docs/commands.md` and `help put` say so |
| 5 | low | `help put` and `docs/commands.md` say 12 requests for a small `put` refused because the target exists; the code and `transfer_costs` say 13 in 6 flights | Read the three places | Both corrected (the plan's deviation I4 had recorded 13 already). Fix 2 does not change this count |
| 6 | low | `put -r` of an unreadable local root created the remote directory and reported `directories:1`, against `docs/transfers.md` | The orchestrator reproduced it (`chmod 000 locked; put -r locked`) | `put_r` reads the root first: a root that cannot be read is the command's `local_io` before the server is touched. `recursive_put_refuses_and_goes_on` (extended) failed first (a summary, kind `other`) |
| 7 | low | A refusal before any data was read (`cat --json` of a file over 1 MiB, `get` into an unwritable directory) marked the connection dirty, so the next command opened a new one | Read `Remote::download`; the reviewer's tap (connections 2) | The crate builds its read window on the first chunk and closes the handle when the download is dropped, so nothing is on the wire: `sink(Part::Start)` no longer sets `dirty`. `refusal_before_data_keeps_the_connection` failed first (1 new connection), asserts 0 for the refusals and for the next command. The rule in `docs/transfers.md`, `commands.md`, `known-limitations.md` and `help all` now says "once data was being read" |
| 8 | low | `read_only_share` asserts that every name in `upload/` starts with `rsmb-test-`, which fails when another test or an earlier run left something there | Read the test | It asserts that the names this test tried to create (`s.bin`, `l.bin`, `tree`, `newdir`, the temporary names of the two files) are absent |

### Checked and found correct

- The local commit through `LocalTemp` (lock, device and inode, link count, rename, the second hard
  link of a crashed commit).
- Names the server supplies: refused when they would leave the destination or have a character the
  local file system cannot hold.
- The completeness checks (a file that shrinks or grows during a download, a confirmed byte count
  on upload).
- The flags of `rm` and `rmdir` (no pattern, no recursion).
- Mutating commands are not run again after a lost connection, and the error says so.
- The local walk of `put -r` does not follow links.
- The deviations I1 to I18 of `docs/plans/stage-2.md` are justified by their evidence.

### Not checked

- Any server other than Samba (Windows, NetApp, other Samba versions): how they present links,
  delete a link, answer a create-only open, or let go of a handle after a dead connection.

## Stage 3 (2026-11-18)

| # | Severity | Finding | How validated | Outcome |
|---|---|---|---|---|
| 1 | medium | A directory is dropped when the first path to it fails: its id entered the visited set when it was queued, not when it was listed (`L/b/c/file.txt`, `L/zz-to-c -> b/c` deleted during the walk: `b/c` skipped as already listed, `zz-to-c` skipped as gone, `matched: 0`, exit 0). The same when the first path is denied | The orchestrator reproduced it; the reviewer twice; read `State::take`. The new unit test failed first (`a-to-c NotFound: listed [["L"], ["L", "b"], ["L", "zz-last"]]`) | A path to an id already queued waits in `State::dups`; when the listing of the path ahead of it fails (gone, denied, any other error) the next waiting path is queued in its place, and with none waiting the id is forgotten for a later path; the waiting paths still there at the end are the `links`. Order stays the queue's. `walk_lists_the_second_path_when_the_first_fails` (the failing path sorted before and after the real one; three error kinds). No integration case: it needs a deletion at a chosen moment, which has no observed event to wait on |
| 2 | low | `same_as` names the wrong directory: the start's seeded `..` id was printed as the textual parent (`tree /loops/self` printed `up/ [already listed as /loops]` though the real `/loops/up` is the share root; `/loops/up` in a walk of `/loops` was "already listed as /", a directory never listed) | The orchestrator reproduced it (`tree /loops/self`); read `end_report` and `tree_cmd`. `end_report_rules` (expectation changed) failed first (`already_listed`, `Some("/s")` against `outside`, `None`) | A path to the start's parent id is reported as `not entered: leads above the start, to a directory this walk does not list`, reason `outside`, `same_as` null, `[...]` on the tree line; `already_listed` only names a path that was listed, the start itself included. `end_report` lost its `start` parameter. `walk_link_loops` (extended with `find` and `tree /loops/self`), `docs/commands.md`, `docs/scripting.md`, `help all` |
| 3 | low | A directory whose name was decoded lossily (unpaired surrogate to U+FFFD) is entered with the altered name, the server says not found, and it is reported as `gone` with exit 0 | Read `parse_entries` and `end_report`; the new unit test failed first (`the altered name was sent to the server`) | The walk does not enter a name that holds U+FFFD: a failure of kind `invalid_name` with the path, exit 1 (also for `get -r`). A real U+FFFD in a name is refused alike (there is no separate marker; none was in the code to reuse). `walk_does_not_enter_a_name_it_could_not_decode`; `docs/known-limitations.md` |
| 4 | low | The case-collision rule of the cache feed only saw directories the walk listed: with `cs/Foo` denied and `cs/foo` listed, `foo`'s listing was stored under the shared key | Read `Cache::feed`; the extended unit test failed first (`a name that collides in the parent's listing is stored for neither`) | `Cache::feed` finds two subdirectory names with one folded key in the parent's listing and gives up both keys, whether or not both are listed; what is below a key given up is given up too. `cache_feed_collision_seen_in_the_parent` (the old `cache_feed_collision` stays) |
| 5 | low (test gap) | The 500,000-entry limit of `tree` was untested at the command level: removing the check after the walk left every test green | Read `tree_cmd`; mutation: with the check removed the new test fails (`called Result::unwrap_err() on an Ok value`) | The collecting and the drawing moved out of `tree_cmd` into `TreeNodes` (with its limit) and `draw_tree_output`; `tree_past_its_limit_draws_nothing` runs both with a limit of 5, in text and `--json`: kind `other`, message names `-L` and `find`, nothing written; at the limit it draws. No variable or flag in the product. `docs/testing.md` corrected |
| 6 | low (docs, messages) | (a) `commands.md` listed `find` under "may or may not have taken effect", the code says "the results above are incomplete"; (b) that tail was added even when `find` had printed nothing; (c) the help topic said `find` holds only the listings in flight; (d) `tree`'s table entry omitted error kind `other` | Read the four places; (b) with a unit test that failed first (`left: Some("; 'find /x' was not run again: the results above are incomplete") right: None`) | (a) `commands.md` says what the code does; (b) `not_run_again` adds the tail to `find` only when it printed (`lost_connection_tails`); (c) the topic names the queue, the visited ids with their paths and the cache feed up to its cap; (d) `other` in the table entry |

### Checked and found correct

- The whole share against the server's own `find`: 119,880 paths, the differences exactly the
  documented ones.
- 22 filter combinations equal to the server's `find`.
- Usage errors exit 2; SIGINT in the middle of `find` exits 130 with whole JSON lines; a closed
  stdout exits 1.
- `/loops` and `/links` print the same output on every run; the queue-order window is at most 16
  and no path to a deadlock was found.
- Eviction in the cache is no longer quadratic.
- `get -r` and `put -r` pass; the flags that take a value do not change the existing commands.

### Not checked

- Windows and other servers.
- The memory figures at 500,000 entries (synthetic).
- The progress line on a real terminal.
- The benchmark numbers.

## Stage 4 and final review (2026-11-18)

The review of the whole codebase after Stage 4 and the `shares` access column. The orchestrator
reproduced findings 3 and 4 and confirmed 1 and 11 in the code; the others are evident from the code
or the documents cited. Each behaviour finding was fixed test first.

| # | Severity | Finding | How validated | What was done | Guarding test |
|---|---|---|---|---|---|
| 1 | medium (test gap) | `sigterm_in_the_shell` waited for the Tab to finish before the signal, so SIGTERM with a Tab's request on the wire was untested although the comment and `docs/testing.md` said so | Read the test | The fast test now says what it covers (the prompt, with the text of a completed Tab). New slow test: at the slow endpoint the tap has seen the share list's first request, then SIGTERM: exit 130, terminal back to `icanon`/`echo`, history kept, and the Tab had not finished | `sigterm_in_a_tab_on_the_wire` (`tests/slow.rs`) |
| 2 | low to medium | Names and comments from the server reached the terminal unfiltered: a file named `a<U+009B>31mX` printed a raw C1 CSI, U+202E reversed the displayed name | Reproduced by the orchestrator | `safe` (`src/session.rs`) escapes C0, DEL, C1 and the bidi controls as `\u{..}`; `Out::text`, `diag`, the text branch of `error`, the progress line, the prompt, the overwrite question and the Tab candidate list use it; the manual uses `Out::doc` unchanged. `--json`, the names sent back and the local names of `get` keep the exact name | `hostile_names_are_escaped_in_text` failed first (`'\u{9b}' reached the terminal`), `safe_escapes_controls_and_bidi` |
| 3 | low | A non-UTF-8 argument with `--json` after it panicked, exit 101 (`std::env::args()` in `main`) | Reproduced | `args_os()`; the usual `usage` error, exit 2 | `startup_usage_errors` failed first (`left: Some(101) right: Some(2)`) |
| 4 | low | `rmdir`'s guard against removing the working directory compared case exactly: `cd cw`, `rmdir ../CW` succeeded and left the session in a missing directory | Reproduced | Components compared lower-cased, as the cache key does. `rm` and the other commands have no guard of this kind; the transfer code compares names of one walk, which agree | `rmdir_guard_folds_case` failed first (`rmdir ../../CW` answered `not_empty`: the guard had not fired) |
| 5 | low | `get` reused a temporary file it did not create (`create(true)` without `O_EXCL`): a planted `.rsmb-<hash>.part` with mode 666 became the final file | Read `LocalTemp::create` | A file already under the name is never adopted: created `O_EXCL|O_NOFOLLOW`; on `EEXIST` it is opened, and if another rsmb holds its lock the transfer is refused (a live transfer is never unlinked), otherwise the stale name is removed and the file made again (three rounds); after the lock the path must still name the inode made. The `nlink` check is gone (a new inode has one link) | `local_temp_is_never_adopted` failed first (`the planted inode became the file`); `two_processes_one_target`, `kill_9_leaves_only_temporaries` unchanged |
| 6 | low (hostile server) | `parse_entries` accepted overlapping entries (`next` shorter than the entry): memory growth; empty SUCCESS answers made the query loop, `read_pipe` and the fragment loop run for ever; the share-list limit was checked after collecting | Read the code | `next` must be 0 or reach the end of the name; an empty SUCCESS query answer, an empty pipe READ and an empty non-last fragment are malformed; the limit is enforced while collecting (`add_fragment`) | `overlapping_entries_are_malformed`, `empty_success_is_malformed`, `share_fragments_are_bounded` (all failed first) |
| 7 | low (`shares`) | (a) A connection lost during the access probe did not fail the command: the loss shows as an error in each answer of the compound, which `access_of` turned into "no answer", so the list was printed with `?` and the exit status was 0; (b) the cost on a low-credit server was not documented | (a) Reproduced by the new test: `shares` ok, every access null, no error. (b) Computed | (a) `probe_access` returns the loss, `Session::access` returns the answers so far with it, `shares` prints the whole list (`?` / `null` for what was not answered) and then fails with `connection_lost`, exit 1; nothing that has printed is run again (`run_parsed`). (b) `known-limitations.md`, `commands.md`, `performance.md`: ceil(shares ÷ (credits ÷ 2)) compounds one after another, 39 for 306 shares at 16 credits, about 7.8 s at 100 ms each way, computed not measured. No flag | `shares_lost_during_the_probe` failed first (`!r.ok`: the command had succeeded with all access null) |
| 8 | low (tests) | `tests/slow.rs` asserted wall times although `docs/testing.md` said those belong in `tests/serial.rs` | Read | In `tests/slow.rs` the only bounds are `NO_HANG` (10 s, at least five times what each takes alone) next to an exit code or kind that proves the behaviour; the precise latencies stay in `tests/serial.rs`. `docs/testing.md` says so | the changed tests |
| 9 | low (tests) | "One connection" checks that could not fail: `big_file`'s `put`, `two_thousand_small_files`, `small_files_counts_slow` asserted on start-up only | Read | `connections == 0` on every reply, `get` and `put` | the same |
| 10 | low (tests) | Assertions weaker than their names: the double-SIGINT round of `signals_mid_put` could assert nothing; `signal_in_the_small_file_phase` compared a count; `signal_mid_put_edge` only printed which connection did the delete; the 255-byte `-H` case; `fixture_names` ignored bash's status; `closed_stdout_is_not_a_panic` wrote less than a pipe buffer; `reconnect_after_idle_cut` slept | Read | `assert_named` (the temporaries on the server are exactly the ones `left_behind` names) in both signal tests and in every round; `signal_mid_put_edge` asserts 0 new connections each round (AGENTS.md's principle now holds); the message starts with `proxy `; `fixture_names` asserts success; stdout closed before rsmb starts; waits for `live_connections() == 0` | the same |
| 11 | low (infrastructure) | `test.sh` removed `upload/rsmb-test-*` without the lock; no `flock` had a wait limit | Confirmed in the code | The stale-directory step takes the exclusive lock; every `flock` in `test.sh`, `up.sh`, `check.sh` and both benchmarks waits at most 30 minutes (`-w 1800`) and then says which lock and why | `./test.sh` itself; the lock wait was tried by hand with `flock -E 99 -w` against a held lock (exit 99) |
| 12 | low (smbtap) | The rule "an encrypted answer with no encrypted request waiting answers the oldest plain frame" has only a synthetic unit test | Read | `docs/measuring.md` says there is no tshark cross-check and why (a capture cannot decrypt it), why rsmb's counts cannot be affected (the crate encrypts connection-wide, rsmb never has both kinds outstanding) and what kind of client would be miscounted | `encrypted_answer_to_a_plain_frame` (unchanged) |
| 13 | docs | `help put` "13 in 6 when the target exists" is only the refusal without `-f` (with `-f`: 7 in 3); `commands.md` "`ls` of a file is not supported yet"; a missing `-H` with `--json` never named `--host` (only clap's first line was kept); `AGENTS.md` "created in Stage 0; until then …"; the index entry of `stage-4.md` listed the unbuilt dial limit as design; `performance.md` 47.1 s and `huge/` 5.4 s against the bench logs 47.0 and 5.35 / 5.23; `known-limitations.md` 96.4 / 119.6 s against 121.1 s; `jq` missing from the host requirements; the help topic on Tab | Read, against the bench logs for the numbers | Each fixed; the sweep for "not yet", "will", "later stage" found nothing else that is stale (historical headings of the plans and `library-decision.md` stay) | `transfer_costs` (new row: `put -f` onto an existing small file, 7 in 3), `startup_usage_errors` (names `--host`), `table_is_complete`, `help_all_has_everything` |
| 14 | plainness | `in_dir` was an alias of `ready`; `medium_copy`, `SAMBA`, `MEDIUM_SHA`, `SLOW`, `FAST` were duplicated; `find_filters` kept always-`Some` options; `let _ = s;` | Read | Removed or moved to `tests/common/mod.rs` | the suites compile and pass |

Also, by the user's instruction: `README.md` has the install line (`cargo install --git <repository>
rsmb`; the package is named because the workspace has two). `cargo install --path .` into a scratch
root built and the installed binary printed `--help-all`; the installed directory was removed.

### The reviewer's walk through the requirements

Every requirement is met; the test that proves each is named (each name was checked to exist).

| Requirement | Proved by |
|---|---|
| One TCP connection | `one_connection_fast`, `one_connection_edge`, `one_connection_through_the_proxy`, `idle_sends_nothing` |
| `cd` at most 2 requests | `cd_is_one_compound`, `cd_slow`, `edge_navigation` |
| A second Tab costs 0 | `completion_costs`, `completion_costs_slow`, `edge_completion` |
| Cached listings | `file_ops_and_cache`, `walk_feeds_the_cache` |
| Bounded concurrency | the peaks in `big_file`, `two_thousand_small_files`, the walks, and `credit_balance` (in `edge_and_encrypted`) |
| Streaming | `big_file`, `over_4g` |
| No partial file under the final name | `cut_mid_get`, `cut_mid_put`, `kill_9_leaves_only_temporaries` |
| No silent overwrite | `overwrite_rules`, `overwrite_prompt` |
| `tree` and `find` | `walk_depth`, `find_filters`, `walk_past_denied`, `walk_link_loops` |
| SOCKS5 proxy | `socks5_handshake_bytes`, `no_way_around_the_proxy`, `proxy_failures`, `cut_through_the_proxy`, the proxied run of `tests/fast` with the log floor |
| Never prompts when not interactive | `no_password_no_prompt`, `json_never_starts_the_shell` |
| `help all` complete | `examples_run`, `table_is_complete` |

### Checked and found correct

- One dial path, through `open_tcp`; no proxy taken from the environment.
- No credential leaks (the password never appears in output, with or without a proxy).
- The access-mask bits against MS-SMB2.
- No test is skipped under `RSMB_TEST_VIA`.
- Every example of `help all` runs against the test server.
- The README's numbers agree with the bench logs.
- Deviations V1 to V12 of the Stage 4 plan are justified.

### Not checked

- Windows and other servers, including the TREE_CONNECT + TREE_DISCONNECT compound and
  `MaximalAccess` there.
- `shares` on a low-credit server at latency (the 7.8 s is computed).
- A raw ESC from a real server (the escaping is tested with the C1 and bidi names Samba accepts).
- Proxies other than microsocks.
- The 500,000-entry memory figures.

## Stage 5 (2026-11-18)

The review of Kerberos login from a ticket cache (`-k`), direct and through `--proxy`, with the
loopback KDC relay. The reviewer reproduced findings 1, 2 and 3; the orchestrator reproduced
finding 4 and confirmed 1 and 5 in the code. Each behaviour finding was fixed with a test that
failed without the fix (how each was checked is in the last column).

| # | Severity | Finding | How validated | What was done | Guarding test |
|---|---|---|---|---|---|
| 1 | medium | `known-limitations.md` and `architecture.md` said the only socket of a TGT-only login is the relay's on 127.0.0.1. The crate's own UDP socket for the KDC binds `0.0.0.0:0` and takes the first datagram from any sender; and the relay forwarded a datagram from any local sender to the KDC (through the user's proxy, with its credentials) and sent the answer back to that sender | The reviewer sent one datagram `30 00` from another host to that port during a TGT-only login: the login ended after 1566 ms, exit 3, kind `auth`, `the ticket in the cache or the KDC's answer could not be used (Invalid data: … got 0x30)`. The crate: `smb2-0.43.0/src/auth/kerberos/kdc.rs:91`, `UdpSocket::bind(0.0.0.0:0)`. The relay: read | The two documents (and the `smb2` list of `known-limitations.md`, `stage-5.md` K12 and V10) now say it: during a login with only a TGT the crate's socket listens on every interface on an ephemeral port, a stray or forged datagram can end that login with an `auth` error, it cannot make a login succeed (an answer is only of use if it decrypts with the key from the cache), rsmb cannot change it, a service ticket in the cache opens none. `kdc_relay` serves only the first sender it hears from and ignores every other address | `kdc_relay_serves_the_first_sender_only` fails with the check taken out (`the stranger got an answer`); `kdc_relay_carries_the_exchange` now sends from one socket, as the crate does |
| 2 | medium | A `KRB5CCNAME` that is set but not UTF-8 was taken as unset (`std::env::var(..).ok()`), the default `/tmp/krb5cc_<uid>` was read, and the message said the variable was not set; the same for `KRB5_CONFIG` (then `/etc/krb5.conf`) | The reviewer's run: a valid cache whose file name has byte `\xe9` gave exit 3, `ticket cache /tmp/krb5cc_1000: No such file or directory … (KRB5CCNAME is not set, so the default file … was used …)`. The code at `session.rs` 677 and 1042 | `var_os`; `ccache_path` and `krb5_files` take `&OsStr` and work on the bytes (the `FILE:` prefix, the refusal of `DIR:`, `KCM:`, `KEYRING:` and other types, the `:` split are unchanged); `commands.md` says it | `kerberos_cache_odd_files` (a cache at a non-UTF-8 path logs in: failed first with the reviewer's message), `ccache_paths` and `kdc_files` (the non-UTF-8 cases) |
| 3 | medium | The end of a ticket was described wrongly in `help all`, `scripting.md` and `known-limitations.md` ("the next command fails with `connection_lost`, and the one after it logs in again"); `commands.md` and `architecture.md` were right | The reviewer's three runs with 130 s tickets, each command 150 s after they were made: a fresh cache in place, `ls /`: ok, no error (a reading command is run again after the reconnect); the old cache kept, `ls /`: `auth`, `reconnect failed: Kerberos: the ticket for cifs/… expired at …; run kinit`; a fresh cache, `mkdir`: `connection_lost`, `'mkdir …' was not run again` | The three texts say that: a reading command is run again on a new login that reads the cache again (it works with new tickets, with the old ones it is `auth`), a command that changes something fails with `connection_lost` and is not run again, and the next command logs in again | `help_all_has_everything` checks the new words and that the old sentence is gone |
| 4 | low | A ticket cache that is not a regular file hung rsmb at start-up and SIGINT and SIGTERM did not end it: the synchronous `open` of a FIFO blocks inside the future `interruptible` drives, so its signal branch is never polled. The same for the `KRB5_CONFIG` files read in `kdc_for` | Reproduced by the orchestrator: `mkfifo -m 600 fifo; KRB5_CONFIG=/dev/null KRB5CCNAME=$PWD/fifo timeout -s INT -k 3 2 rsmb -k -H 127.0.0.1 -P 1 --json -N -C pwd` gave `Killed`, exit 137 after 5001 ms (the `kdc_for` case from reading the code) | `open_regular`: the file is opened with `O_NONBLOCK` (a FIFO does not wait for a writer) and refused unless `fstat` of the opened file says regular, so nothing is read and there is no window between a check and the open. The cache: `auth`, `Kerberos: ticket cache <path>: not a regular file`; a `KRB5_CONFIG` file that is not regular counts as one that cannot be read (absent). Now exit 3 after 2 ms | `non_regular_files_are_not_read` (unit, on a thread with a 5 s bound; fails with the flag removed), `kerberos_cache_odd_files` (a FIFO and a directory: `auth`, `not a regular file`, within 5 s; without the fix the run ended only at the harness's 60 s limit) |
| 5 | low (test) | S71 counted the KDC's whole log, not new lines: `assert_eq!(kdc_requests("kproxy", "10.213.45.1"), 0)`; one direct `kproxy` request at any time made it fail until the container was recreated | The code | The count is taken before and asserted not to have grown (three places) | `kerberos_kdc_is_asked_through_the_proxy` |

### Changed at the user's request during the review: the help

`--help` printed one line of up to 286 characters per flag and the text manual had one line of 2608
characters. Now the doc comment of each flag in `Args` (`src/main.rs`) has a first paragraph, which
is the short `--help` line (within 80 columns), and a second paragraph with the details that only
the manual shows; `wrap` and `Out::doc` (`src/commands.rs`) wrap the text manual to 80 columns.
`--json` is unchanged (the manual's objects keep their text whole). Tests: `manual_fits_80_columns`
(no line of the text manual is longer than 80 columns unless it is one word, `--json` is not
wrapped, and `wrap` keeps the words and the hanging indent) and the extended `args_are_sound`
(`--help` is within 80 columns and does not carry the details, and every flag is in the manual).
The tests that look for a phrase in the manual compare with whitespace squashed
(`commands::tests::squash`).

### Checked and found correct

- The relay still serves the crate's own exchange: the crate binds one socket per KDC call and
  makes one call for a TGT-only login (`tgs_exchange`; `authenticator.rs`), retrying on the same
  socket, so "the first sender" is the crate's socket.
