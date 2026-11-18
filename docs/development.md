# Development

How to build, test and extend rsmb. The rules for agents are in [`../AGENTS.md`](../AGENTS.md);
the design in [`architecture.md`](architecture.md); the tests in [`testing.md`](testing.md).

## Build and run

```
cargo build --release                       # target/release/rsmb
./testenv/up.sh                             # the test servers (docs/test-environment.md)
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM
target/release/rsmb -H 10.213.45.30 -u tester -p testpass -d TESTDOM   # 100 ms each way
target/release/rsmb --proxy socks5://10.213.45.20:1080 -H fileserver.rsmb.test \
    -u tester -p testpass -d TESTDOM                                     # through the proxy
target/release/rsmb --help-all              # the manual, no server needed
```

A Kerberos login by hand, with a cache only you can read and without the host's Kerberos
configuration (the service ticket is in the cache, so no KDC is asked):

```
(umask 077; docker exec rsmb-samba-dc mkcache -f tester cifs/10.213.45.12 > "$XDG_RUNTIME_DIR/rsmb-k.cc")
KRB5_CONFIG=/dev/null KRB5CCNAME="$XDG_RUNTIME_DIR/rsmb-k.cc" target/release/rsmb -k -H 10.213.45.12
```

With a ticket-granting ticket only (`mkcache -f tester` without a service name) the KDC must be
named, because the host cannot resolve the realm's name `rsmb.test`: a file with `[realms]`,
`RSMB.TEST = {`, `kdc = 10.213.45.12`, `}` in `KRB5_CONFIG`. Through the proxy
(`--proxy socks5://10.213.45.20:1080 -H dc.rsmb.test`) `KRB5_CONFIG=/dev/null` works: the proxy
resolves `rsmb.test`.

The workspace has two members: the root package `rsmb` and `smbtap` (the counting tap, a
dev-dependency of the tests). `spikes/` is excluded and has its own lock files.

## Test

```
./test.sh fast     # environment, check.sh, fmt, clippy, build, unit tests, tests/fast direct
                   # and again through the proxy                       (budget 150 s, about 60 s)
./test.sh          # the same plus tests/slow, tests/serial, smbtap against tshark
                   #                                                    (budget 600 s, about 8 min)
```

One test: `cargo test --bin rsmb listing_plan` (a unit test, dev profile) or
`flock -s /tmp/rsmb-env.lock cargo test --release --test fast cd_answers` (an integration test;
take the lock as `./test.sh` does, exclusive `-x` for `tests/serial`, with `--test-threads=1`).
Add `-- --nocapture` to see what a timing test prints. One test of `tests/fast.rs` through the
proxy: `RSMB_TEST_VIA=socks5 flock -s /tmp/rsmb-env.lock cargo test --release --test fast
cd_answers`.

Reading a failure: every count assertion names the command and the endpoint (or the test that
only has one), the expected and the actual requests and flights, the tap's per-command counts and
the request log (connection, flight, command, position in the compound); `Reply::expect`,
`expect_stats` and `common::context` print them. A pty test prints the whole terminal transcript,
escaped. A timeout in the harness kills the child and prints what was received so far and its
stderr.

## Where things go

- **All use of `smb2` is in `src/session.rs`**: the transport halves, `execute` and
  `execute_compound`, the message types, `rpc`. A crate upgrade touches this file only.
- A new **command**: a `CmdId` variant, an entry in `COMMANDS` (syntax, summary, details, flags,
  argument kinds, required count, at least one example that runs against the test server, and
  `rerun`). A **flag with a value** is declared with a space: `("-L <n>", "help")`; the command
  reads it with `opt(a, "-L")` from `Args::opts` (`split_args` takes the next token as the value
  whatever it is; a missing value and a flag given twice are usage errors in `parse`). An example
  runs from the root of `share` and changes nothing; the examples of a command
  that changes the server (`get`, `put`, `mkdir`, `rm`, `rmdir`) are written for the scratch
  context that `help all` states (a remote working directory holding `old.txt` and `olddir/`, a
  local one holding `notes.txt` and `photos/`), and `examples_run` builds exactly that. a function and its `match` arm in `dispatch`
  (`src/commands.rs`), a test in `tests/`, and a section ``## `name` `` in `docs/commands.md`
  with the syntax and every example. `table_is_complete`, `help_all_has_everything`,
  `docs_match_the_table` and `examples_run` fail until all of that is there.
- **Printing names.** `Out::text` escapes control and bidi characters (`safe`): use it for every
  line that can hold a name or a message. `Out::doc` writes our own fixed text unescaped (the
  manual and the help, which have several lines), wrapped at spaces to 80 columns (`wrap`). A new printer that writes to stdout or stderr
  by another way calls `safe` on what it prints; `--json` and `cat` never do.
