# Test environment

Seven containers in the Docker Compose project `rsmb`: three Samba servers, two latency routers, a
SOCKS5 proxy and a Samba Active Directory domain controller with its own KDC (Kerberos). Nothing needs root on the host and nothing changes the host outside this project's
containers, networks and images. Built from `testenv/`.

```
./testenv/up.sh          # build where needed, start, wait until every service is healthy
./testenv/check.sh       # prove the environment is what the tests assume (about 30 s)
./testenv/down.sh        # remove containers and networks; images stay
./test.sh                # up + check + fmt + clippy + every test (see "Tests" below)
```

Host requirements: Docker with Compose (no buildx needed; the classic builder is used), bash,
python3, the Samba client `smbclient`, `iperf3`, `jq` (`testenv/bench-rsmb.sh` reads rsmb's JSON with it); for `testenv/validate-smbtap.sh` also `dumpcap`
and `tshark` usable without root (membership in the `wireshark` group).

## Topology

| Service | Container | Address | Role |
|---|---|---|---|
| `samba` | `rsmb-samba` | 10.213.45.10:445 | fast endpoint; multichannel off. Also 10.213.46.10 on `back` |
| `wan` | `rsmb-wan` | 10.213.45.30:445 | slow endpoint: router to `samba`, 100 ms each way |
| | | 10.213.45.30:4450 | same path, the port that can be blackholed |
| | | 10.213.45.30:5201 | iperf3 on `samba` through the same path (raw TCP ceiling) |
| `wan-bad` | `rsmb-wan-bad` | 10.213.45.31:445 | bad VPN: `delay 150ms 40ms loss 1% rate 20mbit` each way |
| `samba-edge` | `rsmb-samba-edge` | 10.213.45.11:445 | stingy server: 16 credits, 64 KiB I/O, 300 extra shares, multichannel on |
| `socks` | `rsmb-socks` | 10.213.45.20:1080 / :1081 | microsocks 1.0.5: no auth / `tester`:`testpass`. Also 10.213.47.20 on `priv` |
| `samba-private` | `rsmb-samba-private` | `fileserver.rsmb.test` = 10.213.47.10 | reachable only through `socks` |
| `samba-dc` | `rsmb-samba-dc` | 10.213.45.12:445 and :88, aliases `dc.rsmb.test` and `rsmb.test` | the Kerberos server: Samba AD DC, realm `RSMB.TEST`, its own KDC on port 88. The host reaches it directly by address; only the proxy resolves the two names |

Networks, with fixed bridge names so a capture interface is stable:

| Network | Bridge | Subnet | Host access |
|---|---|---|---|
| `pub` | `rsmb-pub` | 10.213.45.0/24 | host is 10.213.45.1 and reaches every container directly |
| `back` | `rsmb-back` | 10.213.46.0/24 | none: `internal: true`, `gateway_mode_ipv4: isolated` (bridge has no IPv4 address, host has no route) |
| `priv` | `rsmb-priv` | 10.213.47.0/24 | none, same settings |

No ports are published: the host talks to container addresses directly, so no userspace proxy
splits the TCP connection and hides Nagle or delayed-ACK effects. Fast and slow endpoints exist at
the same time; a test picks one by address.

Every container runs with `init: true`, so `down.sh` stops in about two seconds.

## Samba servers

One image, `rsmb-samba` (Alpine 3.24.4, Samba 4.23.8). The environment variable `ROLE` selects
`role-main.conf`, `role-edge.conf` or `role-private.conf`, included at the end of the shared
`smb.conf`.

- Standalone server, workgroup `TESTDOM`, NetBIOS off, port 445 only, `server min protocol = SMB2_02`.
- User `tester`, password `testpass` (throwaway values for a local container). Log in as
  `TESTDOM\tester`. A wrong password is refused (`NT_STATUS_LOGON_FAILURE`).
- Guest: `map to guest = Bad User`, so an unknown user name (any password) becomes a guest; the
  empty user name (`-U%`) is an anonymous login. Both may read `guest` and are refused on `share`.
- Shares (all three servers have them):

| Share | Path | Notes |
|---|---|---|
| `share` | `/srv/share` | all fixtures; writable only in `upload/` (the rest is owned by root) |
| `readonly` | `/srv/share` | the same tree, `read only = yes`: every write fails with `NT_STATUS_ACCESS_DENIED` |
| `guest` | `/srv/guest` | `guest ok`, read only, one file `hello.txt` |
| `encrypted` | `/srv/share` | `smb encrypt = required`: SMB3 clients encrypt; an SMB 2.1 client is refused |
| `tiny` | `/srv/tiny` | 10 MiB tmpfs, mode 1777, for disk-full tests |

- `samba` and `samba-private`: `server multi channel support = no`, so a client cannot open a
  second channel around the tap. `samba-private` also has `hosts allow = 10.213.47.20 127.0.0.1`
  (the proxy and its own healthcheck).
- `samba-edge`: multichannel on, `smb2 max credits = 16`, `smb2 max read/write/trans = 65536`, plus
  300 shares `edge001`..`edge300` (every tenth, `edge010$` .. `edge300$`, hidden), all pointing at
  `/srv/guest`. Samba accepted all of these values. Measured with tshark against `smbclient`: the
  NEGOTIATE response offers 65536 / 65536 / 65536 and the multichannel capability; the most
  credits granted in one response was 16; a 50 MiB read by smbclient went out as about 800 READs
  of 64 KiB.

## Latency: a router hop, not the host

`wan` and `wan-bad` run `testenv/wan/router.sh` (image `rsmb-wan`, Alpine `iptables`, which is the
nf_tables variant, and `iproute2`):

- `DNAT` of `PUB_ADDR:445`, `:4450` (to 445) and `:5201` to `samba` on `back`, `SNAT --to-source`
  the router's own `back` address, so replies come back through the router. Rules match by
  address; interface names are looked up from the addresses, never assumed.
- `tc qdisc replace dev <if> root netem $NETEM limit 100000` on both interfaces, so each direction
  is delayed once at a forwarding hop: the round trip is 200 ms, with no TCP split. The large
  `limit` keeps netem from dropping packets at bulk rates (the default is 1000 packets).
- `net.ipv4.ip_forward=1` is set in `compose.yaml` (`/proc/sys` is read-only inside a container).
  The container has `NET_ADMIN` and nothing more.
- ARP passes through netem as well, so the script pings `samba` and the host's bridge address once
  at start, before anything is measured.
- The script runs under `set -eu`; any failed step stops the container.
- Healthcheck (`router.sh health`, every 2 s): fails unless a netem qdisc with `limit 100000` is the
  root qdisc on both interfaces and the DNAT and SNAT rules exist. After start-up two failures make
  the container unhealthy; removing the qdisc by hand turned it unhealthy within 4 s, and
  `check.sh` then stopped at its first section.

`up.sh` checks first that `sch_netem` is loaded, built in, or on disk for the running kernel; if it
is not (typically after a kernel upgrade without a reboot) it says so and exits. On this machine
(kernel 7.3.4) the module was not loaded and autoloaded when the first router set its qdisc.

Measured (median of 5 TCP connects after one warm-up, by `check.sh`): fast endpoint 25–35 µs, slow
endpoint 200.1–200.2 ms, `wan-bad` about 300–360 ms (jitter, by design).

### Raw TCP ceiling

iperf3 runs on `samba` (`iperf3 -s`), reachable at 10.213.45.30:5201 through `wan`. Measured host to
`samba` through `wan`, 12 s: slow start takes about 2 s, then a steady 165 Mbit/s (about 20 MB/s;
`iperf3 -R`, the other direction, 135 Mbit/s over 8 s including slow start). The direct path
(10.213.45.10:5201) carries about 60 Gbit/s. Throughput of a client at the slow endpoint is judged
against this ceiling, not against the link: the limit comes from TCP windows at 200 ms, not from
netem. `check.sh` runs only 2 s, which is still in slow start, so its number is lower.

### Fallbacks (not needed on this machine)

- Router does not work: an extra network on `samba` with `netem delay 200ms` on that interface
  (same round-trip time, one direction delayed).
- netem cannot load: `up.sh` says why (usually a kernel upgraded without a reboot).
- Last resort: a delay option in the tap, documented as optimistic. Not implemented.

## SOCKS5 and the private server

`rsmb-socks` runs two microsocks 1.0.5 processes, built from the release tarball (sha256 pinned in
the Dockerfile; Alpine has microsocks only in edge/testing): port 1080 without authentication,
port 1081 with `tester`/`testpass`. Logging stays on: each proxied connection logs
`client[N] <client-ip>: connected to fileserver.rsmb.test:445`, with the name as the client sent it
and resolved by the proxy (Docker's embedded DNS knows the network alias).

Proof that traffic went through the proxy is positive and negative, all checked by `check.sh`:

- `fileserver.rsmb.test` does not resolve on the host;
- a direct connection to 10.213.47.10:445 fails (the host has no route into `priv`; the packet goes
  to the default route and times out);
- through each listener, an SMB2 NEGOTIATE reaches `samba-private` and gets an SMB2 answer, the
  proxy log gains one `connected to fileserver.rsmb.test:445` line per connection, and the
  independent connection count on `samba-private` rises by one per connection;
- a wrong password, and port 1081 without credentials, are refused;

`samba-private` is also configured to admit only the proxy's address (`hosts allow` in
`role-private.conf`). `check.sh` does not check that: nothing but the proxy and `samba-private`
sits on `priv`, and testing it would need a third container on that network. The proof does not
need it: the host has no route into `priv`.

The proxy also sits on `pub`, so it reaches `samba` (10.213.45.10) and `wan` (10.213.45.30)
too. The tests use that on purpose: a session through the proxy to `samba`, which the host also
reaches directly, is how `cut_through_the_proxy` and `proxy_failures` show that rsmb never falls
back to a direct connection when the proxy is gone; `--proxy socks5://10.213.45.20:1080 -H
10.213.45.30` puts the 100 ms each way between the proxy and the server, which is how the
proxied figures of `docs/performance.md` were taken (there is no latency between the client and
the proxy here).

Two things microsocks does that matter for measuring: it parses **one `recv` per handshake
message** (a request split over two writes is answered `General SOCKS server failure`; the tap's
SOCKS5 mode forwards each message in one write, `docs/measuring.md`), and it does **not set
`TCP_NODELAY`** on its socket to the server, so a burst of requests leaves its last partial
segment one round trip later (Nagle's algorithm; `docs/performance.md`, "Through a SOCKS5
proxy").

## The Kerberos server: `samba-dc`

Image `rsmb-samba-dc` (`testenv/samba-dc/`: `Dockerfile`, `smb.conf`, `krb5.conf`, `mkcache`):
Alpine 3.24.4, `samba-dc` (Samba 4.23.8 with its own Heimdal KDC), `samba-client`, and MIT `krb5`
(1.22.3: `kinit`, `kvno`, `klist`) to make ticket caches inside the container. The domain is
provisioned **at image build**: `samba-tool domain provision --realm=RSMB.TEST --domain=RSMB
--server-role=dc --dns-backend=SAMBA_INTERNAL --host-name=dc --host-ip=10.213.45.12`, with
`--option='vfs objects = dfs_samba4 acl_xattr xattr_tdb'`. Without `xattr_tdb` provisioning fails
in a build container at `setsysvolacl`: the NT ACLs need the `security.NTACL` extended attribute,
which a build may not set; `xattr_tdb` keeps them in a file. Provisioning writes its own
`smb.conf` (with a `dns forwarder` taken from the building host's `resolv.conf`); the image
replaces it with its own. The realm's keys are made at build and are in the image only, in no
file of the repository; the container's hostname does not matter (`netbios name = DC` is in
`smb.conf`), and a cache made before `docker restart` still logs in after it.

`smb.conf`: `server role = active directory domain controller`, `realm = RSMB.TEST`, `workgroup
= RSMB`, NetBIOS off, port 445 only, no printers, `server multi channel support = no` (a tap must
see everything), and **`log level = 1 kerberos:3`**: at `kerberos:3` the KDC writes one line per
request that names the client, the **source address** and the service, for example
`Kerberos: TGS-REQ tester@RSMB.TEST from ipv4:10.213.45.20:44566 for cifs/dc.rsmb.test@RSMB.TEST
[renewable-ok, canonicalize, renewable, forwardable]` (at `kerberos:2` or lower nothing). In 14
runs while planning, the number of such lines equalled packet counters on port 88, so a line count
is a request count (one exception: a request with a TGT expired for more than five minutes was
answered without such a line). The tests read the lines with `docker logs rsmb-samba-dc`.
`krb5.conf` is for `kinit` and `kvno` inside the container only (`kdc = 127.0.0.1`, no DNS).

What is in the realm, and why:

| Thing | For |
|---|---|
| user `tester` / `testpass` | every ordinary Kerberos test |
| users `kdirect`, `kproxy` / `testpass` | `kerberos_kdc_is_asked_through_the_proxy` only: lines of the KDC's log that name them belong to that test, whatever else runs |
| user `kother` / `testpass` | `kerberos_reconnect_reads_the_cache_again` only: the cache of another principal put in a session's place |
| SPN `cifs/dc.rsmb.test` | nothing to add: the machine account has `HOST/dc.rsmb.test` and the KDC maps `cifs` to it |
| SPN `cifs/10.213.45.12` | `-H` by address |
| SPN `cifs/127.0.0.1` | a tap in front of the DC (the tap is reached as 127.0.0.1) |
| SPN `cifs/10.213.45.10` | a valid ticket for `samba`, which does no Kerberos: the server's refusal of a ticket |
| share `share` (`/srv/share`): `hello.txt` (`kerberos works` and a newline), `sub/1m.bin` (1,048,577 bytes: the numbers from `seq 1 200000`), `upload/` (mode 1777) | a listing, a read with a checksum, a write. Files written over SMB belong to the id Samba maps the realm's user to, so a test's scratch directory is made with mode 1777, not owned by a Unix user |
| share `encrypted` (the same path, `smb encrypt = required`) | encryption with a Kerberos session key |
| the admin password `Adm1n-testpass` | a throwaway value inside a local image; the tests use it inside the DC at run time for `smbcacls` and `smbclient` (the NT ACL fixture of `access`, and the DOS attributes of every scratch directory, below) |

`mkcache -f|-F <user> [<spn>…]` (in the image, `/usr/local/bin/mkcache`) runs `kinit` for
`<user>@RSMB.TEST` (password `testpass`) into a temporary file cache with a ticket-granting
ticket that is forwardable (`-f`) or not (`-F`), then `kvno` for each SPN, and writes the cache
file's bytes to stdout. The tests make every cache this way (`docker exec rsmb-samba-dc mkcache
…`, 0.1 s); nothing on the host's Kerberos is used. Two facts of this KDC that the tests design
around: it refuses every TGS request with a ticket of two minutes or less (Samba takes it for a
password-change ticket; `kinit -l 121s` is the shortest lifetime that yields a service ticket), so
an expired ticket is made by rewriting a copy of a cache, not by waiting; and a server accepts a
ticket for five minutes after its end (clock allowance). It answers a TGS request over UDP with
`KRB5KRB_ERR_RESPONSE_TOO_BIG` (the reply is about 1,500 bytes), so a client that starts with UDP
pays one round trip for nothing here.

**The DC is the only server with NT ACLs** (`acl_xattr`): the main Samba servers derive NT rights
from Unix modes. The `access` tests (Stage 6) make their NT ACLs at run time in their scratch
directory with `smbcacls //127.0.0.1/share … -U 'RSMB\Administrator%Adm1n-testpass' --set …`
inside the DC (smbd serves the change at once; `xattr_tdb` keeps the ACL in a file, so no
privilege is needed; about 1.9 s for the whole fixture). Nothing in the image changed for it.

**How the DC keeps DOS attributes, and why the tests remove through Samba.** With `xattr_tdb`
the DC keeps DOS attributes and NT ACLs in `/var/lib/samba/xattr.tdb`, **keyed by the file's
inode** (device and inode, 16 bytes), not on the file. A record outlives its object when the
object is removed outside Samba (`rm -rf` in the container) and when Samba's own rename with
replace drops the replaced file; the next object made outside Samba that gets that inode inherits
the old record. Measured: a file set `+r` over SMB and removed with `rm -f`, the next file made
with `docker exec` took inode 18889957 and showed `R`; the same after `rename new.txt old.txt -f`
over an `R` file. **A directory's record keeps the directory bit:** three directories made over
SMB and removed with `rmdir` in the container, then eight files made in the container: three of
them took the directories' inodes (18891526, 18891558, 18891628) and listed as `D 0`, `setmode
-rsh` left `attributes: D (10)` (it clears R, S and H, never D), and `deltree` of their parent
failed with `NT_STATUS_NOT_A_DIRECTORY listing …\f3\*`. A stale NT ACL is ignored when the
object's Unix mode no longer matches the hash stored with it (a directory made in the container
on the inode of one whose ACL was set with `smbcacls` showed the ACL derived from its mode).
**Through SMB nothing goes stale**: an object created over SMB gets a fresh DOS attribute
(Samba writes one when it creates), and one removed over SMB (`rm`, `rmdir`, `deltree`) takes its
record with it (measured: no record left for any of five objects after `deltree`). The main servers keep the attribute on the
file itself and do not leak.

So, on the DC only:

- `Scratch`'s `Drop` removes its directory **through Samba** before `rm -rf`: one `smbclient`
  call as the administrator (root to Samba, so no ACL of a fixture stops it), `setmode "<path>"
  -rsh` for the directory and everything in it (`deltree` refuses a read-only object with
  `NT_STATUS_CANNOT_DELETE`), then `deltree <name>`. A failure is printed, not raised. The
  `stale test directories` step of `./test.sh` does the same for every `upload/rsmb-test-*` left
  on the DC (a test killed before its `Drop`).
- `Scratch::new` makes its directory in the container, clears its flags over SMB (`smbclient …
  -c "cd upload; setmode <name> -rsh; allinfo <name>"`; `setmode .` is refused, so a directory is
  cleared from its parent) and checks that it lists as `attributes: D (10)` with the ACL Samba
  derives from root's mode 1777 (`smbcacls`, compared with `DC_SCRATCH_ACL`). Otherwise the inode
  carried a record that outlived a replaced file: the directory is removed with `rmdir` over SMB
  (which drops the record) and made again, at most three times.
- `nt_fixture` clears the flags of every object it makes in the container (`setmode -rsh`) and
  then lists them over SMB (`recurse; ls` as the administrator): a file that lists with `D`, a
  directory without it, or any `R`, `H` or `S` fails the test at once with a message that names
  the path, the stale record and how to recreate the DC.
- No test replaces a read-only file on the DC: the replaced file's record cannot be reached by
  any path afterwards. F82's `put -f notes.txt E/file.txt` and `put -f notes.txt F/file.txt` (K2)
  replace files whose records hold an NT ACL only: two such records per run of F82 (counted: 4
  after `./test.sh fast`, 8 after two more runs of F82 each way, none from the rest of
  `tests/fast`), harmless while the stored hash does not match the new object's mode, and caught
  by `Scratch::new` for its directory if it ever does.

Without this an exact-mask test fails at random by inode reuse, a stale `R` refuses `rm`, and a
file with a directory's record breaks `ls` of its parent (`docs/plans/stage-6.md` 2.13, W11).
`access_predicts_nt_acls` first asserts that its scratch directory's mask is 0x001f01ff and its
attributes 0x10.

**Stale records already in a DC.** The DC keeps its state in the container (no volume), so
recreating the container drops every record: under the exclusive lock, `flock -x
/tmp/rsmb-env.lock docker compose -f testenv/compose.yaml up -d --force-recreate --wait samba-dc`
(seconds). Counted with python's `tdb` module in the container (records whose device and inode no
file has): before the fix 293 such records on the share's device, 83 of them with a DOS attribute
and 16 with the directory bit; after recreating 0 (the 12 records of the image's build carry the
build's device and never match).

No capability, no volume, no connection counter (`ctl.sh conns` and `evict` do not apply): the
tests count connections in their taps and KDC requests in the KDC's log. The healthcheck looks for
ports 445 and 88 in LISTEN state in `/proc/net/tcp`, without connecting. Ports listen 0.3 s after
`docker restart`; healthy about 1.3 s after `docker run`. Image 219 MB; build from nothing 84.7 s
on this machine (95 packages, provisioning, users, SPNs), seconds with the layers cached.

The proxy is on `pub` too, so it reaches the DC by address and resolves both aliases:
`dc.rsmb.test` is the DC's name in the realm, `rsmb.test` is the realm's own name, where rsmb
looks for the KDC when no `krb5.conf` names one (as a domain's name resolves to its domain
controllers). The host resolves neither, but reaches 10.213.45.12 on ports 445 and 88 directly:
that is what makes "nothing went around the proxy" a claim that could fail.

## Fixtures: built to hurt

Created at image build: `testenv/samba/large-files.sh` (own image layer, so editing other fixtures
does not regenerate 5.5 GiB) and `testenv/samba/fixtures.sh`. The unusual names are defined once,
as `printf` byte escapes, in `testenv/samba/names.sh`, which `check.sh` also reads. Everything is
under the share `share` unless noted; every file and directory is owned by root and world-readable
unless noted. Each fixture targets one way a client loses time or data.

Scale (round trips, memory, bounded concurrency)

| Fixture | Content | Targets |
|---|---|---|
| `many/` | 10,000 empty files `f00001`..`f10000` | listing size, Tab completion at size |
| `huge/` | 100,000 empty files `f000001`..`f100000` | query buffer size: 172 queries (`smb2`) or 124 (`smb`) at 64 KiB, depending on the information class asked for, against 2–3 at 8 MiB |
| `xl/` | 1,000,000 empty files `f0000001`..`f1000000`, only with `RSMB_XL=1 ./testenv/up.sh` (build about 4.5 min; a later plain `up.sh` rebuilds without it) | memory of the cache; off by default |
| `longnames/` | 2,000 files with 250-byte names `0001_xxx…` | most bytes per entry |
| `fanout/` | 2,000 directories `d0001`..`d2000`, one `file.txt` each | unbounded traversal, credit exhaustion |
| `tree/` | breadth 4 × depth 4 = 340 directories `d0`..`d3`, one `file.txt` in each | routine traversal timing |
| `deep/` | 12 levels `l01/…/l12/bottom.txt` | depth |
| `chain/` | 60 levels `c01/…/c60/bottom.txt` | depth that cannot be parallelised |
| `smallfiles/` | 20 directories `d01`..`d20` × 100 files `f001.bin`..`f100.bin`, 1024–4096 bytes | per-file overhead of recursive transfers |

Sizes (chunking and offsets). Content is the AES-128-CTR keystream with key
`000102030405060708090a0b0c0d0e0f` and the IV shown, so it is pseudo-random and reproducible. Every
large length is odd, so no chunk size divides it.

| File | Bytes | IV | sha256 |
|---|---|---|---|
| `large/big.bin` | 1,181,116,007 (1.1 GiB) | `…01` | `ad2f1026d65a08bc81d0198783ffd36c104bf57a6c379a650794c3dd7e14de9a` |
| `large/over4g.bin` | 4,617,089,843 (4.3 GiB, offsets above 2^32) | `…02` | `d0b7cf9be6a8e93f2198475dd8a6a728d536f3bcb1489f545aa2f39cf4ad4d85` |
| `large/medium.bin` | 52,432,897 (50 MiB + 4097) | `…03` | `50556313740b0f8de619aaeccebf8c30140d68c25957b2d8164d53c374cc62ca` |
| `sizes/<n>.bin` | n = 0, 1, 65535, 65536, 65537, 1048575, 1048576, 1048577, 8388607, 8388608, 8388609 | `…05` (prefix of one stream) | checked by `check.sh` |
| `smallfiles/…` | slices of an 8 MiB stream with IV `…04` | | |

Checksums are always taken on the server with `docker exec rsmb-samba sha256sum /srv/share/<path>`.

Names. Files and directories, also nested several levels deep. Exact UTF-8 bytes (hex):

| Name in `names.sh` | Text | Code points | Bytes |
|---|---|---|---|
| `ZH_HANS` | 简体中文 | U+7B80 U+4F53 U+4E2D U+6587 | `e7 ae 80 e4 bd 93 e4 b8 ad e6 96 87` |
| `ZH_HANT` | 繁體中文 | U+7E41 U+9AD4 U+4E2D U+6587 | `e7 b9 81 e9 ab 94 e4 b8 ad e6 96 87` |
| `KO_NFC` | 한글, precomposed | U+D55C U+AE00 | `ed 95 9c ea b8 80` |
| `KO_NFD` | 한글, conjoining jamo | U+1112 U+1161 U+11AB U+1100 U+1173 U+11AF | `e1 84 92 e1 85 a1 e1 86 ab e1 84 80 e1 85 b3 e1 86 af` |
| `JA_HIRA_NFD` | がっこう with combining dakuten | U+304B U+3099 U+3063 U+3053 U+3046 | `e3 81 8b e3 82 99 e3 81 a3 e3 81 93 e3 81 86` |
| `JA_KATA_NFD` | パン with combining handakuten | U+30CF U+309A U+30F3 | `e3 83 8f e3 82 9a e3 83 b3` |
| `HAN_SMP` | 𠮷野家 (first character outside the BMP: a surrogate pair in UTF-16) | U+20BB7 U+91CE U+5BB6 | `f0 a0 ae b7 e9 87 8e e5 ae b6` |
| `FULLWIDTH` | ＦＵＬＬ | U+FF26 U+FF35 U+FF2C U+FF2C | `ef bc a6 ef bc b5 ef bc ac ef bc ac` |
| `IDEO_SPACE` | 東京　大阪 (ideographic space, not ASCII whitespace) | U+6771 U+4EAC U+3000 U+5927 U+962A | `e6 9d b1 e4 ba ac e3 80 80 e5 a4 a7 e9 98 aa` |
| `UMLAUT` | Grüße | | `47 72 c3 bc c3 9f 65` |
| `CYRILLIC` | Привет | | `d0 9f d1 80 d0 b8 d0 b2 d0 b5 d1 82` |
| `EMOJI` | 😀 | U+1F600 | `f0 9f 98 80` |
| `CAFE_NFC` | café, NFC | U+00E9 | `63 61 66 c3 a9` |
| `CAFE_NFD` | café, NFD | U+0065 U+0301 | `63 61 66 65 cc 81` |
| `EUCKR` | 한글 in EUC-KR (not UTF-8) | | `c7 d1 b1 db` |
| `GBK` | 中文 in GBK (not UTF-8) | | `d6 d0 ce c4` |
| `SJIS` | 日本 in Shift-JIS (not UTF-8) | | `93 fa 96 7b` |

- `cjk/` (1,010 entries): a directory per East Asian name above, each holding `<name>.txt`; 1,000
  files `<name>_<nnnn>.txt` (nnnn = 0000..0999, name = the (nnnn mod 9)th of the nine); and
  `nested/` with the nine names as nine nested directories and `leaf.txt` at the bottom. The
  Korean NFC/NFD pair are two different directories that look the same.
- `special/` (22 entries): `with space.txt`, `  two leading spaces.txt`, `Grüße.txt`,
  `Привет.txt`, `😀 smile.txt`, `café.txt` twice (NFC and NFD), `it's.txt`, `-rf`, `--help.txt`,
  `[abc].txt`, `{a,b}.txt`, `bang!.txt`, `#hash.txt`, `$dollar.txt`, `semi;colon.txt`,
  `per%cent.txt`, `amp&.txt`, ``tick`.txt``, `Case.txt` and `case.txt`, and the directory chain
  `dir with space/Grüße/Привет/😀/leaf.txt`.
- `mangled/` (12 files on disk): the three legacy-encoded names `<EUCKR>.txt`, `<GBK>.txt`,
  `<SJIS>.txt`; Windows-forbidden characters `colon:name.txt`, `question?.txt`, `star*.txt`,
  `pipe|.txt`, `less<more>.txt`, `quote".txt`, `back\slash.txt`; `trailingdot.` and
  `trailingspace ` (ends in a space). Each file contains its own name and a newline, so a mangled
  name can be traced back by reading the file.

What Samba 4.23.8 does with them, measured with `smbclient` and checked by `check.sh`:

- `cjk/` and `special/` are listed with exactly the bytes above (the listing is compared as a set
  of byte strings), and the nested files can be read by full path.
- The nine Windows-illegal names in `mangled/` are listed as 8.3 mangled names such as
  `C69YSY~1.TXT` (mangled names are hashes and can change with the Samba version). The four whose
  original contains a DOS wildcard character (`?`, `*`, `<` and `>`, `"`) cannot be opened by
  their mangled name: `NT_STATUS_OBJECT_NAME_NOT_FOUND` (measured with `smbclient`). The other
  five (`:`, `|`, `\`, trailing dot, trailing space) can. Over rsmb's own CREATE (Stage 6) all
  nine open for their attributes (masks 0x00120089), but the four answer an open for data with
  `OBJECT_NAME_INVALID` (`cat mangled/LUI9XK~T.TXT`: `invalid_name`), so `access -e /mangled`
  shows `get` for all nine.
- The three legacy-encoded names are **not listed at all**: Samba logs
  `Conversion error: illegal character` and skips them. The plan expected them to appear mangled;
  they do not, and no Samba option was found that would present them. A client therefore cannot
  see them over SMB; they still exist on disk (`check.sh` counts 12 there and 9 over SMB). The
  Windows-only case of unpaired UTF-16 surrogates cannot come from Samba at all and needs a unit
  test in the client.
- `special/` holds `Case.txt` and `case.txt`; SMB name lookup is case-insensitive, so opening one
  by name may return the other.

Hostile structure

| Fixture | Content | Behaviour over SMB (smbclient) |
|---|---|---|
| `forbidden/` | root, 0700, holds `secret.txt` | listing and reading denied; `cd forbidden` in smbclient succeeds (its check opens the directory for attributes only) |
| `tree/forbidden/` | same, inside the traversal tree | listing denied |
| `tree/unreadable.txt` | root, 0600 | reading denied |
| `traverse-only/` | root, 0711, holds `known.txt` (0644) | can be entered, listing denied, `known.txt` readable by name |
| `loops/self -> .`, `loops/up -> ..` | symlinks Samba follows | a recursive walk sees endless depth |
| `loops/dangling -> /nonexistent` | dangling symlink | Samba does not list it (`loops/` shows `file.txt`, `self`, `up`) |
| `links/a/to-c -> ../b/c`, `links/b/c/file.txt` (`linked`) | a link to a sibling: two paths to one directory | both are directories with one file id and the same content; which of the two a walk lists (the first in breadth-first order, `a/to-c`, every time) and which it reports as already listed (`b/c`). Added in Stage 3 |
| `upload/` | owned by `tester`, on the container's own disk (not tmpfs) | the place for upload tests, including multi-gigabyte ones; emptied only by recreating the container (`down.sh` + `up.sh`) |

Hostile server and link: `samba-edge`, the `tiny` share, `wan-bad`, the blackhole port, a paused
server (`ctl.sh pause samba`: TCP is accepted by the kernel, nothing answers), and the tap cutting
a connection at any moment (see `docs/measuring.md`).

Image: 5.96 GB, of which 5.5 GiB are the large files. On this machine: build and start from
nothing 68 s (Stages 1 to 4; with the DC's 84.7 s about 150 s); a rebuild from the fixtures layer on (the large files are a layer of their own and
stay) 36.4 s, then `down.sh` and `up.sh` 6 s (Stage 3, for `links/`); `down.sh` 0.8 s; `up.sh` with the images built 4.6–4.7 s; `up.sh` when already running
about 1 s. `up.sh` prints the build log only when the build fails (the classic builder ignores
`--quiet`).

## Controls for tests and spikes: `testenv/ctl.sh`

| Command | Effect |
|---|---|
| `ctl.sh blackhole on` / `off` | `iptables -I FORWARD -p tcp -m conntrack --ctorigdstport 4450 -j DROP` in `wan`: every packet of every connection that was made to 10.213.45.30:4450 is dropped silently, in both directions, including connections already open. Port 445 on `wan` is unaffected. `off` removes the rule. |
| `ctl.sh evict <service> <path>` | `dd if=<path> iflag=nocache count=0` inside the container: drops the file from the page cache. Measured on `big.bin`: 13–56 ms to read when cached, 380–600 ms after eviction. `fincore` reports 0 pages for files on overlayfs whether cached or not, so it cannot confirm an eviction here; `check.sh` compares read times instead. |
| `ctl.sh conns <service>` | TCP connections to port 445 from outside the container since the last reset (see below) |
| `ctl.sh conns-reset <service>` | sets that count to zero |
| `ctl.sh pause <service>` / `unpause <service>` | `docker pause` / `docker unpause`; also `samba-dc` (a silent KDC: TCP is accepted, nothing answers) |
| `ctl.sh drops` | packets netem on `wan` has dropped since the container started (summed over its qdiscs). Fails if `wan` cannot be asked, so a broken `docker exec` never reads as 0. `check.sh`, the benchmark and both spikes use it. |

### The lock: `/tmp/rsmb-env.lock`

One `flock` file arbitrates who may disturb the environment. The convention, in one place:

| Holder | Mode | Why |
|---|---|---|
| `check.sh` | exclusive, whole run | resets connection counters, turns the blackhole on and off |
| `up.sh` | exclusive, only while it unpauses servers and turns the blackhole off | cleans up after an interrupted spike without disturbing a running one |
| `./test.sh`, stale test directories | exclusive, only while it removes `upload/rsmb-test-*` on the three Samba servers and the DC | a test run or benchmark holding the shared lock may be writing there |
| `spikes/*/run.sh`, steps 5, 6, 7c, 8, 10, 11 (smb-rs) and 5, 6, 10 (smb2) | exclusive, per step | timed 1.1 GiB transfers, cache eviction, pause, blackhole, `conns` counting |
| `spikes/*/run.sh`, all other steps | shared, per step | ordinary requests that must not be disturbed |
| `testenv/bench-smbclientng.sh` | shared, whole run | timed runs that must not be disturbed; changes nothing |
| `testenv/bench-rsmb.sh` | shared, whole run | the same, for rsmb |
| `testenv/bench-rsmb.sh transfers` | exclusive, whole run | timed bulk transfers at the slow endpoint and iperf3, which tests running beside it would distort |
| `testenv/bench-rsmb.sh walk` | shared, whole run | timed walks beside smbclient-ng; changes nothing |
| `./test.sh`: `tests/fast`, `tests/fast through the proxy`, `tests/slow` | shared, per step (`flock -s`) | ordinary requests; each test has its own tap. The proxied step counts the proxy's log lines before and after; other shared holders can only add to them |
| `./test.sh`: `tests/serial` | exclusive, per step (`flock -x`) | pauses `samba`, `socks` and `samba-dc`, turns the blackhole on, resets connection counters, counts the proxy's log lines exactly, asserts wall times |
| `ctl.sh`, `validate-smbtap.sh` | none themselves | |
| `./test.sh`, other steps | none | `check.sh` takes its own exclusive lock, so `./test.sh` holds none while calling it |

Shared holders run side by side; an exclusive holder waits for them all and they wait for it. A
**No wait is unbounded:** `./test.sh`, `up.sh`, `check.sh` and the two benchmarks wait at most 30
minutes (`flock -w 1800`; the longest holder is `bench-rsmb.sh transfers`, about 21 minutes) and
then stop with a message that names `/tmp/rsmb-env.lock` and says another run holds it. A
spike that is interrupted during step 10 turns the blackhole off and unpauses `samba` on exit
(`trap`), and `up.sh` does the same if the shell was killed outright.

### The independent connection count

Each Samba container adds one rule at start (`entrypoint.sh`, needs `NET_ADMIN`):

```
iptables -t nat -A PREROUTING -p tcp --dport 445 ! -i lo -j ACCEPT
```

The nat table is consulted only for the first packet of a connection (conntrack keeps the NAT
decision for the rest), so the rule's packet counter is the number of new TCP connections to port
445: a retransmitted SYN is not counted twice. Loopback connections never pass nat PREROUTING, so
the healthcheck (which connects over 127.0.0.1) does not show up. `ctl.sh conns` reads the counter
with `iptables -t nat -L PREROUTING -v -x -n`; `conns-reset` zeroes it with `iptables -t nat -Z`.
Connections through `wan` arrive from `wan`'s address and are counted the same way. It knows
nothing of smbtap, so it can catch a client that bypasses the tap. `check.sh` verifies it: after a
reset, three direct connects and one through `wan` read as exactly 4.

## `check.sh`: what it proves

It uses only tools independent of this project's code and fails with a non-zero exit and a `FAIL`
line per broken assumption:

`check.sh` exports `KRB5_CONFIG=/dev/null` at its top: the host's `smbclient` tries Kerberos by
default and would read the host's `/etc/krb5.conf`, which may describe another realm. Nothing in
it depends on the host's Kerberos configuration or ticket caches.

1. All seven containers healthy (stops here otherwise); prints the Samba version.
2. TCP connect time (not ping: only one direction of a ping crosses each netem qdisc on the way
   out): fast < 5 ms, slow 195–230 ms, `wan-bad` > 150 ms.
3. Logins: `TESTDOM\tester`; wrong password refused; guest and anonymous read `guest`; guest
   refused on `share`; `encrypted` readable with SMB3 and refused at SMB 2.1; the slow endpoint
   serves the share.
4. Fixtures: entry counts of `many/`, `huge/`, `fanout/`, `longnames/`; `forbidden/`,
   `tree/forbidden/`, `traverse-only/` cannot be listed; the secret and unreadable files cannot be
   read; `traverse-only/known.txt` can; `readonly` refuses writes; `share/upload/` and `tiny`
   accept a write and a delete; size and sha256 of every file in `large/` and `sizes/`, downloaded
   with smbclient, match `docker exec … sha256sum`; `cjk/` and `special/` listings equal the
   expected byte strings; nested name paths readable; `mangled/` shows 9 mangled names and has 12
   files on disk; `links/a/to-c/file.txt` reads `linked` over SMB and `links/a/to-c` is a symbolic
   link on disk.
5. SOCKS5 and `samba-private` as described above.
6. `samba-edge` lists 300 edge shares, hidden ones included.
7. Controls: connection count exact; blackhole open when off, silent when on, port 445 unaffected;
   eviction makes a read at least 3× slower.
8. iperf3 through `wan` runs; netem on `wan` dropped no packet; `wan-bad`'s drop counters are
   printed (loss is intended there).
9. Kerberos and the DC (after point 5): inside the DC its own `kinit` and `smbclient
   --use-kerberos=required` read `hello.txt` (`kerberos works`); `kvno` gets a ticket for each of
   `cifs/10.213.45.12`, `cifs/127.0.0.1`, `cifs/10.213.45.10`, `cifs/dc.rsmb.test` and refuses
   `cifs/nosuch.rsmb.test`; the KDC's log gained at least 4 `TGS-REQ tester@RSMB.TEST from ipv4:`
   lines meanwhile (the wording the tests count); `mkcache -f tester cifs/dc.rsmb.test` writes at
   least 1,000 bytes that start `05 04`; `dc.rsmb.test` and `rsmb.test` do not resolve on the
   host, and the proxy's port 1080 resolves both and gets an SMB2 answer; the KDC
   10.213.45.12:88 is reachable from the host without the proxy (within 3 s); the host's
   `smbclient --use-kerberos=off` with a wrong password at the DC gets `NT_STATUS_LOGON_FAILURE`.

## Tests

`./test.sh` has two modes (details and the budget in `docs/testing.md`):

```
./test.sh fast    environment up · stale test directories · check.sh · fmt · clippy · build ·
                  unit tests · tests/fast (direct) · tests/fast through the proxy
./test.sh         the same, then tests/slow · tests/serial · smbtap against tshark
```

`fast` is everything without injected latency: `tests/fast` direct to `samba`, then the same
binary through `socks` to `samba-private` (`RSMB_TEST_VIA=socks5`), so `tests/fast` needs
`socks` and `samba-private` as well as `samba`; its Kerberos tests need `samba-dc` (by address
direct, as `dc.rsmb.test` through the proxy).

"Stale test directories" removes `upload/rsmb-test-*` on the three Samba containers and the DC with
`docker exec`. Every test that writes to a server does it below a scratch directory of its own,
`upload/rsmb-test-<pid>-<n>` (owned by tester, removed when the test ends, also when it fails);
`tiny` is used by one test only, which empties it. A fixture a test needs for itself alone is made
there at run time: the `access` tests (Stage 6) make a POSIX fixture as root with `docker exec`
(owners, modes, a sticky directory, a FIFO, the DOS read-only attribute set over SMB from inside
the container; 0.15 s; `posix_fixture`) and an NT ACL fixture on the DC (`nt_fixture`, above); both
are written out in `docs/plans/stage-6.md` section 6. Each step prints its duration; the last line
prints the counted time against the budget (150 s for `fast`, 600 s full; Stage 1 had 90 and 240,
Stage 2 120 and 420, Stage 3 120 and 540; Stages 5 and 6 kept 150 and 600). The measured times are in `docs/testing.md`.

The tests of `tree` and `find` compare with the server's own `find`, run in the container **as
tester** (`docker exec -u tester -w /srv/share …`), so it sees what the SMB user sees (as root it
would see inside the denied directories: `find tree -mindepth 1` gives 683 as root, 682 as
tester). The containers are Alpine: `find` is BusyBox on musl, with `-name`, `-iname`, `-type`,
`-maxdepth`, `-mindepth`, `-print0` and `-size N[bck]` but no `-printf` and no `M` or `G` units;
its matcher takes names as UTF-8 without any locale variable.

The interactive tests run rsmb on a pseudo-terminal through `testenv/ptyrun.py` (python3's `pty`
module; python3 is a host requirement already). The serial tests pause `samba`, `socks` or `samba-dc` and turn the
blackhole on; each undoes it in a guard that also runs when the test fails, and `up.sh` cleans up
after a killed run.
