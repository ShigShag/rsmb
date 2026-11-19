# rsmb

`rsmb` is a fast SMB shell client in Rust, a replacement for the user's use of smbclient-ng.
It is designed around network round trips: one TCP connection, one tree connect per share, a
single compound open/close for `cd`, a directory listing in one compound, cached directory
listings for Tab completion, nothing sent between commands, transfers and walks that keep up to
16 requests in flight.

**Status: all six stages are done.** Login (user/password/domain, guest, anonymous, and
Kerberos from the ticket cache with `-k`, direct and through the proxy, the KDC included), an
interactive shell with history and completion (remote and local paths), a non-interactive mode
with `--json` and fixed exit codes, the commands `shares` (with the share-level access the
server reports for each share, one more round trip), `use`, `cd`, `pwd`, `ls`, `access`
(`-e`: which commands the server would allow on a path or on every entry of a directory, asked
with one open per path and nothing tried; `cd` and `ls` unchanged), `tree`
(`-L`), `find` (`-name`, `-iname`, `-type`, `-size`, `-maxdepth`), `get` and `put` (single files
and `-r` directories, streaming, never a partial file under the final name, no overwrite without
`-f`), `cat`, `mkdir` (`-p`), `rm`, `rmdir`, `lcd`, `lpwd`, `lls`, `refresh`, `help` (`help
all`), `exit`, and `--proxy socks5://[user:pass@]host:port`, through which every connection goes,
the server's name resolved by the proxy.

**Tested against Samba 4.23.8 only; no claim is made for Windows servers.**

Three uses, each checked:

- **Interactively** by a person: history, Tab completion that costs nothing the second time,
  progress lines, the overwrite question, Ctrl-C that interrupts a command and keeps the session.
- **Non-interactively by a program or an AI**: `-C`, `-S` or stdin, `--json` (one object per
  line, a `done` after every command), fixed exit codes, never a prompt; `rsmb --help-all` is the
  complete manual and needs no server.
- **Developed by AI end to end**: `./test.sh` brings up the containers, checks them and runs every
  test without root and without a human step.

**`-k` is Kerberos now; it used to be the short form of `--keep-going`**, which keeps only its long
form. An old command line with `-k` and `-p` or `--no-pass` is now refused (exit 2); one without
either (an anonymous run such as `rsmb -H h -N -k -C ls`, or one that waited for the password
prompt) is now a Kerberos login as the principal of the default ticket cache. Write
`--keep-going`.

## Install

```
cargo install --git https://github.com/ShigShag/rsmb rsmb
```

The repository holds two packages (`rsmb` and the test tool `smbtap`), so the package is named; the
binary is `rsmb`. It needs a recent stable Rust (edition 2024; `File::try_lock` is used, stable
since Rust 1.89; built and tested with 1.99) and no system library (no Kerberos library either).
`cargo install --locked --git https://github.com/ShigShag/rsmb rsmb` builds with the committed
`Cargo.lock`, which is the set of versions the tests ran with.

## Using it

```
# build the binary (target/release/rsmb)
cargo build --release

# interactive shell; asks for the password
target/release/rsmb -H 10.213.45.10 -u tester -d TESTDOM

# non-interactive: each -C is one command; list the directory /sizes on share "share"
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'ls /sizes'

# commands from stdin, output as one JSON object per line (for scripts and AI)
printf 'use share\ncd tree\nls\n' | target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM --json

# download a directory recursively into the current local directory
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'get -r /tree/d0'

# upload a local file into the remote directory "upload"
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'cd upload' -C 'put notes.txt'

# directory tree, two levels deep
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'tree -L 2 /tree'

# may I list, put into, make a directory in, remove upload? and the same for every entry of /tree
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'access upload' -C 'access -e /tree'

# find files by name pattern (quote it so the local shell does not expand it)
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C "find /special -name '*.txt'"

# through the SOCKS5 proxy (this listener wants user:password); the proxy resolves the server name
target/release/rsmb --proxy socks5://tester:testpass@10.213.45.20:1081 -H fileserver.rsmb.test \
    -u tester -p testpass -d TESTDOM -C 'use share' -C 'ls /sizes'

# Kerberos from the ticket cache (KRB5CCNAME, else /tmp/krb5cc_<uid>): no password; the
# ticket-granting ticket must be forwardable (kinit -f), or the service ticket in the cache
kinit -f alice@CORP.EXAMPLE
target/release/rsmb -k -H fs1.corp.example -N -C 'use docs' -C 'ls'

# the whole manual, no server needed
target/release/rsmb --help-all
```