- A method of `Session` that sends starts with `self.tree(..)` (which reconnects a dead
  connection) and sets `in_flight` around its requests, so an interrupt replaces the connection.
- A request a transfer, a walk or a file operation needs is a method of **`Remote`** in
  `src/session.rs` (every path through `crate_path`, every error through `classify`, no `smb2`
  type in its signature). Every command that uses one has this shape, and only this shape:
  `let remote = sess.remote().await?; let r = work(&remote, …).await; sess.finish(remote, r).await`.
  Anything that runs after the command's future could be dropped (a spawned blocking task) holds a
  `Remote` clone or an `Alive`, so the cleanup waits for it.
- `src/transfer.rs`: `get`, `put`, `cat` and everything local about them (temporary files, targets,
  the local walk, progress). `src/walk.rs`: the traversal and the pure parts of `tree` and `find`;
  a new walking command is an `each` callback of `run_walk` in `src/commands.rs` (or an `on_dir`
  of `walk::walk`), not a second loop. A new **`find` test** (`-newer`, `-mindepth`) is a field of
  `walk::Filter` with its arm in `Filter::parse` and its condition in `Filter::matches`, a case in
  the unit test `filter_rules`, a row in `find_filters` (F42) compared with the server's own
  `find` through `server_find` (BusyBox: check by hand first that it has the predicate), and a line
  in `find`'s syntax, flags and `docs/commands.md` section.
- A new test in **`tests/fast.rs`** names its endpoint as `fast()` and uses its methods
  (`route`, `args`, `host`, `container`, `tap`), never an address, a prompt with an address or
  the name `rsmb-samba`: the file runs twice, the second time through the proxy. A Kerberos test
  names `dc()` and its `spn(tapped)` the same way; a direct TGT-only run names the KDC with
  `krb5_conf(dir, "10.213.45.12")`, and a tapped Kerberos session through the proxy holds the
  service ticket (`docs/testing.md`, "The harness"). A test that
  passes its own `--proxy` builds its whole command line and does not add to `route()`
  (`docs/testing.md`).
- A new **start-up flag** goes into `Args` in `src/main.rs` with a doc comment; the manual renders
  it from clap, and `docs_match_the_table` wants its long name in `docs/commands.md`. The doc
  comment's first paragraph is what `--help` shows: one short line, so that the line stays within
  80 columns (`args_are_sound` checks it); details go into a second paragraph, which only the
  manual shows, joined to the first by "; ".
- A new **JSON object type** goes into `JSON_OBJECTS` in `src/commands.rs`;
  `every_json_line_has_a_type` fails for a type that is printed but not listed.

## The `smb2` pin

`smb2 = "=0.43.0"`, `Cargo.lock` committed. No upgrade inside a stage. An upgrade is its own
change: bump the pin, run `spikes/smb2/run.sh` and `./test.sh`, record the result and the reason
in `docs/library-decision.md`. A defect we hit that upstream has fixed is a reason; a newer
version is not. The crate source is in the cargo registry (`~/.cargo/registry/src/*/smb2-0.43.0`)
for reading.

## Dependencies

`smb2`, `tokio`, `async-trait` (the crate's transport traits), `tokio-socks`, `libc` (the terminal's
attributes, restored on SIGTERM in the shell; `O_NOFOLLOW` for the local temporary file), `clap` (without
default features), `rustyline` (with file history only), `serde_json`, `rpassword`; dev:
`smbtap`. Stages 2 to 5 added none (`tokio-socks` does the SOCKS5 handshake since Stage 1; the
master plan's `indicatif` was never added): no progress-bar crate (one line of our own), no hashing
crate (FNV-1a in ten lines), tokio's `fs` feature not enabled (local I/O is `std::fs` on the main
thread or in `spawn_blocking`), no glob crate (`Glob` in `src/walk.rs`, about 150 lines with its
classes and folding, compared with the server's `find` on 60 patterns). No PTY, time, temp-file, URL or regex crate: the tests use `testenv/ptyrun.py`,
`env!("CARGO_TARGET_TMPDIR")` and a few lines of their own. No Kerberos crate and no system
Kerberos library: `smb2` parses the cache and does the Kerberos exchange; rsmb adds the cache's
path, its owner (`libc::getuid`), the expiry check, a small `krb5.conf` reader and the KDC relay
(tokio's UDP socket and `open_tcp`). A new dependency needs a line of justification in the stage
plan.

## Throwaway measurements

Programs written to measure something once live under `spikes/smb2/target/` (ignored by git)
with their own `Cargo.toml` and `[workspace]`, or in a scratch directory outside the repository.
They are not committed; what they found is written into the plan or the document it informs.
