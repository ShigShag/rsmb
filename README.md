# rsmb

`rsmb` is a fast SMB shell client written in Rust. It is built around network round trips, so it
stays quick on high-latency links where other clients crawl: one TCP connection, one tree connect
per share, a single compound open/close for `cd`, a directory listing in one compound, cached
listings for Tab completion, nothing sent between commands, and transfers and walks that keep up
to 16 requests in flight.

## Features

- **Login**: user/password/domain, guest, anonymous, and Kerberos from the ticket cache (`-k`).
- **Interactive shell**: history, Tab completion of remote and local paths (free the second time),
  progress lines, an overwrite prompt, and Ctrl-C that interrupts a command but keeps the session.
- **Scriptable**: commands via `-C`, `-S` or stdin, `--json` output (one object per line, a `done`
  after every command), fixed exit codes, never a prompt. `rsmb --help-all` prints the complete
  manual without a server, so an AI agent can learn the tool from the binary alone.
- **Commands**: `shares` (with the share-level access the server reports), `use`, `cd`, `pwd`,
  `ls`, `access` (`-e`: which commands the server would allow on a path or on every entry of a
  directory, nothing tried), `tree` (`-L`), `find` (`-name`, `-iname`, `-type`, `-size`, `-maxdepth`), `get` and `put`
  (files and `-r` directories, streaming, never a partial file under the final name, no overwrite
  without `-f`), `cat`, `mkdir` (`-p`), `rm`, `rmdir`, `lcd`, `lpwd`, `lls`, `refresh`, `help`,
  `exit`.
- **SOCKS5 proxy**: `--proxy socks5://[user:pass@]host:port`. Every connection goes through it
  (the KDC included), the server name is resolved by the proxy, and there is no fallback to a
  direct connection.
- **No system libraries**: pure Rust, no Kerberos library needed.

Tested against Samba 4.23. Kerberos login has also been checked by hand against a Windows AD
domain controller; general Windows server support is not covered by the test suite.

## Install

```
cargo install --git https://github.com/ShigShag/rsmb rsmb
```

The repository holds two packages (`rsmb` and the test tool `smbtap`), so the package is named;
the binary is `rsmb`. It needs a recent stable Rust (edition 2024, 1.89 or newer). Add `--locked`
to build with the committed `Cargo.lock`, the versions the tests ran with.

## Usage

```
# interactive shell; asks for the password
rsmb -H fs1.example.com -u alice -d CORP

# non-interactive: each -C is one command
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C 'ls /reports'

# commands from stdin, output as one JSON object per line (for scripts and AI agents)
printf 'use docs\ncd reports\nls\n' | rsmb -H fs1.example.com -u alice -p 'secret' -d CORP --json

# download a directory recursively into the current local directory
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C 'get -r /reports/2026'

# upload a local file into the remote directory "upload"
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C 'cd upload' -C 'put notes.txt'

# directory tree, two levels deep
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C 'tree -L 2 /reports'

# may I list, put into, make a directory in, remove "upload"? and the same for every entry of /reports
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C 'access upload' -C 'access -e /reports'

# find files by name pattern (quote it so the local shell does not expand it)
rsmb -H fs1.example.com -u alice -p 'secret' -d CORP -C 'use docs' -C "find / -name '*.pdf'"

# through a SOCKS5 proxy; the proxy resolves the server name
rsmb --proxy socks5://user:pass@127.0.0.1:1080 -H fs1.internal -u alice -p 'secret' -d CORP -C 'shares'

# Kerberos from the ticket cache (KRB5CCNAME, else /tmp/krb5cc_<uid>): no password; the
# ticket-granting ticket must be forwardable (kinit -f), or the service ticket in the cache
kinit -f alice@CORP.EXAMPLE
rsmb -k -H fs1.corp.example -C 'use docs' -C 'ls'

# the whole manual, no server needed
rsmb --help-all
```

`-k` means Kerberos; the option to continue after a failed command is `--keep-going` (long form
only).

[`docs/commands.md`](docs/commands.md) has every command and flag, including the proxy's and
Kerberos' rules and messages; [`docs/scripting.md`](docs/scripting.md) the non-interactive
contract and the `--json` objects; [`docs/transfers.md`](docs/transfers.md) every rule a transfer
keeps.

## Performance

Compared with smbclient-ng 3.1.0 against Samba at 100 ms latency each way (200 ms round trip).
[`docs/performance.md`](docs/performance.md) has the full tables and the method.

| At 100 ms each way | smbclient-ng 3.1.0 | rsmb | Faster by |
|---|---|---|---|
| `cd` | 1,609 ms; 6 requests in 6 round trips; 2 new TCP connections | 201 ms; 2 requests in 1 round trip; none | 8× |
| `ls`, 5 entries | 1,208 ms; 5 round trips | 201 ms; 5 requests in 1 round trip | 6× |
| `ls`, 10,000 entries | 4,559 ms; 17 round trips | 424 ms (1.5 s as the first listing of a connection); 1 round trip | 11× (3×) |
| `ls`, 100,000 entries | 37.6 s; 139 round trips | 3.2 s; 1 round trip | 12× |
| `tree`, 85 directories | 69.3 s; 342 round trips, one after another | 1.61 s; 340 requests in 47 flights | 43× |
| download, 50 MiB | 16.7 s | 5.7 s | 2.9× |
| upload, 50 MiB | 66.6 s | 5.7 s | 12× |

Through a SOCKS5 proxy, `cd` and `ls` cost the same; walks and many small files take 1.1 to 1.3×
the direct time.

rsmb is built on the [`smb2`](https://crates.io/crates/smb2) crate (pinned exactly), with its own
one-compound listing, `cd` check, share list and Kerberos service-ticket request on top.
[`docs/library-decision.md`](docs/library-decision.md) explains the choice.

## Development

The test environment runs entirely in Docker containers; nothing needs root or changes the host.
It needs Docker with Compose, bash, python3, `smbclient`, `iperf3`, `jq`, and for the full run
`tshark` and `dumpcap` usable without root. The containers are three Samba servers, two latency
routers, a SOCKS5 proxy in front of a server the host cannot reach, and a Samba AD domain
controller for Kerberos.

```
./testenv/up.sh      # build and start (about 150 s the first time, seconds afterwards)
./testenv/check.sh   # verify the environment with independent tools
./test.sh            # full test suite (about 9 min)
./test.sh fast       # tests without injected latency (about 1.5 min)
./testenv/down.sh    # stop and remove containers and networks
```

Inside the environment Samba is at 10.213.45.10 (direct) and 10.213.45.30 (100 ms each way), user
`TESTDOM\tester`, password `testpass`:

```
cargo build --release
target/release/rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM
```

## Documentation

- [`docs/commands.md`](docs/commands.md), [`docs/scripting.md`](docs/scripting.md): using it.
- [`docs/transfers.md`](docs/transfers.md): how `get` and `put` keep data safe.
- [`docs/known-limitations.md`](docs/known-limitations.md): what does not work or was not tested.
- [`docs/performance.md`](docs/performance.md): measurements and method.
- [`docs/architecture.md`](docs/architecture.md): how rsmb is built.
- [`docs/development.md`](docs/development.md), [`docs/testing.md`](docs/testing.md): working on it.
- [`docs/test-environment.md`](docs/test-environment.md): the containers and fixtures.
- [`docs/measuring.md`](docs/measuring.md): how requests and round trips are counted.