[`docs/commands.md`](docs/commands.md) has every command and flag (and the proxy's and
Kerberos' rules and messages), [`docs/scripting.md`](docs/scripting.md) the non-interactive contract and the `--json`
objects, [`docs/transfers.md`](docs/transfers.md) every rule a transfer keeps.

## Against smbclient-ng

Measured on 2026-11-18 with the final code (commit `93712f1` plus the Stage 4 changes), rsmb
0.1.0 against smbclient-ng 3.1.0, Samba 4.23.8, at 100 ms each way (200 ms round trip), by
`testenv/bench-smbclientng.sh` and `testenv/bench-rsmb.sh` (all modes); the proxy row is the
measurement made while planning Stage 4. [`docs/performance.md`](docs/performance.md) has the whole
table, every run's output and the method.

| At 100 ms each way | smbclient-ng 3.1.0 | rsmb | Faster by |
|---|---|---|---|
| `cd` | 1,609 ms; 6 requests in 6 round trips; 2 new TCP connections | 201 ms; 2 requests in 1 round trip; none | 8× |
| `ls`, 5 entries | 1,208 ms; 5 round trips | 201 ms; 5 requests in 1 round trip | 6× |
| `ls`, 10,000 entries | 4,559 ms; 17 round trips | 424 ms (1.5 s as the first listing of a connection); 1 round trip | 11× (3×) |
| `ls`, 100,000 entries | 37.6 s; 139 round trips | 3.2 s; 1 round trip | 12× |
| `tree`, 85 directories | 69.3 s; 342 round trips, one after another | 1.61 s; 340 requests in 47 flights | 43× |
| download, 50 MiB | 16.7 s (median of 3) | 5.7 s (median of 3) | 2.9× |
| upload, 50 MiB | 66.6 s (median of 3) | 5.7 s (median of 3) | 12× |
| the same through a SOCKS5 proxy (measured while planning, 2026-11-18, one session each) | no proxy option | `cd` and `ls` unchanged (201 ms); walks and small files 1.1 to 1.3×, a 50 MiB download 1.6× the direct time with microsocks | |

## Library: `smb2` 0.43.0

Two crates were tried in Stage 0 with a decision rule fixed before anything was measured
([`docs/library-decision.md`](docs/library-decision.md) has every gate, number and reason).
**`smb2` 0.43.0** passes seven of the eight hard gates as written and the eighth (gate 2, the
16-wide walk "close to levels × listing round trips") only as reinterpreted after measuring
("requests overlap, bounded by the width"), which neither crate met as written; it takes a
caller's TCP stream, which is how the SOCKS5 proxy works; it has typed errors, 72 dependencies
against 209, and spike code that compiled unchanged across 0.40–0.43. **`smb` (smb-rs)** failed as
published: it needs an unreleased git pin to list large directories and 30 patched lines for
downloads, guest login and the share list; anonymous login still failed inside an exactly pinned
dependency, and one IOCTL hung on the 16-credit server. The choice is a judgement on those points,
not an arithmetic result. `smb2` is pinned exactly (`=0.43.0`), because breaking releases come
every few days; rsmb adds its own one-compound listing, `cd` check and share list on its public
API.

Key numbers of the Stage 0 spike (`docs/library-decision.md`, "Comparison"), at 100 ms each way
(200 ms round trip) unless marked fast:

| | smbclient-ng 3.1.0 | `smb2` 0.43.0 | `smb` (smb-rs), git pin + 30 patched lines |
|---|---|---|---|
| directory check (`cd`) | 2 TCP connects + 6 flights, 1,610 ms | 1 flight (our compound), 201.1 ms | 1 flight + a CLOSE in the background, 201.0 ms |
| `ls`, small | 5 flights, 1,208 ms | 4 flights, 803.0 ms | 4 flights, 803.9 ms |
| `ls`, 10,000 entries | 17 flights, 4,535 ms | 4 flights, 901.1 ms warm, 2,103 ms fresh | 4 flights, 1,078.8 ms |
| `ls`, 100,000 entries | 139 flights, 37,320 ms | 5 flights, 2,337 ms warm, 3,974 ms fresh | 5 flights, 2,141.4 ms |
| share list (306 on the 16-credit server) | | all 306 with our srvsvc call (the crate's own drops hidden ones) | with its own NDR32 code |
| 1.1 GiB download, slow; fast | | 17.4 MB/s (raw TCP 16.9); 878–1,054 MB/s | the pin times out after 120 s; patched 19.9 MB/s; 1,459–1,518 MB/s |
| takes a caller's TCP stream (SOCKS5) | | yes | yes |
| dependencies; patches | | 72 crates; none | 209 crates; 30 lines, anonymous login still failing |

The spike's times were taken differently from smbclient-ng's (the plan's (10 − 0) ÷ 10 method was
not used for the crates), so compare the flights; a fresh connection is slow in its first
listings (TCP slow start fits the numbers).

## Test environment and tests

Needs Docker with Compose, bash, python3, `smbclient`, `iperf3`, `jq`, and for the full run
`tshark` and `dumpcap` usable without root. No root and no host changes: everything privileged
runs inside containers, seven of them (Samba ×3, two latency routers, a SOCKS5 proxy in front of a
server the host cannot reach, and a Samba AD domain controller with its own KDC for Kerberos).

```
./testenv/up.sh      # build and start: about 150 s from nothing (6 GB image and the DC's 219 MB), seconds with the images built
./testenv/check.sh   # verify the environment with independent tools (about 30 s)
./test.sh            # everything: about 506 s counted of the 600 s budget (about 9 min)
./test.sh fast       # up to tests/fast, direct and through the proxy: about 77 s of 150
./testenv/down.sh    # stop and remove containers and networks
```

Samba is at 10.213.45.10 (direct) and 10.213.45.30 (through a router adding 100 ms each way),
user `TESTDOM\tester`, password `testpass`; the proxy at 10.213.45.20 (port 1080 without a login,
1081 with `tester`/`testpass`), the private server behind it as `fileserver.rsmb.test`; the
Kerberos server at 10.213.45.12 (realm `RSMB.TEST`, `dc.rsmb.test` through the proxy), whose
ticket caches the tests make with `docker exec rsmb-samba-dc mkcache -f tester …`.

## Documentation

- [`docs/architecture.md`](docs/architecture.md): how rsmb is built.
- [`docs/commands.md`](docs/commands.md), [`docs/scripting.md`](docs/scripting.md): using it.
- [`docs/transfers.md`](docs/transfers.md): how `get` and `put` keep data safe, rule by rule.
- [`docs/performance.md`](docs/performance.md): the measured comparison with smbclient-ng, every
  target with its value, the proxy's cost.
- [`docs/known-limitations.md`](docs/known-limitations.md): what does not work or was not tested.
- [`docs/reviews.md`](docs/reviews.md): the review of each stage, what was found and done.
- [`docs/development.md`](docs/development.md), [`docs/testing.md`](docs/testing.md): working on it.
- [`docs/brief.md`](docs/brief.md): the requirements; [`docs/plans/`](docs/plans/): the plans.
- [`docs/library-decision.md`](docs/library-decision.md): the spikes, the gates, the choice.
- [`docs/test-environment.md`](docs/test-environment.md): the containers and fixtures.
- [`docs/measuring.md`](docs/measuring.md): how requests and round trips are counted, and the
  smbclient-ng baseline.
- [`AGENTS.md`](AGENTS.md): the index of everything, and the working rules.
