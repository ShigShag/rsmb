# Stage 5 plan: Kerberos login from a ticket cache (`-k`)

> Written on 2026-11-18 by a planning agent that read the repository and measured against a
> throwaway Samba AD DC, before any Stage 5 code exists. It follows amendment 9 of
> [`../brief.md`](../brief.md) and the design the orchestrator gave; where it departs from that
> design, section 4 says where and why. It is written for one implementing agent who was not
> present: everything needed to build Stage 5 in one pass is here or in a document this one
> names. **Revised on 2026-11-18 after the adversary's review:** section 11 lists the seventeen
> findings, the evidence for each and what changed; the text above it is the revised plan, and
> 2.11 holds what was measured for the revision. Section 10 lists what could not be verified.
>
> Words are used as in `docs/measuring.md` (request, flight) and in
> [`stage-4.md`](stage-4.md) (the proxy, direct). "The crate" is `smb2` 0.43.0 (the newest
> release: `cargo search smb2`, 2026-11-18). **The DC** is the new container `samba-dc`:
> a Samba 4.23.8 Active Directory domain controller with its own (Heimdal) KDC, realm
> `RSMB.TEST`, `dc.rsmb.test` = 10.213.45.12 on the network `pub`. **The cache** is the Kerberos
> ticket cache file. **TGT** is a ticket-granting ticket (`krbtgt/RSMB.TEST@RSMB.TEST`); a
> **service ticket** is one for `cifs/<host>`; a cache is **TGT-only** when it holds no service
> ticket for the server asked for. **SPN** is the service's name, `cifs/<host>`. **The relay** is
> the piece of section 5.6 that carries the crate's KDC request over rsmb's own dialer.

## 1. Scope, what is not in it, exit criteria

**In scope** (brief, amendment 9):

- `-k` / `--kerberos`: log in with Kerberos from the ticket cache (`KRB5CCNAME`), with a service
  ticket in the cache or with a TGT only, direct and through `--proxy`. Through the proxy the
  exchange with the KDC goes through the proxy too: TCP to port 88, the KDC's name resolved by
  the proxy, no packet and no name lookup around it.
- `--keep-going` keeps only its long form; `-k` is Kerberos.
- The test environment gains a Kerberos server; `help all`, the documents and the README follow.

**Not in Stage 5:** Kerberos with a password (an AS exchange), a flag for the KDC or for the
cache file, PKINIT, cache types other than a file (`KCM:`, `KEYRING:`, `DIR:`, `MEMORY:`,
`API:`), writing to the cache (a service ticket rsmb obtained is used once and not stored),
finding the KDC through DNS (`_kerberos._tcp` SRV records), more than one KDC per realm, tickets
across realms (referrals), `include` and `includedir` in `krb5.conf`, `default_ccache_name` from
`krb5.conf`, renewing a ticket, re-authenticating a session in place when its ticket ends (the
session is replaced by a new login, 5.9), a TGS request built by rsmb itself (K2, 5.8),
delegation, NTLM as a fallback when Kerberos fails (never: `-k` means Kerberos or an error).

**Exit criteria:** every row of section 7.4 passes in `./test.sh`; the full run stays within
600 s of counted time and `./test.sh fast` within 150 s; `help all` has the flag, an example and
the topic; every document of section 9 matches the code; `grep -rn -- "-k\b"` over `src/`,
`tests/`, `testenv/`, `README.md`, `AGENTS.md` and `docs/*.md` (not `docs/plans/`, not
`docs/reviews.md`) finds `-k` only as Kerberos; the review's confirmed findings are fixed and
their tests are in the suite.

## 2. What was measured while planning

All on 2026-11-18, with throwaway files under `spikes/smb2/target/scratch-stage5/` (git-ignored,
not committed): `dc/` and `dc-final/` (two Dockerfiles of the DC), `krbprobe/` (a Rust program on
`smb2 = "=0.43.0"` with the transport of `src/session.rs` in small, a relay as in 5.6, and
`smbtap` in front when asked), `run.sh` (one probe run with what the KDC's log, the proxy's log
and two packet counters on port 88 gained meanwhile). The caches were deleted. The DC ran as a
container of its own on the project's network `rsmb_pub` at 10.213.45.12 with the alias
`dc.rsmb.test`; `testenv/` was not changed. Samba 4.23.8 (Alpine 3.24.4 `samba-dc`), MIT krb5
1.22.3 inside the DC, Heimdal 7.8.0 tools in a second throwaway container, microsocks 1.0.5,
smbclient-ng 3.1.0. Nothing of this needed the exclusive lock (the pauses were of the throwaway
DC only). The throwaway containers and images were removed at the end.

### 2.1 What exists today

| What | Where | State |
|---|---|---|
| Kerberos in the crate | `smb2::Session::setup_kerberos_from_ccache(conn, &KerberosCredentials, server_hostname, &CCache)`, `smb2::auth::kerberos::ccache::{parse_ccache, CCache}` | public. With a service ticket for `cifs/<server_hostname>` in the cache it builds the AP-REQ at once; with a TGT only it first runs a TGS exchange with `KerberosCredentials::kdc_address`. One SESSION_SETUP. `CCache` and its entries have public fields: rsmb can remove entries before handing it over |
| The crate's cache lookup | `CCache::find_service_ticket("cifs", host, realm)`, `find_tgt(realm)` | the first entry whose server is `cifs/<host>@<realm>` (two name parts; service, host and realm compared without ASCII case); else the first `krbtgt/<realm>@<realm>`. **No look at the end time**: an expired entry is taken |
| The crate's default cache | `load_ccache(None)` | reads `KRB5CCNAME` (strips `FILE:`); without the variable it returns an error, although its comment promises `/tmp/krb5cc_<uid>` |
| The crate's KDC client | `src/auth/kerberos/kdc.rs`, `send_to_kdc` | its own sockets, no hook: UDP first (3 attempts, 10 s each, 1 s and 2 s between), then TCP the same way; the address is a string (`host` or `host:port`, port 88) |
| The crate's TGS request | `encode_kdc_req_body` | KDC options fixed at `0x40810010`: forwardable, renewable, canonicalize, renewable-ok. `kerberos_encrypt` and `kerberos_decrypt` are `pub(crate)`; the HMAC they use is private |
| rsmb | `login` in `src/session.rs` | NTLM only (`Session::setup`). `-k` is `--keep-going` (`src/main.rs`). `classify` turns the crate's `Io`, `Timeout` and `SessionExpired` into `connection_lost` |
| Where `-k` means keep-going | `grep -rn -- '-k\b'` | `src/main.rs` (flag, one comment), `src/commands.rs` (synopsis, exit-code text, three topic texts, one test list), `tests/fast.rs` (6 places), `testenv/bench-rsmb.sh` (line 261), `docs/commands.md`, `docs/scripting.md`, `docs/architecture.md`, `docs/known-limitations.md`, `docs/testing.md`, `AGENTS.md` (index row of `scripting.md`). `tests/common/mod.rs` already uses the long form |
| The test environment | `testenv/` | six containers, no KDC |

### 2.2 The DC as a container

| Question | Measured |
|---|---|
| Provisioning at image build | `samba-tool domain provision --realm=RSMB.TEST --domain=RSMB --server-role=dc --dns-backend=SAMBA_INTERNAL --host-name=dc --host-ip=10.213.45.12 --adminpass=…` **fails** in a build container at `setsysvolacl` (the `security.NTACL` extended attribute needs a privilege a build does not have). With `--option='vfs objects = dfs_samba4 acl_xattr xattr_tdb'` (NT ACLs in a file) it succeeds in 5 s |
| What provisioning writes | its own `smb.conf`, with `dns forwarder` taken from the build container's `resolv.conf` (an address of the building host). The image replaces the file with its own |
| Build and size | from nothing 60.8 s (95 packages, then provisioning, users, fixtures), image 219 MB; with the layers cached, seconds |
| Start-up | ports 445 and 88 listening 0.3 s after `docker restart`, 0.69 s after `docker run`; healthy 1.34 s after `docker run` with the healthcheck of 6.2 (start interval 1 s). The first Kerberos login succeeded at the first try, 32 ms after the ports listened |
| Does the baked provisioning survive? | yes. After `docker restart` a cache made before the restart logs in. A second container from the same image with a random hostname and another address (10.213.45.13) accepted a ticket the first had issued: the realm's keys are in the image, the container's hostname does not matter (`netbios name = DC` is in `smb.conf`) |
| The SPN for the address | `samba-tool spn add cifs/10.213.45.12 'DC$'` at build. Afterwards `kvno cifs/10.213.45.12` and a login with `-H 10.213.45.12` work. The same for `cifs/127.0.0.1` (a tap in front of the DC is reached as 127.0.0.1). `cifs/dc.rsmb.test` needs nothing: the machine account has `HOST/dc.rsmb.test` and the KDC maps `cifs` to it |
| A TGT-only cache | `echo testpass \| kinit -f -c FILE:<file> tester@RSMB.TEST` inside the DC (its `/etc/krb5.conf` names `kdc = 127.0.0.1`). 1,673 bytes. 102 ms through `docker exec` |
| A cache with service tickets | the same, then `kvno -c FILE:<file> cifs/dc.rsmb.test cifs/10.213.45.12 cifs/127.0.0.1`. 5,579 bytes, 125 ms |
| A short-lived or expired ticket | `kinit -l 10s`, `30s`, `60s`, `2m` give a TGT, but **the KDC then refuses every TGS request with it** (`KRB5KRB_AP_ERR_TKT_EXPIRED`, reason `PAC check failed: Ticket is not a ticket-granting ticket`: Samba treats a ticket of two minutes or less as a password-change ticket). `-l 121s` is the shortest lifetime that yields a service ticket. A really expired service ticket therefore cannot be made in less than 121 s, and the server still accepts it for five more minutes (2.7) |
| What the KDC logs | with `log level = 1 kerberos:3` one line per request that names client, **source address** and service: `Kerberos: TGS-REQ tester@RSMB.TEST from ipv4:10.213.45.1:44566 for cifs/10.213.45.12@RSMB.TEST [renewable-ok, canonicalize, renewable, forwardable]`, and a result line (`TGS-REQ SUCCESS …`, `TGS-REQ ERR_BADOPTION … reason=…`). With `kerberos:2` or lower: nothing. About 25 log lines per request. In 14 probe runs the number of `… from ipv4:` lines equalled the packet counters on port 88 (`iptables -A INPUT -p udp/tcp --dport 88`), so a line count is a request count. One exception: a request with a TGT expired for more than five minutes was answered (error 32) without such a line |
| UDP | the KDC answers a TGS request over UDP with `KRB5KRB_ERR_RESPONSE_TOO_BIG` (the reply is 1,498 bytes for a 1,451-byte request); MIT `kinit` got the same for its AS request. A client that starts with UDP pays one wasted round trip here |
| An independent Kerberos client | inside the DC, `kinit` and then `smbclient //dc.rsmb.test/share --use-kerberos=required -c 'get hello.txt /dev/stdout'` prints the file (112 ms). The host's `smbclient` with a copied cache and its own `KRB5_CONFIG` refuses a server given by address (`Could not find a suitable mechtype in NEG_TOKEN_INIT`): `check.sh` uses the one in the container |
| Heimdal and impacket tools | Heimdal 7.8.0 `kinit` and `kgetcred` against the DC work (2.8). impacket 0.13.0 `getTGT.py` (Alpine `py3-impacket`) crashes on this KDC's AS reply (`Attempted "__str__" operation on ASN.1 schema object`, the reply has no start time): no impacket cache was made |

### 2.3 A Kerberos login through the crate

`krbprobe`: dial as rsmb does, NEGOTIATE, `Session::setup_kerberos_from_ccache`, then
TREE_CONNECT, a listing, two reads, a write and a delete. User and realm from the cache's
default principal. Requests and flights from `smbtap` (in SOCKS5 mode in front of the proxy for
the proxied rows).

| Case | Result | Login in the tap | KDC |
|---|---|---|---|
| direct, `10.213.45.12`, cache with service tickets | works | **NEGOTIATE 1, SESSION_SETUP 1: 2 requests in 2 flights, 1 connection** (NTLM: 3 in 3) | no request |
| direct, `10.213.45.12`, TGT-only (`kinit -f`), the crate's own KDC client (KDC = the server) | works | the same 2 | **2 requests** from 10.213.45.1: one over UDP answered `RESPONSE_TOO_BIG`, one over TCP |
| the same with the relay | works | the same 2 | **1 request**, over TCP, from 10.213.45.1; 4.1 ms |
| through port 1080, `dc.rsmb.test`, cache with service tickets | works | the same 2; proxy log `connected to dc.rsmb.test:445` | no request |
| through port 1080, `dc.rsmb.test`, TGT-only, **relay** (KDC = `dc.rsmb.test`, dialled through the proxy by name) | works | the same 2 | 1 request **from 10.213.45.20** (the proxy); proxy log `connected to dc.rsmb.test:88` and `connected to dc.rsmb.test:445` |
| through port 1080, `dc.rsmb.test`, TGT-only, **the crate's own KDC client** pointed at 10.213.45.12 | works | the same 2 | 2 requests **from 10.213.45.1**: around the proxy. This is what the crate does when left alone |
| `-H DC.RSMB.TEST` (upper case), cache with `cifs/dc.rsmb.test` | works, no KDC request | | the lookup ignores case |
| SPN `cifs/127.0.0.1`, cache with that ticket | works | | |
| a share with `smb encrypt = required` | works: 11 encrypted frames, listing, reads, write, delete | | |

Signing: the DC requires it (`signing_required=true`); the session signs with AES-GMAC and every
request of the probe was accepted, the write and the delete included. Timings at the fast
endpoint (one run each, a second login on a warm server): NEGOTIATE 7.6–9.0 ms (the DC forks per
connection), the Kerberos step 4.7–6.6 ms with a service ticket, 9.7–10.9 ms with the relay and
a TGT, 12.9–13.4 ms with the crate's own client. The very first Kerberos login after the DC
starts took 50 ms.

Round trips of a login, counted from the messages (no latency was put in front of the DC, 10
assumption 3): TCP connect, NEGOTIATE and SESSION_SETUP are 3 with a service ticket in the cache
(NTLM: 4). With a TGT only, the relay adds a TCP connect to the KDC and one exchange: 5. The
crate's own client would add one UDP exchange more against this KDC: 6.

### 2.4 Which ticket-granting tickets the crate can use

The crate always asks the KDC for a forwardable, renewable service ticket (2.1). Heimdal answers
that by the rules of RFC 4120:

| TGT made with | Flags (`klist -f`) | TGS exchange for `cifs/10.213.45.12` |
|---|---|---|
| MIT `kinit` (no option; `forwardable` not set in `krb5.conf`) | `RIA`: renewable, not forwardable | **refused**: KRB-ERROR 13 (`KDC_ERR_BADOPTION`), KDC log `reason=Bad request for forwardable ticket`. The crate: `Auth { message: "Kerberos TGS exchange failed: KRB-ERROR code 13 ()" }` after 4 ms |
| MIT `kinit -F -r 1d` | `RIA` | refused, the same |
| MIT `kinit -f` | `FRIA`: forwardable and renewable (this KDC makes every MIT-requested TGT renewable) | **works** |
| MIT `kinit -f -p` | `FPRIA` | works |
| Heimdal `kinit` (its default) | forwardable, **not renewable** | **refused**: KRB-ERROR 13, `reason=Bad request for renewable ticket` |
| any of them, with the service ticket already in the cache (`kvno`, `kgetcred`) | | works: the KDC is not asked |

So **a TGT-only login needs a TGT that is forwardable and renewable**. Many distributions set
`forwardable = true` in `/etc/krb5.conf`; MIT's own default is false. smbclient-ng (impacket)
sends the same four options, so it has the same rule where the KDC enforces it; against this KDC
it does worse (2.9). Whether a Windows KDC enforces the rule was not measured (10, assumption 1).
What rsmb does about it is decision K2.

### 2.5 Reaching the KDC: the crate alone and the relay

A TGT-only login whose SMB server answers (`samba`, 10.213.45.10) and whose KDC is varied:

| KDC | The crate's own client | The relay (one TCP exchange through rsmb's dialer, 10 s for all of it) |
|---|---|---|
| the DC | 2 requests (UDP, then TCP), 12.9 ms | 1 request, 9.7 ms |
| port 88 closed (10.213.45.10) | **36.0 s**, then `Io(Os { code: 111, kind: ConnectionRefused })`, which `classify` would report as `connection lost` | **4 ms**: `connect to 10.213.45.10:88: Connection refused (os error 111)` |
| silent (the DC paused: TCP is accepted, nothing answers) | **66.0 s**, then `Timeout`, which `classify` would report as "no complete reply within the response deadline; on a slow link raise --timeout" | **10.0 s**: `no answer within 10 s` |
| address absent (10.213.45.99) | not run (33 s of UDP first) | 3.05 s: `No route to host` |
| reached through the proxy | not possible: the crate dials itself (2.3, sixth row) | from the proxy's address, by name (2.3, fifth row) |

The relay with a limit of 12 s against the silent KDC ended at 12.0 s although the crate had
sent its request a second time after 11 s: the relay's error ends the login, the crate's retries
do not matter.

Three ways to send the KDC exchange through the proxy were weighed (the orchestrator's list):

| Way | Size | Works? | Verdict |
|---|---|---|---|
| **1. A loopback relay.** The crate is given `127.0.0.1:<port>` as the KDC; rsmb holds a UDP socket there, takes the crate's datagram, sends it over TCP (4-byte length) through `open_tcp` and hands the answer back as a datagram | about 45 lines in the probe | **yes, measured** (2.3, 2.5). The crate's UDP-first costs nothing, because the relay *is* the UDP peer: no wait, no fallback to TCP inside the crate | **chosen** (K1) |
| 2. rsmb runs the TGS exchange itself and builds a `CCache` with the service ticket | the crate's `encode_tgs_req_body` has the options fixed and its Kerberos encryption is `pub(crate)` with a private HMAC: rsmb would need its own KDC-REQ encoder (about 150 lines of DER), its own AES-CTS-HMAC-SHA1 and RC4 wrappers (two new direct dependencies or a SHA-1 of its own), and the reply's decryption: about 300 lines of Kerberos | not built | rejected for this stage: it is a second Kerberos implementation beside the crate's. It is the only way to lift the rule of 2.4, and is named in K2 as what to build if the user wants that |
| 3. A `[patch]` of the crate that adds a transport hook to `send_to_kdc` | a copy of the crate in the repository (4.3 MB of source) or a fork on a hosting service; about 20 changed lines | not built | rejected: it ends the exact pin (`docs/development.md`, "The `smb2` pin"), makes `cargo install --git` build a private copy of the library, and every later crate upgrade a merge |

### 2.6 What each failure returns

| Case | The crate's error, or what the relay saw | Time |
|---|---|---|
| the cache has no ticket for the realm asked for | `Auth { message: "ccache has no TGT or service ticket for realm OTHER.TEST" }`, before any request | 0 ms |
| an SPN the realm does not know (`cifs/nosuch.rsmb.test`; `cifs/10.213.45.10`) | KRB-ERROR **7** (`KDC_ERR_S_PRINCIPAL_UNKNOWN`), `sname` = the SPN | 1.4 ms |
| TGT not forwardable or not renewable | KRB-ERROR **13** (2.4) | 4 ms |
| TGT expired | KRB-ERROR **32** (`KRB_AP_ERR_TKT_EXPIRED`) | 3 ms |
| the user name given to the crate is not the ticket's client (`other`), or the same name in another case (`TESTER`), or the realm in lower case, with a service ticket | the server: `Protocol { status: STATUS_LOGON_FAILURE, command: SessionSetup }` | 3 ms |
| the same with a TGT | KRB-ERROR **36** (`KRB_AP_ERR_BADMATCH`) | 1 ms |
| a valid ticket presented to a server that does no Kerberos (`samba`, standalone; SPN `cifs/10.213.45.10` added to the realm for this) | `Protocol { status: STATUS_INVALID_PARAMETER, command: SessionSetup }`, kind `Other` | 0 ms |
| a service ticket that expired more than five minutes ago | the same `STATUS_INVALID_PARAMETER`; the server's log: `ticket is expired, possible clock skew` | 3 ms |
| a service ticket that expired 13 s ago | **the login succeeds**, and the first request after it fails: `SessionExpired` ("Session expired and reauthentication failed") | |
| KDC refused, silent, absent | 2.5 | |

Two things follow. The user and the realm must go to the crate **exactly as the cache spells
them**; `-u` and `-d` can only be compared. And the server's answers to a bad ticket say nothing
a person can act on (`STATUS_INVALID_PARAMETER` for an expired ticket and for a server outside
the realm alike): rsmb has to check the cache itself and word these cases itself.

### 2.7 A ticket's end is the session's end

The server gives a Kerberos session the lifetime of its ticket. Measured with a ticket of five
minutes: 13 s after its end time a new login with it still succeeded (the server allows five
minutes of clock difference), and every request on that session was answered
`STATUS_NETWORK_SESSION_EXPIRED`, which the crate reports as `SessionExpired`. After five more
minutes the login itself was refused (2.6). For rsmb this means: a session that outlives its
ticket fails its next command with `connection_lost` (today's mapping of `SessionExpired`), the
connection is marked dead, and the next command logs in again, reading the cache again. The
existing reconnect path is the right one; only the message is wrong (5.9).

### 2.8 What caches look like

| Written by | Version | Default principal | Service entries | Other |
|---|---|---|---|---|
| MIT `kinit` and `kvno` 1.22.3 | 0x0504 | `tester@RSMB.TEST`, name type 1 | `cifs/dc.rsmb.test@RSMB.TEST`, `cifs/10.213.45.12@RSMB.TEST` (name type 1, the realm of the KDC, the host as it was asked for) | two configuration entries with the server realm `X-CACHECONF:` and end time 0; all tickets end when the TGT ends |
| Heimdal `kinit` and `kgetcred` 7.8.0 | 0x0504 | the same | the same form | TGT with `renew_till` 0 |

Both match the crate's lookup for `-H dc.rsmb.test`, `-H DC.RSMB.TEST` and `-H 10.213.45.12`.
A cache holds the host as it was asked for: a ticket for `cifs/dc.rsmb.test` is not found for
`-H 10.213.45.12`, and rsmb then asks the KDC (or fails without a TGT).

Files that are not caches, given to `parse_ccache`:

| File | Result |
|---|---|
| empty; 300 random bytes; text; a version 0x0502 | `InvalidData`: `file too short for version`, `unsupported version 0x…` |
| a real cache cut after 40 bytes | `InvalidData`: `unexpected end of data` |
| a real cache cut after 700 bytes (inside the first ticket) | **accepted**, with the entries before the cut (an error inside an entry is taken as the end of the file) |
| 20 bytes: a valid version and header, then a principal that claims 4,294,967,295 name parts | **the process aborts**: `memory allocation of 103079215080 bytes failed`, exit 134 (`Vec::with_capacity` on a count read from the file) |

### 2.9 smbclient-ng with the same caches

`smbclientng -k --ccache-file <cache> --kdcHost 10.213.45.12 -H 10.213.45.12 -d RSMB.TEST -u
tester`: with the cache that holds the service ticket it logs in. With any TGT-only cache (MIT
with and without `-f`, Heimdal) it fails: `Kerberos SessionError: KRB_AP_ERR_INAPP_CKSUM`; the
KDC logs `TGS-REQ KRB5KRB_AP_ERR_INAPP_CKSUM` (impacket's TGS request carries no checksum, which
Heimdal demands). So against a Samba KDC rsmb with the crate does more than the tool it replaces;
against a Windows KDC smbclient-ng is known to work with a TGT, and rsmb was not measured.

### 2.10 What the stage adds to the gate

| What | Measured or counted |
|---|---|
| the DC in `up.sh` | 0.7 to 1.4 s to healthy, beside the other containers (not counted time) |
| one cache from the DC (`docker exec … mkcache`) | 102 to 125 ms |
| a Kerberos login and a few commands | 30 to 170 ms per process |
| the in-container `kinit` and `smbclient` of `check.sh` | 112 ms |
| the tests that wait: a silent KDC | 10 s (the relay's limit), in `tests/serial` |
| the planned `check.sh` assertions (6.3), by hand against the prototype | all hold: the wrong password is `NT_STATUS_LOGON_FAILURE`, port 88 opens from the host, the four `kvno` succeed and the unknown one fails, `mkcache` output starts `05 04` |
| the estimate | 7.2: about 26 s more, 482 of 600 |

### 2.11 Measured after the review

The prototype DC was built and started once more, with a second network alias `rsmb.test` and a
fourth user; removed again afterwards.

| What | Measured |
|---|---|
| TGT-only through port 1080, `-H dc.rsmb.test`, the relay's KDC given as **`rsmb.test`** (the realm's name in lower case) | works. KDC log: 1 request, from 10.213.45.20. Proxy log: `connected to rsmb.test:88` and `connected to dc.rsmb.test:445`. `getent hosts rsmb.test` on the host: nothing |
| the same KDC name without a proxy | `connect to rsmb.test:88: failed to lookup address information: Name or service not known` after 4 ms; no request at the KDC. A direct TGT-only test therefore names the KDC in a `krb5.conf` of its own |
| direct **through a tap** as 127.0.0.1 (SPN `cifs/127.0.0.1`), TGT-only, KDC 10.213.45.12 | works. **The tap: 1 connection, NEGOTIATE 1, SESSION_SETUP 1, 2 requests in 2 flights**; the KDC logged 1 request from 10.213.45.1 for `cifs/127.0.0.1`. The KDC's connection goes beside the tap |
| the host's `smbclient` 4.25.1 with `KRB5_CONFIG=/dev/null` and `--use-kerberos=off`, `RSMB\tester%wrong` at 10.213.45.12 | `NT_STATUS_LOGON_FAILURE`; with the right password a listing |
| `cargo clippy --workspace --all-targets -- -D warnings` after a change in `src/session.rs`, and after one in `src/` and the harness (on a copy of the sources in the scratch directory, dependencies checked before) | 0.28 s and 0.30 s: clippy after a code change costs under a second of counted time |
| `/etc/hostname` on the host | owned by uid 0, readable by all: a file of another owner for F73 |
| the image with a fourth user (`kother`) | builds |

## 3. Decisions

| # | Point | Decision | Reason |
|---|---|---|---|
| K1 | How the KDC exchange is sent | **The relay (5.6), always**: direct and through the proxy, one path, one TCP exchange through `open_tcp` | Measured (2.5): it is the only one of the three ways that needs neither a second Kerberos implementation nor a patched crate; it removes the crate's 36 s and 66 s; it saves the UDP round trip this KDC wastes; its errors are rsmb's own |
| K2 | A TGT that is not forwardable and renewable | **Accepted as a limit of the crate, with a message that says what to do** (5.7, error 13): `kinit -f`, or put the service ticket into the cache with `kvno cifs/<host>`. Documented in `help all`, `docs/commands.md` and `docs/known-limitations.md`; `docs/library-decision.md` records it as a crate defect to report upstream. **Confirmed by the user on 2026-11-18**: it stays a documented limit with a clear message. Lifting it would mean rsmb builds the TGS request itself (way 2 of 2.5, about 300 lines and its own Kerberos encryption); this plan does not include it | 2.4. The case the user named (smbclient-ng under proxychains) sends the same options; the fix is a second Kerberos client inside rsmb, which AGENTS.md rule 5 weighs against |
| K3 | Kind and exit code | Two kinds, by what the caller can do (finding 2). **`auth`**: the cache (missing, unreadable, another user's, another type, not a cache, no ticket, expired, another principal), a KRB-ERROR of the KDC, the server refusing the ticket, a ticket or a KDC answer the crate cannot use. **`connect`**: the KDC could not be asked: the dial to it failed (direct, or at the proxy), it did not answer within 10 s, or it closed the connection. Exit 3 at start-up, 1 when a reconnect fails. `-k` with `-p` or `--no-pass` is `usage`, exit 2. A dial of the SMB server that fails stays `connect`; a lost connection stays `connection_lost`. No new kind: 15 as before | The rule the documents already state: a proxy failure is `connect` (`docs/commands.md`, "Proxy"), a failed reconnect is "`connect` when nothing answers, `auth` when the login is refused" (`docs/commands.md`, `docs/scripting.md`). A script retries the one and asks for new credentials on the other. Every message starts `Kerberos: ` |
| K4 | Where the KDC is | The first `kdc` of the realm in the first file of `KRB5_CONFIG` (a colon-separated list) that has one, or in `/etc/krb5.conf` when the variable is not set; **otherwise the realm's own DNS name, `<realm in lower case>`, port 88** (finding 1), resolved by the proxy when there is one and on the host when there is none. One KDC, no SRV lookup, no `include`, no flag | In Active Directory the domain's name resolves to its domain controllers, and the file server is usually not one; smbclient-ng (impacket) without `--kdcHost` connects to the domain's name too. Measured through the proxy (2.11). A message about a KDC that could not be asked says where its address came from and points at `KRB5_CONFIG` with a file of the user's own (`/etc/krb5.conf` needs root) |
| K5 | Who the session is | User and realm are the cache's default principal, as spelled there. `-u` and `-d`, if given, must equal them without regard to ASCII case. **After the first login the session keeps that principal**: a reconnect that finds another principal in the cache fails and names both | 2.6: the crate needs the cache's spelling. A session must not change identity on a reconnect |
| K6 | Expiry | rsmb removes every entry whose end time is not after now (this host's clock, no allowance) before the crate sees the cache; then a service ticket, else a TGT, else an error that says "expired" with the time when an expired candidate was removed | 2.1: the crate takes an expired entry. 2.6, 2.7: the server answers an expired ticket with `STATUS_INVALID_PARAMETER`, or accepts it and expires the session at once |
| K7 | How long the KDC may take | The dial's 10 s (`DIAL_TIMEOUT`) for the whole exchange: connect, with a proxy its handshake, request, answer. Fixed, like the dial (Stage 4, V1) | 2.5: 10.0 s measured. The KDC exchange is two round trips |
| K8 | The cache file | `KRB5CCNAME`: `FILE:<path>` or a path. Unset or empty: `/tmp/krb5cc_<uid>`. Another type (`KCM:`, `KEYRING:`, `DIR:`, `MEMORY:`, `API:`, any `<letters>:` that is not a path) is an error naming the type and how to get a file. Read with `std::fs::read` at every login; `default_ccache_name` of `krb5.conf` is not read | The binding design. A host whose default is KCM or KEYRING gets the message for the missing default file, which names both |
| K9 | The service ticket rsmb obtained | Used for that login and dropped: the crate does not hand it out, and rsmb never writes a cache. Every login with a TGT-only cache asks the KDC once | 2.1 |
| K10 | The extra round trip of a TGT-only login | The TCP connect to the KDC happens when the crate asks, after NEGOTIATE; it is not opened ahead beside the SMB dial | One round trip per login, saved only by a second code path; whoever counts round trips puts the service ticket into the cache (3 round trips instead of NTLM's 4) |
| K11 | A session that outlives its ticket | Nothing new: the next command fails with `connection_lost` and a message that names the ticket (5.9), the one after it logs in again with the cache as it then is. A read-only command is run again at once, as after any closed connection | 2.7 |
| K12 | The loopback socket | UDP on 127.0.0.1, a port the kernel picks, open only while a TGT-only login runs. What arrives there is forwarded to the KDC; an answer is only of use to the crate if it decrypts with the TGT's key. **Corrected in the review (finding 1):** only the first sender is served; and the crate's *own* socket binds `0.0.0.0:0` (V10) | 5.6; stated in `docs/known-limitations.md` |
| K13 | A cache file that makes the crate's parser abort | Not guarded. The process ends with exit 134 and the allocator's line on stderr, without an `error` object, under `--json` too; documented as a crate limit and recorded for upstream | 2.8. With K18 the file is the user's own. A check before `parse_ccache` that is complete has to walk every entry's two principals, which is a second cache parser; checking the first principal alone would leave the same hole one entry further on (finding 13) |
| K14 | Where the tests go | Logins that work both ways go into `tests/fast.rs` behind `dc()`, which is the DC by address in the direct run and the DC by name through the proxy in the proxied run (7.1); what proves the KDC's side goes into `tests/slow.rs`; what pauses the DC or counts the proxy's log exactly into `tests/serial.rs`. No new test binary | The second run of `tests/fast` exists to prove "the same through the proxy" |
| K15 | The tap and Kerberos | A tapped session **through the proxy** uses a cache that holds the service ticket for `cifs/dc.rsmb.test`: there the KDC's connection would pass the tap (in SOCKS5 mode) as a second connection whose frames it cannot read. A tapped session **direct** to the DC (`-H 127.0.0.1`, SPN `cifs/127.0.0.1`) may be TGT-only: with `KRB5_CONFIG` naming `kdc = 10.213.45.12` the KDC's connection goes beside the tap, and "a TGT-only login is 2 SMB requests in 2 flights on 1 connection" is asserted (F71) | finding 15; measured (2.11) |
| K16 | Code | `src/session.rs` (all of it but the flag), `src/main.rs` (flags), `src/commands.rs` (manual). No new file, no new dependency (`libc::getuid` and tokio's `net` feature, which has UDP, are there) | |
| K17 | Old command lines that pass `-k` for keep-going | **Their meaning changes, and not always loudly.** With `-p` or `--no-pass` clap refuses the pair (exit 2). Without either (an anonymous run such as `rsmb -H h -N -k -C ls`, or a run that waited for the password prompt at a terminal) the same line is now a Kerberos login as the principal of the default cache: an `auth` error when there is no cache, **a login as that principal when there is one**. Stated in the README, in `docs/commands.md` (the flag row and "Differences from smbclient-ng") and in `docs/scripting.md` | finding 7: `src/main.rs` asks for no password when the user name is empty. The user decided the letter; the documents say what it does to an old script |
| K18 | Whose cache | The file must belong to the user rsmb runs as: `fstat` on the opened file against `getuid`. Another owner is an `auth` error naming both ids, before the file is parsed | finding 13: the default file lies in world-writable `/tmp`; a cache put there by another local user would log the session in as that user's principal, and nothing under `--json` would show it |

## 4. Departures from the design given by the orchestrator

| # | Design given | Here | Why |
|---|---|---|---|
| D1 | Point 4: with `--proxy` rsmb never contacts a KDC and refuses a TGT-only cache | **The KDC exchange goes through the proxy**: the relay dials the KDC with `open_tcp`, so through the proxy and by name. A TGT-only cache works through the proxy | Changed by the user while this plan was written (brief, amendment 9: "Through `--proxy` it must work too, including the exchange with the KDC"). Measured: 2.3 row 5, the KDC saw the proxy's address only |
| D2 | Point 4: the crate contacts the KDC; rsmb decides what a silent KDC costs | The crate never opens a socket to a KDC: it is always given the relay's loopback address, without a proxy too | K1. One way to reach a KDC; 10 s instead of 66 s; rsmb's own messages |
| D3 | Point 4: KDC failures need their own messages where `classify` says "connection lost" | They come from the relay, not from `classify`; `classify` gains one message (`SessionExpired`) and nothing else | D2 |
| D4 | Point 2: rsmb checks the ticket's end time "instead of passing an expired ticket on" | rsmb removes expired entries from the parsed cache and lets the crate choose among the rest | 2.1: the crate takes the first match, expired or not; a cache can hold an expired service ticket beside a valid TGT |
| D5 | Point 2: `-u`/`-d` must match the cache | and after the first login the session is pinned to the cache's principal | K5 |
| D6 | Point 7: cases "TGT-only through the proxy: refused, the KDC sees no request" | "TGT-only through the proxy: works; the KDC sees one request, from the proxy's address, while the KDC is reachable directly from the host" | D1 |
| D7 | Point 7: "an extra SPN for its IP" | three: `cifs/10.213.45.12`, `cifs/127.0.0.1` (a tap), `cifs/10.213.45.10` (so that a valid ticket can be presented to a server that does no Kerberos) | K15; 2.6 |
| D8 | Point 7: expired ticket as a test case | the test makes it from a valid cache by rewriting the end times in a copy; no expired ticket comes from the KDC | 2.2: nothing below 121 s can be issued, and the server accepts for 5 minutes more |
| D9 | Not in the design | a TGT must be forwardable and renewable (K2); a ticket's end ends the session (K11) | 2.4, 2.7: found by measuring |
| D10 | Point 7: the DC needs nothing special | no `NET_ADMIN` and no connection counter in the DC: the tests count in their taps and read the KDC's log | 2.2: the log line names the source address |
| D11 | Point 4: without a `kdc` in `krb5.conf` the KDC is `-H` | it is the realm's DNS name, `<realm in lower case>:88` | K4, finding 1 |
| D12 | Point 4: every KDC failure is kind `auth` | a KDC that could not be asked (dial, silence, closed connection) is `connect`; a KDC that answers with an error is `auth` | K3, finding 2 |
| D13 | Point 2: rsmb reads the cache file | and refuses one that another user owns | K18, finding 13 |

Nothing else departs: one code path in `login` for start-up and reconnect, the cache read at
every login, SPN `cifs/<-H as given>`, the banner, no flag for the KDC or the cache, the 15
kinds, the exit codes, all use of `smb2` in `src/session.rs`, every proxy rule of Stage 4.

## 5. Design

### 5.1 Files, dependencies

No new dependency. `Cargo.toml` is unchanged.

| File | Stage 5 changes |
|---|---|
| `src/main.rs` | `-k, --kerberos`; `--keep-going` without a short form; the password logic; `Target.kerberos`; the comment in `run_blocking` |
| `src/session.rs` | `Target.kerberos`; `ccache_path`, `Ticket`, `read_ticket`, `pick_ticket`, `krb5_kdc`, `krb5_files`, `kdc_for`, `kdc_error`, `kdc_relay`, `kerberos_setup`, `kerberos_refusal`; `login` returns who logged in; `classify` (one message); `banner`; `Session::connect`; unit tests |
| `src/commands.rs` | synopsis, the topic `kerberos`, the examples topic, every `-k` in a text, the exit-code text, the tests of the manual |
| `tests/common/mod.rs`, `tests/fast.rs`, `tests/slow.rs`, `tests/serial.rs`, `test.sh` | section 7 |
| `testenv/samba-dc/` (new), `testenv/compose.yaml`, `check.sh`, `ctl.sh`, `up.sh`, `bench-rsmb.sh` | section 6 |

### 5.2 Flags and usage checks (`src/main.rs`)

```rust
/// Log in with Kerberos, using the ticket cache (KRB5CCNAME, or /tmp/krb5cc_<uid>): no
/// password, never a prompt; user and realm come from the cache (-u and -d, if given, must
/// match it)
#[arg(short = 'k', long, conflicts_with_all = ["password", "no_pass"])]
kerberos: bool,
/// Do not stop at the first failing command; exit with the first failure's code
#[arg(long = "keep-going")]
keep_going: bool,
```

- `-k -p x` and `-k --no-pass` are refused by clap: exit 2, and with `--json` the `error` object
  of kind `usage` that `main` already makes of a clap error.
- With `-k` the password is the empty string, `rpassword` is never called and "no password: give
  -p or --no-pass" is never said, whatever stdin is.
- `-u` and `-d` keep their parsing (`REALM\user` is split when `-d` is absent). With `-k` an
  empty `-u` means "the cache's principal", not anonymous.
- A line that used `-k` for keep-going and had neither `-p` nor `--no-pass` is now a Kerberos
  login (K17). No code treats that case; the documents say it.
- `Target { …, kerberos: args.kerberos }`.
- The help of `-u` gains "; with -k: optional, must match the ticket cache" and the help of `-d`
  "; with -k: the realm".
- Nothing about the cache is checked in `main`: `login` reads it before it dials (5.5), which is
  before anything touches the network, and the same code runs on a reconnect.

### 5.3 The cache: where, whose, and what it must hold

```rust
#[derive(Clone, Debug)]
pub struct Target {
    // … as now …
    /// `-k`. `user` and `domain` then hold what `-u` and `-d` gave (empty: take the cache's)
    /// and, after the first login, the cache's principal.
    pub kerberos: bool,
}

/// The cache file, and whether it is the default one: `var` is KRB5CCNAME. `FILE:<path>` or a
/// path; None or empty: `/tmp/krb5cc_<uid>`. Another cache type is an error that names it.
fn ccache_path(var: Option<&str>, uid: u32) -> Result<(PathBuf, bool)>

/// What one Kerberos login takes from the cache.
struct Ticket {
    /// The parsed cache without its expired entries.
    cache: CCache,
    /// The default principal, as the cache spells it.
    user: String,
    realm: String,
    /// The cache holds a ticket for `cifs/<host>`: no KDC is asked.
    service: bool,
}

/// Reads the cache for a login at `t.host`: KRB5CCNAME, the file (which must be ours),
/// `parse_ccache`, then `pick_ticket` with the clock. No network.
fn read_ticket(t: &Target) -> Result<Ticket>

/// The pure part: the principal and its match with `t.user` and `t.domain`, expiry, which
/// ticket. `default`: the path is the default file (KRB5CCNAME was not set).
fn pick_ticket(cache: CCache, t: &Target, now: u64, path: &str, default: bool) -> Result<Ticket>
```

`ccache_path`:

| `KRB5CCNAME` | Result |
|---|---|
| not set, or empty | `/tmp/krb5cc_<uid>` (`libc::getuid()`), default |
| `FILE:/tmp/x` | `/tmp/x` |
| begins with `/` or `.`, or holds no `:` | the value as a path (`/tmp/a:b` is a path) |
| `<letters or _>:<rest>`, anything else | error: `Kerberos: KRB5CCNAME names a ticket cache of type '<TYPE>'; rsmb reads a cache file only. Put the tickets into a file and name it: kinit -c /tmp/krb5cc_rsmb <user>; KRB5CCNAME=/tmp/krb5cc_rsmb` |

**The hint** (finding 14), appended to three messages below when the path is the default:
` (KRB5CCNAME is not set, so the default file <path> was used; if kinit writes somewhere else,
for example to a KCM or KEYRING cache, set KRB5CCNAME to a cache file: kinit -c <file>)`. A user
whose `kinit` fills a KCM cache would otherwise run `kinit` again and again against a stale file.

`read_ticket`, each error `Kind::Auth`, each message one line that starts `Kerberos: `:

| Step | Failure | Message after `Kerberos: ` |
|---|---|---|
| `File::open(path)`, later `read_to_end` | any | `ticket cache <path>: <io error>`, and the hint |
| `file.metadata()?.uid()` against `libc::getuid()` (K18; on the opened file, so the check and the read see the same file) | another owner | `ticket cache <path> belongs to uid <n>, not to the user rsmb runs as (uid <m>); rsmb uses a cache of its own user only` |
| `parse_ccache` | `Err` | `<path> is not a ticket cache rsmb can read (<the crate's text without "Invalid data: ">)` |

A file for which the crate's parser asks for an absurd allocation ends the process there (K13).

`pick_ticket`, in this order:

1. `user` = the default principal's one name part, `realm` = its realm. More than one part
   (`host/name`): `the ticket cache <path> belongs to '<a/b>@<realm>'; a principal with more
   than one name part is not supported` (the crate sends one part).
2. If `t.user` is not empty and differs from `user` without regard to ASCII case, or `t.domain`
   is not empty and differs from `realm` the same way: `the ticket cache <path> belongs to
   '<user>@<realm>', not to '<t.user>' in '<t.domain>' (given with -u and -d, or the principal
   this session logged in as). With -k leave -u and -d out, or give the cache's own` (`in
   '<t.domain>'` only when a domain was given). `-d` is compared with the **realm**: a NetBIOS
   domain name (`-d RSMB` for the realm `RSMB.TEST`) does not match and gets this message.
3. Candidates: entries whose server is `cifs/<t.host>@<realm>` or `krbtgt/<realm>@<realm>`
   (two parts, compared as the crate does). Remember the latest end time among candidates
   that are **not** after `now`, and its server name.
4. `cache.credentials.retain(|c| u64::from(c.endtime) > now)`. (This also drops the
   configuration entries, which no lookup uses.)
5. `service` = `cache.find_service_ticket("cifs", &t.host, &realm).is_some()`. If not, and
   `cache.find_tgt(&realm)` is none: with a remembered expired candidate `the ticket for
   <server> in <path> expired at <time>; run kinit`, otherwise `<path> holds no ticket for
   cifs/<host>@<realm> and no ticket-granting ticket for <realm>; run kinit <user>@<realm>`;
   both with the hint.

`<time>` is RFC 3339 from `commands::filetime(ft, true)`, which takes a **Windows FILETIME**
(100 ns since 1601), not Unix seconds (finding 10): `ft = (u64::from(end) + 11_644_473_600) *
10_000_000`. The unit test writes its expected string by hand for a fixed end time
(1,795,029,928 is `2026-11-18T19:25:28Z`), not through this conversion.

The host is compared as given with `-H`. Nothing is resolved or canonicalised: `-H` by address
needs a ticket, or an SPN in the realm, for that address (5.10).

### 5.4 The KDC's address

```rust
/// The realm's first `kdc` in the text of a krb5.conf: host and port (88 without one).
fn krb5_kdc(conf: &str, realm: &str) -> Option<(String, u16)>
/// The files to look in: `var` is KRB5_CONFIG. None or empty: `/etc/krb5.conf`; otherwise its
/// colon-separated parts, in order.
fn krb5_files(var: Option<&str>) -> Vec<PathBuf>
/// Where to ask for a service ticket: the first of `files` that names a kdc for `realm` (a
/// file that cannot be read counts as absent), with that file; otherwise the realm's own name
/// in lower case, port 88, and None.
fn kdc_for(files: &[PathBuf], realm: &str) -> (String, u16, Option<PathBuf>)
```

The environment is read in one place, `kerberos_setup` (`krb5_files(std::env::var(
"KRB5_CONFIG").ok().as_deref())`), so both functions are tested without it (finding 12): the
unset branch gives `/etc/krb5.conf` as a value, and `kdc_for` is given files the test wrote.

`krb5_kdc` reads line by line and **counts braces** (finding 11): a line `[realms]` opens the
section and any other `[…]` line at depth 0 closes it; inside, a line `<realm> = {` at depth 0
(the realm compared exactly) opens the realm's block, depth 1; any further line that ends in
`{` goes one deeper (`auth_to_local_names = {`), a line `}` one up; a `kdc = <value>` counts
only at depth 1 of the wanted realm, and the first one wins. `#` and `;` start a comment line;
blanks around names and values are dropped.

`<value>`: a Heimdal prefix `tcp/` or `udp/` is dropped (rsmb always uses TCP); a value that
starts `http` or `kkdcp` is skipped. Then: `[v6]:port` and `[v6]`; a value with **more than one
`:` and no brackets is an IPv6 address** and has no port (`2001:db8::1`); `host:port`; `host`.
Not read: `include`, `includedir`, a realm written on one line with its braces,
`[domain_realm]`, `dns_lookup_kdc`. Only called when a TGT must be used.

Without a `kdc` in any file the KDC is the realm's name in lower case (`RSMB.TEST` gives
`rsmb.test:88`; K4). rsmb hands that name to `open_tcp` like any host: the proxy resolves it
when there is one, the host otherwise. A realm name longer than 255 bytes cannot go through a
proxy; `open_tcp`'s error says so.

### 5.5 The branch in `login`

```rust
/// Dial, negotiate, log in (NTLM, or Kerberos with `t.kerberos`). Used at start-up and for
/// every reconnect. With Kerberos, also who the cache says we are.
async fn login(t: &Target) -> Result<(Connection, Option<(String, String)>)> {
    // Before the dial: a cache that cannot be used costs no connection.
    let ticket = if t.kerberos { Some(read_ticket(t)?) } else { None };
    let mut conn = dial(t).await?;
    // … timeouts and NEGOTIATE as now …
    match &ticket {
        None => { /* Session::setup as now */ }
        Some(k) => kerberos_setup(&mut conn, t, k).await?,
    }
    Ok((conn, ticket.map(|k| (k.user, k.realm))))
}

/// SESSION_SETUP with the cache's ticket; with a TGT only, the relay beside it.
async fn kerberos_setup(conn: &mut Connection, t: &Target, k: &Ticket) -> Result<()>
```

`kerberos_setup`:

1. `what` = `Kerberos login as '<realm>\<user>' at <hostport>`.
2. With `k.service`: `creds.kdc_address` is `"127.0.0.1:9"` (never used: the crate takes the
   service ticket), and `setup_kerberos_from_ccache(conn, &creds, &t.host, &k.cache)` is
   awaited alone.
3. Without: `kdc_for(&krb5_files(…KRB5_CONFIG…), &k.realm)` (5.4); a `tokio::net::UdpSocket` bound to `127.0.0.1:0`
   (failure: `Kind::LocalIo`, `Kerberos: cannot open a loopback socket: <e>`); `creds.kdc_address`
   is that socket's address; then

   ```rust
   tokio::select! {
       biased;
       e = kdc_relay(sock, kdc, …, DIAL_TIMEOUT) => Err(e),
       r = smb2::Session::setup_kerberos_from_ccache(conn, &creds, &t.host, &k.cache) => r.map_err(…),
   }
   ```

   The relay never ends well: it returns only its error, so the first branch ends the login
   exactly when the KDC exchange failed, whatever the crate is waiting for.
4. `KerberosCredentials { username: k.user, password: String::new(), realm: k.realm, kdc_address }`.
5. The crate's error from `setup_kerberos_from_ccache` (when the relay did not end the login
   first) goes through `kerberos_refusal`, which asks whether the server said it (finding 5):

   | The error | Result |
   |---|---|
   | a lost connection (`classify(e, what).is_lost()`) | as `classify` says |
   | `e.status()` is `Some` and `classify` calls it `auth` (`STATUS_LOGON_FAILURE`, …) | `classify`'s `<what>: login refused (…)` |
   | `e.status()` is `Some`, anything else (2.6: `STATUS_INVALID_PARAMETER`) | `Kind::Auth`, `<what>: the server did not accept the ticket (<status>); is <host> in the realm <realm>, and is the ticket valid on the server's clock?` |
   | no status: the server said nothing of the kind. The crate raised it itself, before sending or on what came back: an encryption type it does not know (`etype_from_code`), a ticket in the cache it cannot parse, a KDC answer it cannot decrypt or parse, a SESSION_SETUP answer that fails its signature | `Kind::Auth`, `<what>: the ticket in the cache or the KDC's answer could not be used (<the crate's text>)` |

   ```rust
   /// The crate's error for a Kerberos SESSION_SETUP, in our words (the table of 5.5).
   fn kerberos_refusal(e: smb2::Error, what: &str, host: &str, realm: &str) -> Error
   ```

`Session::connect(mut target)`: after `login`, `if let Some((u, r)) = who { target.user = u;
target.domain = r; }` (K5). `ensure_connected` takes the connection and ignores the name (it is
the same, or `pick_ticket` has failed). Nothing else of the session changes: trees, cache,
`abandon`, the rerun rule.

### 5.6 The relay

```rust
/// Carries the crate's KDC requests: each datagram that arrives on `sock` (a loopback socket
/// of ours, which the crate was given as the KDC's address) goes over TCP, with a 4-byte
/// length, through `open_tcp` to `kdc` (through the proxy when `kdc.proxy` is set), and the
/// answer goes back as a datagram. One exchange has `limit`. Serves until dropped; returns
/// only an error, which is the login's error.
async fn kdc_relay(sock: UdpSocket, kdc: Target, from: Option<&Path>, spn: &str, limit: Duration) -> Error
```

- `from` (the file the KDC's address came from, None: the realm's name) and `spn` are what its
  messages need (5.7); the implementer may pass them as one small struct.
- `kdc` is the login's `Target` with `host` and `port` replaced by the KDC's, so `open_tcp`
  does what it does for the server: `TCP_NODELAY`, the proxy with its login, the proxy
  resolving the name, **no fallback to a direct connection**.
- Per datagram, **one timer at a time** (finding 4):

  ```rust
  let t0 = Instant::now();
  let mut tcp = open_tcp(&kdc, limit).await?;      // its own limit, its own messages
  let left = limit.saturating_sub(t0.elapsed());
  let answer = tokio::time::timeout(left, exchange(&mut tcp, request)).await;   // write, read
  ```

  The dial is bounded by `open_tcp` alone and reports with `open_tcp`'s words (which already
  say whether the proxy answered); what is left of `limit` bounds the write and the read, and
  running out there has one message of its own. No outer timer races the dial's, so the same
  silence always gives the same message. `exchange` writes the length and the request in one
  write, reads 4 bytes and then the answer; an answer longer than 65,507 bytes is an error (it
  cannot go back as a datagram; a ticket that large does not fit a SESSION_SETUP either). A new
  TCP connection per datagram: the crate sends one, or the same one again after its own 10 s.
- An answer that starts with `0x7e` and parses with the crate's public `parse_krb_error` is a
  refusal: the relay returns `kdc_error(code, …)` (5.7) and does not hand it on. Anything else
  goes back to the sender's address with `send_to`.
- The loop goes on after an answer (the crate may ask again); the socket closes when the login
  ends and the future is dropped.
- The limit is `DIAL_TIMEOUT`, 10 s, equal to the crate's own wait for a datagram. Measured
  (2.5): the relay's error ends the login at 10.0 s; if the crate sent its request again a
  moment before, nothing is lost.
- The crate's request is at most a TGT and a few hundred bytes (1,451 bytes here). One that
  does not fit a loopback datagram (65,507 bytes) cannot be sent by the crate at all; such a
  ticket could not be used for SMB either.

### 5.7 The KDC's failures in our words

Every message: `Kerberos: asking the KDC <hostport> for a ticket for cifs/<host>: <reason>`.
The kind says whether the KDC was reached (K3).

| What happened | Kind | `<reason>` |
|---|---|---|
| `open_tcp` failed, its limit included | `connect` | its message as it is: `connect to <kdc>: Connection refused (os error 111)`, `connect to <kdc>: no answer within 10 s`, `connect to <kdc>: failed to lookup address information: …`, `proxy <proxy>: cannot reach <kdc>: host unreachable`, `proxy <proxy>: user name or password refused`, `proxy <proxy>: connected to the proxy, but the SOCKS5 handshake did not finish within 10 s (…)` |
| connected, and what was left of the 10 s passed | `connect` | `connected to the KDC, but it did not answer within 10 s` |
| the KDC closed the connection, or sent less than it announced | `connect` | `the KDC closed the connection without an answer` |
| KRB-ERROR 7 | `auth` | `the realm <realm> has no service cifs/<host>: give -H the name the server has in the realm` |
| KRB-ERROR 13 | `auth` | `refused (KDC_ERR_BADOPTION): rsmb asks for a forwardable, renewable ticket, and this KDC gives one only for a ticket-granting ticket that is both. Get one with kinit -f (add -r 7d if it is not renewable), or put the service ticket into the cache: kvno cifs/<host>` |
| KRB-ERROR 32 | `auth` | `the KDC says the ticket-granting ticket has expired; run kinit` |
| KRB-ERROR 37 | `auth` | `the clocks of this host and the KDC differ too much` |
| KRB-ERROR 36 | `auth` | `the KDC does not accept the ticket-granting ticket for '<user>@<realm>' (KRB_AP_ERR_BADMATCH)` |
| any other KRB-ERROR | `auth` | `KDC error <code>` and the error's text when it has one |
| a KRB-ERROR that does not parse | | handed to the crate like an answer; its own error then falls under 5.5 step 5 (no status: "could not be used") |

The three `connect` rows get a tail that says where the address came from and what to do, and
never sends the user to a file only root can write (finding 1):

- from a file: ` (the KDC is the one <file> names for <realm>)`;
- from the realm's name: ` (no kdc for <realm> in <the files looked at>, so the realm's name
  was tried. To name the KDC, write a file with the four lines "[realms]", "<realm> = {",
  "kdc = <host>", "}" and set KRB5_CONFIG to it; or put the service ticket into the cache:
  kvno cifs/<host>)`.

```rust
/// The reason for a KRB-ERROR `code` of the KDC (the table of 5.7).
fn kdc_error(code: i32, text: Option<&str>, host: &str, user: &str, realm: &str) -> String
```

### 5.8 What rsmb does not repair

A TGT that is not forwardable and renewable gets error 13 and its message (K2). rsmb does not
read the ticket's flags from the cache (the crate's parser drops them) and does not rewrite the
crate's request: the request body is covered by a checksum inside the encrypted authenticator.

### 5.9 A session whose ticket ends

`classify`: `K::SessionExpired` gets its own arm, still `Kind::ConnectionLost { silent: false }`:
`connection lost: the server ended the session (with -k: the ticket's lifetime is over)`. The
rest is today's path: `run_parsed` marks the connection dead, runs a read-only command again,
and that login reads the cache again: with new tickets it works, with the old ones `pick_ticket`
says `reconnect failed: Kerberos: the ticket for … expired at …; run kinit`.

### 5.10 The server's name

The SPN is `cifs/<-H as given>`; the crate builds it from the `server_hostname` rsmb passes
(`t.host`). Consequences, all documented, none with a flag:

- `-H <address>` works when the cache holds a ticket for `cifs/<address>` or the realm has that
  SPN; otherwise error 7 and its message.
- `-H` must be the name the server has in the realm (usually its full DNS name); a short name or
  a DNS alias is another SPN.
- Through a proxy the name goes to the proxy as given (Stage 4) and into the SPN as given.

### 5.11 Banner and `--json`

`banner`: ` (Kerberos)` after the user when `t.kerberos`: `logged in as 'RSMB.TEST\tester'
(Kerberos) on dc.rsmb.test:445 through proxy 10.213.45.20:1080 (SMB 3.1.1)`. `display_user` is
unchanged (after the first login `domain` is the realm and `user` the cache's name).

`--json`: no new object type and no new field. A Kerberos failure at start-up is the one
`error` object with `"kind":"auth"` (or `"connect"` when the KDC could not be asked, K3),
`"command":null` and the message, exit 3, no `done`; on a
reconnect it is the command's `error` with `reconnect failed: Kerberos: …`, exit 1.

### 5.12 The manual (`src/commands.rs`)

- Synopsis (finding 9): `rsmb -H <host> [-P <port>] [-k] [-u <user>] [-d <domain>] [-p
  <password> | --no-pass]`, and `[--keep-going]` where `[-k]` stood. The topic and the flag's
  help state that `-k` excludes `-p` and `--no-pass`, and that `-u` and `-d` may be given with
  it and must then match the cache.
- Every "unless -k" becomes "unless --keep-going"; "with -k (--keep-going)" becomes "with
  --keep-going"; "also with -k" becomes "also with --keep-going" (`EXIT_CODES` 1, the topic
  "giving commands", the JSON paragraph).
- A topic `kerberos` in `topics()`, after `proxy` (9 topics): `-k` logs in with the ticket cache
  and never asks for a password; it cannot be combined with `-p` or `--no-pass`; the cache is
  the file `KRB5CCNAME` names (`FILE:` or a path), else `/tmp/krb5cc_<uid>`, other cache types
  are not read, and the file must belong to the user rsmb runs as; user and realm are the
  cache's, `-u` and `-d` are optional and must match (`-d` is the realm); the ticket is for
  `cifs/<-H as given>`, so `-H` is the server's name in the realm; with that service ticket in
  the cache no KDC is asked (`kvno cifs/<host>` puts it there) and the login is 2 requests;
  with a ticket-granting ticket only, rsmb asks the KDC once per login, over TCP port 88, 10 s
  at most: the realm's `kdc` in the files of `KRB5_CONFIG`, or in `/etc/krb5.conf` when that is
  not set, else the realm's own name in lower case (`corp.example:88`); to name another KDC
  write a krb5.conf of your own and set `KRB5_CONFIG` to it; that ticket-granting ticket must
  be forwardable and renewable (`kinit -f`); through `--proxy` the KDC is reached through the
  proxy, by name, and nothing goes around it; an expired ticket is refused by rsmb with its end
  time; a Kerberos failure starts `Kerberos: ` and is kind `auth` (the cache, a refusal by the
  KDC or the server) or `connect` (the KDC could not be asked), exit 3 at start-up; a session
  ends when its ticket does, the next command logs in again and reads the cache again; rsmb
  never writes the cache and never falls back to a password; **`-k` used to be the short form
  of `--keep-going`**.
- The examples topic gains: `Kerberos: kinit -f alice@CORP.EXAMPLE; rsmb -k -H
  fs1.corp.example -N -C 'use docs' -C 'ls'`.
- `EXIT_CODES`, 3: "connect or login failed at start-up (a proxy or Kerberos failure included)".

### 5.13 What does not change

`open_tcp`, `dial`, `parse_proxy`, `proxy_error`, the reconnect and rerun rules, `abandon`, the
kinds and exit codes, the 19 commands, the cache of listings, walks, transfers, the prompt
(`//<-H as given>/…`), `smbtap`.

## 6. The test environment

### 6.1 The image `rsmb-samba-dc` (`testenv/samba-dc/`)

Four files; the forms below were built and run while planning (2.2; `dc-final/` in the scratch
directory holds them).

`Dockerfile`:

```dockerfile
# The Kerberos server: a Samba AD DC (Samba's own Heimdal KDC), provisioned at image build.
FROM alpine:3.24.4
# krb5: MIT kinit, kvno and klist, to make ticket caches inside the container.
RUN apk add --no-cache samba-dc samba-client krb5
# Provisioning writes its own smb.conf; ours replaces it afterwards. xattr_tdb keeps the NT ACLs
# in a file, because a build container may not set security.* extended attributes.
RUN rm -f /etc/samba/smb.conf \
    && { samba-tool domain provision --realm=RSMB.TEST --domain=RSMB --server-role=dc \
            --dns-backend=SAMBA_INTERNAL --host-name=dc --host-ip=10.213.45.12 \
            --adminpass='Adm1n-testpass' \
            --option='vfs objects = dfs_samba4 acl_xattr xattr_tdb' >/tmp/provision.log 2>&1 \
         || { cat /tmp/provision.log; exit 1; }; } \
    && rm /tmp/provision.log
COPY smb.conf /etc/samba/smb.conf
COPY krb5.conf /etc/krb5.conf
COPY mkcache /usr/local/bin/mkcache
RUN chmod +x /usr/local/bin/mkcache \
    && samba-tool domain passwordsettings set --complexity=off --min-pwd-length=1 \
        --min-pwd-age=0 --max-pwd-age=0 --history-length=0 >/dev/null \
    && for u in tester kdirect kproxy kother; do samba-tool user create "$u" testpass >/dev/null; done \
    && for spn in cifs/10.213.45.12 cifs/127.0.0.1 cifs/10.213.45.10; do \
           samba-tool spn add "$spn" 'DC$' >/dev/null; done \
    && mkdir -p /srv/share/sub /srv/share/upload && chmod 1777 /srv/share/upload \
    && echo 'kerberos works' > /srv/share/hello.txt \
    && seq 1 200000 | head -c 1048577 > /srv/share/sub/1m.bin \
    && testparm -s >/dev/null 2>&1
ENTRYPOINT ["samba", "--foreground", "--no-process-group", "--debug-stdout"]
```

`smb.conf`: `netbios name = DC`, `realm = RSMB.TEST`, `workgroup = RSMB`, `server role = active
directory domain controller`, `dns forwarder = 127.0.0.1`, `vfs objects = dfs_samba4 acl_xattr
xattr_tdb`, `disable netbios = yes`, `smb ports = 445`, no printers, `server multi channel
support = no` (a tap must see everything), **`log level = 1 kerberos:3`** (the request lines of
2.2); the shares `sysvol` and `netlogon` as provisioning wrote them, `[share]` (`/srv/share`,
writable) and `[encrypted]` (the same path, `smb encrypt = required`).

`krb5.conf` (for `kinit` and `kvno` inside the container only): `default_realm = RSMB.TEST`,
`dns_lookup_kdc = false`, `dns_lookup_realm = false`, `dns_canonicalize_hostname = false`,
`rdns = false`, and `RSMB.TEST = { kdc = 127.0.0.1 }` on three lines.

`mkcache`:

```sh
#!/bin/sh
# mkcache -f|-F <user> [<spn>...]: a new ticket cache for <user> (password testpass) with a
# ticket-granting ticket that is forwardable (-f) or not (-F), plus one service ticket per
# <spn>; writes the cache file's bytes to stdout. Used by the tests and by check.sh.
set -eu
c=$(mktemp -u /tmp/mkcache.XXXXXX)
trap 'rm -f "$c"' EXIT
flag=$1 user=$2
shift 2
echo testpass | kinit "$flag" -c "FILE:$c" "$user@RSMB.TEST" >/dev/null
[ $# -eq 0 ] || kvno -q -c "FILE:$c" "$@" >/dev/null
cat "$c"
```

What is in the realm, and why:

| Thing | For |
|---|---|
| user `tester` / `testpass` | every ordinary Kerberos test |
| users `kdirect`, `kproxy` / `testpass` | `kerberos_kdc_is_asked_through_the_proxy` only: lines of the KDC's log that name them belong to that test, whatever else runs (7.4) |
| user `kother` / `testpass` | F75 only: the cache of another principal put in a session's place (finding 8: `kdirect` and `kproxy` stay S71's alone) |
| SPN `cifs/10.213.45.12` | `-H` by address |
| SPN `cifs/127.0.0.1` | a tap in front of the DC |
| SPN `cifs/10.213.45.10` | a valid ticket for `samba`, which does no Kerberos: the refusal of 5.5 step 5 |
| `share`: `hello.txt` (`kerberos works` and a newline), `sub/1m.bin` (1,048,577 bytes, the numbers from `seq`), `upload/` (mode 1777) | a listing, a read with a checksum, a write. Files written over SMB belong to the id Samba maps the realm's user to, so a test's scratch directory is made with mode 1777, not owned by a Unix user |
| `encrypted` | encryption with a Kerberos session key |
| the admin password `Adm1n-testpass`, the realm's keys | throwaway values inside a local image; the keys are made at build and are in no file of the repository |

### 6.2 `compose.yaml`

```yaml
  samba-dc:
    build: samba-dc
    image: rsmb-samba-dc
    container_name: rsmb-samba-dc
    hostname: dc
    init: true
    healthcheck:
      # SMB (445 = 0x01BD) and the KDC (88 = 0x0058) in LISTEN state, without connecting.
      test: ["CMD-SHELL", "grep -q ':01BD 00000000:0000 0A' /proc/net/tcp && grep -q ':0058 00000000:0000 0A' /proc/net/tcp"]
      interval: 2s
      timeout: 5s
      retries: 2
      start_period: 60s
      start_interval: 1s
    networks:
      pub:
        ipv4_address: 10.213.45.12
        aliases: [dc.rsmb.test, rsmb.test]
```

No capability, no volume, no dependency on another service. The aliases make `dc.rsmb.test`
and `rsmb.test` resolve for `socks` (which is on `pub`) and for nobody on the host: the second
is the realm's own name, where rsmb looks for the KDC when no `krb5.conf` names one (K4), as a
domain's name resolves to its domain controllers. The header comment of the
file and the `pub` comment stay true.

### 6.3 `check.sh`, with tools that are not this project's code

**`export KRB5_CONFIG=/dev/null` at the top of the script** (finding 6): the host's `smbclient`
(4.25.1 here) tries Kerberos by default and would read the host's `/etc/krb5.conf`, which may
describe another realm; nothing in `check.sh` may depend on it. The new password check below
also passes `--use-kerberos=off`. The existing calls are unchanged otherwise.

A new section "Kerberos and the DC", after "SOCKS5 and the private server"; `rsmb-samba-dc`
joins the list of the first section, whose line becomes "all seven containers healthy".

| Check | How |
|---|---|
| a Kerberos login reads the share | in the container: `kinit` for `tester` into a temporary cache, then `smbclient -s /dev/null //dc.rsmb.test/share --use-kerberos=required -c 'get hello.txt /dev/stdout'` prints `kerberos works` |
| the SPNs exist | in the container, with that cache: `kvno cifs/10.213.45.12 cifs/127.0.0.1 cifs/10.213.45.10 cifs/dc.rsmb.test` succeeds; `kvno cifs/nosuch.rsmb.test` fails |
| `mkcache` gives a cache | `docker exec rsmb-samba-dc mkcache -f tester cifs/dc.rsmb.test` writes at least 1,000 bytes that start with `05 04` (`od`) |
| the KDC's log names the source of a request | before and after the `kvno` above, the number of lines `TGS-REQ tester@RSMB.TEST from ipv4:` in `docker logs rsmb-samba-dc` (captured first, then counted) rose by at least 4 |
| `dc.rsmb.test` and `rsmb.test` do not resolve on the host | `getent hosts` fails for both |
| the proxy resolves both and reaches the DC | `py socks $PROXY 1080 dc.rsmb.test` and `py socks $PROXY 1080 rsmb.test` print `smb2` (the helper connects to port 445; both names are the one container) |
| the KDC is reachable from the host without the proxy | `exec 3<>/dev/tcp/10.213.45.12/88` succeeds within 3 s. This is what makes "nothing went around the proxy" a claim that could fail (7.4, S71) |
| a password login is still refused where it must be | `smbclient -s /dev/null //10.213.45.12/share --use-kerberos=off -U 'RSMB\tester%wrong' -c ls` says `NT_STATUS_LOGON_FAILURE` (measured with `KRB5_CONFIG=/dev/null`, 2.11) |

About 1 s more. `check.sh` still holds the exclusive lock for its whole run.

### 6.4 `ctl.sh`, `up.sh`, `down.sh`, `test.sh`, `bench-rsmb.sh`

- `ctl.sh`: `pause` and `unpause` already take any service name; the usage comment gains
  `samba-dc` in the list for `pause`. `conns` and `evict` do not apply to the DC (no counter
  rule; the comment says so).
- `up.sh`: the message "from nothing: about 68 s" becomes the measured figure with the DC
  (about 130 s; the DC's build is 61 s of it, measured alone). The unpause line already covers
  every `rsmb-` container.
- `down.sh`: unchanged.
- `test.sh`, "stale test directories": `rsmb-samba-dc` joins the loop.
- `testenv/bench-rsmb.sh` line 261: `-k` becomes `--keep-going` (with `-p` beside it the old
  form would now exit 2).

## 7. Tests

### 7.1 The harness, `tests/common/mod.rs`

```rust
pub enum Ep { Fast, Slow, Edge, Bad, Proxied, ProxiedLogin, /* new: */ Dc, DcProxied }
/// The DC's name, which only the proxy resolves, and its address.
pub const DC_NAME: &str = "dc.rsmb.test";
pub const DC: &str = "10.213.45.12";
impl Ep {
    // addr(): Dc "10.213.45.12:445"; DcProxied "10.213.45.20:1080".
    // container(): both "rsmb-samba-dc". proxied(): DcProxied too.
    // route(): Dc as a direct endpoint; DcProxied `--proxy socks5://<proxy or tap> -H dc.rsmb.test -P 445`.
    // host(): DcProxied "dc.rsmb.test"; Dc "127.0.0.1" tapped, "10.213.45.12" not.
    // args(): for the two, `route(tap)` and `-k` (no user, no password).
    /// The SPN a session to this endpoint asks for: `cifs/127.0.0.1` (Dc, tapped),
    /// `cifs/10.213.45.12` (Dc, not tapped), `cifs/dc.rsmb.test` (DcProxied).
    pub fn spn(self, tapped: bool) -> &'static str
}
/// The Kerberos endpoint of tests/fast.rs: `Ep::Dc`, or `Ep::DcProxied` when RSMB_TEST_VIA=socks5.
pub fn dc() -> Ep
/// A new cache from the DC (`docker exec rsmb-samba-dc mkcache <flag> <user> <spns…>`), written
/// to `dir/<name>` with mode 0600. `flag` is "-f" or "-F".
pub fn krb_cache(dir: &Path, name: &str, flag: &str, user: &str, spns: &[&str]) -> PathBuf
/// A copy of `cache` in which every ticket ended an hour ago: each run of four big-endian
/// 32-bit times (auth, start, end, renew) that lies within three days of now gets `now - 3600`
/// as its end.
pub fn expired_copy(cache: &Path) -> PathBuf
/// A krb5.conf in `dir` that names `kdc` for RSMB.TEST; the path, for KRB5_CONFIG.
pub fn krb5_conf(dir: &Path, kdc: &str) -> PathBuf
/// Lines of the DC's log that are a TGS request of `user` from `addr`:
/// `TGS-REQ <user>@RSMB.TEST from ipv4:<addr>:`.
pub fn kdc_requests(user: &str, addr: &str) -> usize
impl Client {
    /// A tapped Kerberos session to `ep` (`Ep::Dc` or `Ep::DcProxied`) with KRB5CCNAME=`cache`.
    pub fn kerberos(ep: Ep, cache: &Path, extra: &[&str]) -> Client
}
```

- **Every rsmb process the harness starts gets `KRB5_CONFIG=/dev/null` and
  `KRB5CCNAME=/nonexistent/rsmb-test/krb5cc`** unless the test sets them (`run_env`, `run_in`,
  `Client::build`, `Pty::command`; `pub const NO_CACHE: &str` holds the path): no test can read
  the host's `/etc/krb5.conf`, the developer's `/tmp/krb5cc_<uid>` or a `KCM:` the developer's
  shell exports (finding 3). `Client::build` and `Pty::command` gain an `env` parameter (a
  slice, as `run_env` has).
- A tapped Kerberos session through the proxy uses a cache with the service ticket for
  `ep.spn(true)`; a tapped session direct to the DC may be TGT-only when `KRB5_CONFIG` is
  `krb5_conf(dir, "10.213.45.12")` (K15).
- **A direct TGT-only run always names the KDC with `krb5_conf(dir, "10.213.45.12")`**: without
  it rsmb would ask `rsmb.test:88`, which the host cannot resolve (2.11). A proxied run may use
  `/dev/null`: the proxy resolves `rsmb.test`.
- `Scratch::new(ep)` for the DC: `mkdir -p` and `chmod 1777` instead of `chown tester:tester`
  (6.1). `Scratch::sh` (which runs as the Unix user `tester`) is not used on the DC.
- `kdc_requests` reads `docker logs rsmb-samba-dc` (stdout and stderr) as `proxy_lines` does.
- The rules of Stage 4 hold: a test in `tests/fast.rs` names `fast()` or `dc()`, never an
  address; a test that passes its own `--proxy` builds its whole command line.

### 7.2 `./test.sh` and the budget

No new step. The proxied step's floor of 100 lines counts `fileserver.rsmb.test:445` only and is
not touched by the DC's lines. Budgets stay **150 s and 600 s**.

| Step | Now | Expected | From |
|---|---|---|---|
| `check.sh` | 27 s | 28 | the checks of 6.3 |
| `cargo fmt --check`, `cargo clippy` | 0 | 1 | clippy is counted and was cached in the 456 s run; after a code change it takes 0.3 s (2.11). A run on a cold `target/` checks every dependency in this step and is not a budget run |
| unit tests | 3 | 4 | nine tests, two with local sockets |
| `tests/fast` | 12 | 15 | F71 to F76: about 14 caches at 0.1 s, about 30 short processes; they run beside the others |
| `tests/fast` through the proxy | 15 | 18 | the same |
| `tests/slow` | 31 | 33 | S71, S72 run beside the others; S72 waits 3 s for an address that is not there |
| `tests/serial` | 365 | 380 | X71 10 s for the silent KDC and its pause; X72 2 s |
| tshark | 3 | 3 | |
| **counted** | **456** | **about 482 of 600** | a margin of about 118 s |

`fast`: 57 s now, about 66 of 150. No new test takes more than 15 s. If the first full run is
over 540 s, the implementing agent says so in "Deviations during implementation" before adding
anything; X71 (10 s) is the one test whose assertion a unit test also holds.

### 7.3 Unit tests (no server)

| Test | File | Asserts |
|---|---|---|
| `ccache_paths` | session | the table of 5.3: unset and empty give `/tmp/krb5cc_1000` for uid 1000 **and say "default"**; `FILE:/tmp/x`; `/tmp/a:b`, `./c`, `plain` are paths and not default; `KCM:`, `KCM:1000`, `KEYRING:persistent:1000`, `DIR:/run/x`, `MEMORY:y`, `API:z` are `Kind::Auth` errors whose message holds the type and `cache file` |
| `ticket_choice` | session | `pick_ticket` on `CCache` values built in the test (the fields are public): a valid service ticket: `service`, user and realm as spelled in the cache; `-H` in another case still finds it; a valid TGT only: not `service`; **an expired service ticket beside a valid TGT: not `service`, and the expired entry is gone from `cache.credentials`**; everything expired, end time 1,795,029,928: the message holds the server's name and **the literal `expired at 2026-11-18T19:25:28Z`** (written by hand, not computed with the conversion under test; finding 10); a cache with tickets for another host and no TGT: `holds no ticket for cifs/<host>@<realm>`; **with `default` true the expired and the no-ticket message hold `KRB5CCNAME is not set, so the default file`, with `default` false they do not** (finding 14); the configuration entries (end time 0) never count; `t.user` = `TESTER` and `t.domain` = `rsmb.test` pass and the result keeps the cache's spelling; `t.user` = `other`: the message holds `tester@RSMB.TEST` and `other`; `t.domain` = `OTHER.TEST`: holds both realms; a default principal with two parts: `more than one name part`. Every error is `Kind::Auth` and starts `Kerberos: ` |
| `krb5_conf_kdc` | session | `krb5_kdc`: a file with two realms gives the right one; `kdc = h`, `kdc = h:750`, `kdc = [::1]:88`, `kdc = [2001:db8::1]`, **`kdc = 2001:db8::1` (a bare IPv6 address: the whole value is the host, port 88)**, `kdc = tcp/h`, `kdc = udp/h:88`; the first `kdc` of several; a `kdc` under `[libdefaults]` or in another realm's block is not taken; **a realm block with a nested block before its `kdc` (`auth_to_local_names = {` … `}`) still gives its `kdc`, a `kdc` line inside the nested block is not taken, and a `kdc` of the next realm after such a block is not taken for this one** (finding 11); comments, tabs, `kdc=h`; a realm in another case does not match; an empty file, a file without `[realms]`: none |
| `kdc_files` | session | `krb5_files(None)` and `krb5_files(Some(""))` are `["/etc/krb5.conf"]`; `Some("/a:/b")` is `/a`, `/b` in that order. `kdc_for` on files the test wrote: the first file that names a kdc for the realm wins and is returned with it; **a path that does not exist and a directory count as absent**; no file with the realm: `("rsmb.test", 88, None)` for `RSMB.TEST`. No call reads the environment or `/etc` (finding 12) |
| `cache_owner` | session | the check of 5.3 as a function of two ids: equal passes; different is `Kind::Auth` and the message holds both ids and `own user only` |
| `kerberos_refusals` | session | `kerberos_refusal`: `Protocol { STATUS_LOGON_FAILURE }` keeps `login refused`; `Protocol { STATUS_INVALID_PARAMETER }`: `the server did not accept the ticket` and the status; `Auth { message: "TGS session key has unsupported etype 20" }` and `InvalidData` (no status): **`could not be used`, the crate's text, and no word that blames the server**; `Disconnected`: a lost connection. All but the last `Kind::Auth` (finding 5) |
| `kdc_errors` | session | `kdc_error` for 7, 13, 32, 36, 37 and 99: the words of 5.7; 13 holds `kinit -f` and `kvno cifs/<host>`; 7 holds the SPN and the realm |
| `kdc_relay_carries_the_exchange` | session | a fake KDC on a local TCP socket that records what it reads, and a UDP client standing for the crate. **Answer handed back**: the client's datagram arrives at the KDC as 4 bytes of length and the same bytes, the KDC's framed answer comes back as one datagram, and a second datagram is served too. **KRB-ERROR**: the fake answers a hand-written KRB-ERROR with code 7: the relay returns **`Kind::Auth`** with `has no service`, and the client gets nothing. **Connected and silent** (accepts, never answers), limit 200 ms: **`Kind::Connect`**, `connected to the KDC, but it did not answer within 200 ms`, within a second, **the same message in 20 runs of 20** (one timer at a time, finding 4). **Closed port**: `Kind::Connect`, `Connection refused`. **Closes after the request**: `Kind::Connect`, `closed the connection without an answer`. **Through a proxy**: `kdc.proxy` is the fake proxy of `socks5_handshake_bytes`: the CONNECT request it records is `05 01 00 03`, the length, `kdc.invalid`, port 88: **the KDC's name goes to the proxy as a name**; with a fake proxy that answers reply 5 the error is `Kind::Connect`, `proxy <proxy>: cannot reach kdc.invalid:88: connection refused`, and **nothing connected to the fake KDC** (its accept count is 0): no way around the proxy; with a fake proxy that accepts and never answers, limit 200 ms: `open_tcp`'s handshake message, every time. **The tails**: with `from` a file the message names it; with `from` None it holds `so the realm's name was tried`, `KRB5_CONFIG` and `kvno cifs/`, and never `/etc/krb5.conf` as the file to edit. Every message starts `Kerberos: asking the KDC` |
| `classify_statuses` (extended) | session | `smb2::Error::SessionExpired` is `ConnectionLost { silent: false }` and its message holds `ended the session` |
| `banner_names_the_proxy` (extended) | session | with `kerberos` the line holds `'RSMB.TEST\tester' (Kerberos) on`; without, no `Kerberos` |
| `args_are_sound` (extended) | main | `-k` sets `kerberos`; `-k -p x` and `-k --no-pass` are errors; **`-k -u u -d D` parses** (finding 9); `--keep-going` sets `keep_going` and `-k` does not; the help of `-k` names `KRB5CCNAME` |
| `table_is_complete`, `help_all_has_everything` (extended) | commands | 9 topics; the topic `kerberos` holds `KRB5CCNAME`, `kvno cifs/`, `kinit -f`, `through the proxy`, `the realm's own name`, `used to be the short form of --keep-going` and `never falls back to a password`; the example line; the manual holds no `unless -k` and no `-k (--keep-going)`; the synopsis holds `[-k] [-u <user>]` and `[--keep-going]` |

### 7.4 Integration tests

Every test runs the real binary. Caches come from `krb_cache` into the test's `tmp` directory.
"`conf`" is `krb5_conf(dir, "10.213.45.12")` as `KRB5_CONFIG`.

**`tests/fast.rs`** (each run twice: `dc()` is the DC by address, then the DC by name through
port 1080)

| # | Test | How observed | Asserts |
|---|---|---|---|
| F71 | `kerberos_login` | `Client::kerberos(dc(), cache with the service ticket for dc().spn(true))`, a `Scratch` on the DC | **start-up is 2 requests in 2 flights on 1 connection: NEGOTIATE 1, SESSION_SETUP 1**; `use share` 1 / 1; `ls /` has `hello.txt`, `sub`, `upload`; `get /sub/1m.bin`: sha256 equal to the DC's; `put` of a file of 300,000 bytes into the scratch directory: sha256 on the DC equal (a signed write); `use encrypted`, `cat /hello.txt`: `kerberos works`; exit 0; stderr empty. **In the direct run, a second tapped session with a TGT-only cache (`-f`) and `conf`: start-up is again 2 requests in 2 flights on 1 connection in the tap** (the KDC's connection goes beside it), and `ls /` works (finding 15) |
| F72 | `kerberos_tgt_only` | one-shot runs, not tapped: `rsmb <dc().route(None)> -k --json -N -C 'use share' -C 'ls /'` with a TGT-only cache made with `-f`. Direct run: `conf`. Proxied run: `KRB5_CONFIG=/dev/null`, so the KDC is `rsmb.test`, which only the proxy resolves | exit 0, 3 entries. With a cache made with **`-F`**: exit 3, one `error` of kind `auth`, `"command":null`, no `done`, the message holds `KDC_ERR_BADOPTION`, `kinit -f` and `kvno cifs/` |
| F73 | `kerberos_cache_errors` | one-shot runs with `dc().route(Some(&tap))` and `-k`, so that the tap can show nothing was dialled; text and `--json` | each: exit 3, kind `auth`, the message starts `Kerberos: `, **the tap saw 0 connections**. `KRB5CCNAME=<tmp>/absent`: holds the path and `No such file`, and not `KRB5CCNAME is not set`. `KCM:` and `DIR:/x`: hold `KCM`, `DIR`. A text file: `is not a ticket cache`. **`KRB5CCNAME=/etc/hostname` (a file of another owner; skipped with a printed line when the test itself runs as uid 0 or the file is its own): holds `belongs to uid 0` and `own user only`** (finding 13). `expired_copy` of a good cache: `expired at`. `-u nobody`: holds `tester@RSMB.TEST` and `nobody`. `-d OTHER.TEST`: holds both. (A cache without any usable ticket, and the hint for the default file, are unit-test cases: the gate never reads `/tmp/krb5cc_<uid>`.) Then, with the good cache: `-u TESTER -d rsmb.test` logs in (exit 0) |
| F74 | `kerberos_usage` | one-shot runs, no server | `-k -p x`: exit 2, with `--json` kind `usage`; `-k --no-pass`: the same. **`rsmb -H 127.0.0.1 -P 1 -k --json -N -C pwd` with stdin not a terminal and the harness's default `KRB5CCNAME`: exit 3, kind `auth`, the message holds `ticket cache /nonexistent/rsmb-test/krb5cc` and `No such file`**: `-k` asked for no password, read the cache it was told to, and did so before dialling port 1 (finding 3: a positive assertion, on a path the test controls). `help all` has the topic `KERBEROS`, the flag `-k, --kerberos` and `--keep-going` without a short form |
| F75 | `kerberos_reconnect_reads_the_cache_again` | `Client::kerberos(dc(), cache)`, `drop_connections()` | `use share`, `ls /`: works. The cache file is renamed away, the tap drops the connection: `ls /` fails, kind `auth`, the message holds `reconnect failed: Kerberos: ticket cache` and the path; `pwd` works. The file is put back: `ls /` works on "exactly 1 new connection" with **8 requests** (NEGOTIATE, SESSION_SETUP, TREE_CONNECT, 5). Then the file is replaced by a cache of **`kother`**, the tap drops: `ls /` fails, kind `auth`, the message holds `kother@RSMB.TEST` and `tester` (K5; finding 8) |
| F76 | `kerberos_banner` | pty, `Pty::tapped`-style with `-k` and the cache | the banner holds `logged in as 'RSMB.TEST\tester' (Kerberos) on`; the transcript holds no `Password`; in the proxied run it holds `through proxy` |
| H2 | `docs_match_the_table` (extended) | reads `docs/commands.md` | **the section `## Kerberos` holds every message of 5.3, 5.5 and 5.7** (finding 16), as the section "Proxy" is checked today: `names a ticket cache of type`, `is not a ticket cache rsmb can read`, `belongs to uid`, `own user only`, `more than one name part`, `not to '`, `expired at`, `holds no ticket for cifs/`, `KRB5CCNAME is not set, so the default file`, `asking the KDC`, `connected to the KDC, but it did not answer within 10 s`, `closed the connection without an answer`, `has no service cifs/`, `KDC_ERR_BADOPTION`, `kinit -f`, `kvno cifs/`, `the ticket-granting ticket has expired`, `clocks`, `KRB_AP_ERR_BADMATCH`, `KDC error`, `so the realm's name was tried`, `did not accept the ticket`, `could not be used`, `ended the session`; and it names both kinds, `auth` and `connect`. The unit tests assert the same strings in the code, so a message changed in one place fails in the other |
| — | the six existing places that pass `-k` for keep-going | | `--keep-going` (`closed_stdout_ends_a_keep_going_run`, `stop_at_first_failure`, `json_never_starts_the_shell` and three more) |

**`tests/slow.rs`** (parallel, shared lock)

| # | Test | How observed | Asserts |
|---|---|---|---|
| S71 | `kerberos_kdc_is_asked_through_the_proxy` | one-shot runs, not tapped; TGT-only caches of **`kdirect`** and **`kproxy`** (`-f`), which no other test uses; `kdc_requests` before and after | **Control** (the probe can see): direct, `-H 10.213.45.12`, the cache of `kdirect`, `conf`: exit 0, and `kdc_requests("kdirect", "10.213.45.1")` rose by exactly 1. **Through the proxy**: `--proxy socks5://10.213.45.20:1080 -H dc.rsmb.test`, the cache of `kproxy`, **`conf`, whose KDC the host reaches directly** (`check.sh`): exit 0; `kdc_requests("kproxy", "10.213.45.20")` rose by exactly 1; **`kdc_requests("kproxy", "10.213.45.1")` is 0, now and at the end of the test**; the proxy's log gained at least one `connected to 10.213.45.12:88`. Again with `KRB5_CONFIG=/dev/null`: exit 0, one more request from 10.213.45.20, none from 10.213.45.1, and the proxy's log gained **`connected to rsmb.test:88`** (the realm's name, resolved by the proxy; finding 1). Again through port 1081 with `tester:testpass@`: the same. With the service-ticket cache of `kproxy` (`cifs/dc.rsmb.test`): exit 0 and no new `kproxy` line at all. A client that asked the KDC itself would show `from ipv4:10.213.45.1` (measured with the crate alone, 2.3) |
| S72 | `kerberos_failures` | one-shot runs, `--json` and text, TGT-only cache of `tester` (`-f`) unless said | each: exit 3, one `error` with `"command":null`, no `done`; the kind as said (finding 2). `KRB5_CONFIG` naming `kdc = 10.213.45.10` (no port 88), `-H 10.213.45.12`: within 2 s, kind **`connect`**, the message starts `Kerberos: asking the KDC 10.213.45.10:88`, holds `refused` and the name of the file. `KRB5_CONFIG=/dev/null`, direct, `-H 10.213.45.12`: kind **`connect`**, holds `KDC rsmb.test:88`, `so the realm's name was tried`, `KRB5_CONFIG` and `kvno cifs/10.213.45.12`. `-H 10.213.45.11` with `conf`: kind **`auth`**, holds `has no service cifs/10.213.45.11`. A cache with a ticket for `cifs/10.213.45.10`, `-H 10.213.45.10`: kind **`auth`**, holds `did not accept the ticket`. Through port 1080 with `kdc = nosuch.rsmb.test`: kind **`connect`**, holds `Kerberos: asking the KDC nosuch.rsmb.test:88` and `proxy 10.213.45.20:1080: cannot reach nosuch.rsmb.test:88`. Through port 1080 with `kdc = 10.213.47.99`: the same form, within 12 s. Through port 1082 (closed) with `-H 10.213.45.12` and `conf`, a server and a KDC the host reaches directly: kind **`connect`**, the message starts `proxy 10.213.45.20:1082` (the dial of the server fails first, and nothing is tried without the proxy) |
| S64 | `no_way_around_the_proxy` (extended) | the loader probe of Stage 4 | one more proxied run: `-k`, a TGT-only cache, `-H dc.rsmb.test`, `KRB5_CONFIG=/dev/null` (the KDC is `rsmb.test`): with the control seeing a `libnss_` module, this run loads none (neither the server's nor the KDC's name is looked up on the host) |

**`tests/serial.rs`** (one at a time, exclusive lock)

| # | Test | How observed | Asserts |
|---|---|---|---|
| X71 | `silent_kdc` | a TGT-only cache made first; `ctl.sh pause samba-dc` with a `Guard` that unpauses; `-H 10.213.45.10` (which answers NEGOTIATE), `conf` | exit 3 between 9 and 13 s, kind **`connect`**, the message holds `KDC 10.213.45.12:88` and `connected to the KDC, but it did not answer within 10 s`. After the unpause a login at the DC works |
| X72 | `kerberos_connections_through_the_proxy` | `proxy_lines` before and after, nothing else running; `KRB5_CONFIG=/dev/null` | a TGT-only login through port 1080 by name: exactly 1 more `connected to rsmb.test:88` and exactly 1 more `connected to dc.rsmb.test:445`. With the service ticket in the cache: 1 more `:445`, 0 more `:88` |

Where each demand of the design is enforced: direct by address, TGT-only (KDC asked) F71, F72,
S71 · direct, service ticket (KDC not asked) F71, S71 · through the proxy by name, service
ticket F71, X72 · through the proxy, TGT-only, nothing around it S71, X72, F72, S64,
`kdc_relay_carries_the_exchange` · the KDC found by the realm's name S71, X72, F72 (proxied),
S72, `kdc_files` · expired, missing, another type, another owner, mismatch F73, `ticket_choice`,
`ccache_paths`, `cache_owner` · reconnect reads the cache again F75 · the request count F71,
F75 · KDC failures and their kinds S72, X71, `kdc_errors`, `kdc_relay_carries_the_exchange` ·
the crate's own errors `kerberos_refusals` · the documents' messages H2 · the rename of `-k`
F74, `args_are_sound`.

### 7.5 Not in the gate, and why

A session that outlives its ticket (the shortest ticket is 121 s; measured by hand, 2.7; the
message's mapping is a unit test). A Windows KDC or server. A KDC other than Samba's Heimdal. A
cache written by impacket (2.2). Latency in front of the DC (the round trips are counted, 2.3).
A TGT that is forwardable and not renewable (Heimdal's default; measured by hand, 2.4; the gate
has the not-forwardable case). The cache file that aborts the parser (K13). The default
cache file `/tmp/krb5cc_<uid>` through the binary (the harness keeps every test away from it;
`ccache_paths` and `ticket_choice` hold the rule and the hint). A KDC found by the realm's name
without a proxy (the host cannot resolve `rsmb.test`; S72 asserts the failure and its message). A realm with more
than one KDC, a `krb5.conf` with `include`. Clock difference between client and KDC (containers
share the host's clock).

## 8. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` runs after T1,
T4 and T6. Documentation is written with the task that makes it true.

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T1 | The DC | — | `testenv/samba-dc/` (6.1), the service (6.2), `check.sh` (6.3), `ctl.sh`, `up.sh`, `test.sh` (6.4); `docs/test-environment.md`; every "six containers" text. **The full `./test.sh` here**, to see the environment with seven containers pass unchanged tests, and the real build time for `up.sh`'s message | the checks of 6.3 |
| T2 | The rename | — | `--keep-going` long only, in `src/`, `tests/`, `testenv/bench-rsmb.sh` and the documents of section 9; `-k` not yet defined | `args_are_sound`, the manual tests, the six changed tests |
| T3 | The cache | T2 | 5.2, 5.3; `ccache_path`, `pick_ticket`, `read_ticket` with the owner check | `ccache_paths`, `ticket_choice`, `cache_owner` |
| T4 | The login | T1, T3 | 5.4 to 5.7, 5.9, 5.11; the harness of 7.1. First by hand: a login with the service ticket, then TGT-only direct, then through the proxy, each beside `docker logs rsmb-samba-dc`. **The full `./test.sh`** | `krb5_conf_kdc`, `kdc_files`, `kdc_errors`, `kdc_relay_carries_the_exchange`, `kerberos_refusals`, the two extended unit tests; F71 to F76; S71, S72, S64; X71, X72 |
| T5 | The manual | T4 | 5.12; `docs/commands.md` (the flag rows, a section `## Kerberos`), `docs_match_the_table` | `table_is_complete`, `help_all_has_everything`, F74, H2 (the section's messages) |
| T6 | Documents and the final gate | T5 | section 9; the measured step times; the README | — |

If a count or a message differs from section 2, the implementing agent finds out why before
changing a number or a word: they were measured, and a difference is a defect or a finding to
record under "Deviations during implementation".

## 9. Documents

No new document. Each in the task that makes it true:

| Document | What changes |
|---|---|
| `README.md` | status: Kerberos from a ticket cache (`-k`), direct and through the proxy; the install line becomes `cargo install --git https://github.com/ShigShag/rsmb rsmb` (and the `--locked` sentence follows it); one example (`kinit -f …; rsmb -k -H …`); "seven containers" where the environment is described; the gate times; **a note that `-k` was the short form of `--keep-going` until this version**: with `-p` or `--no-pass` an old command line is now refused, without either it is a Kerberos login (K17) |
| `docs/commands.md` | the synopsis; the rows `-k, --kerberos` and `--keep-going`; `-u`, `-d` with `-k`; "Start-up costs 3 requests … with `-k` 2"; every `-k` that meant keep-going; a section `## Kerberos`: the content of the topic (5.12), the tables of 5.3 and 5.7 with every message, kind and exit code, the cost (requests; round trips with and without a service ticket; one KDC exchange per login), the server's name and the SPN (5.10), the forwardable and renewable rule with the two ways out, the proxy (KDC through it, by name, nothing around), a session that outlives its ticket; exit code 3; "Differences from smbclient-ng": no `--kdcHost`, no `--ccache-file`, no password or key for Kerberos; what it does with a TGT against a Samba KDC (2.9); in that section and in the flag row, the change of meaning of `-k` (K17); the KDC's address (K4) with the sentence that smbclient-ng without `--kdcHost` asks the domain's name too; the kinds per message (`auth` or `connect`, K3), leaving the rule for a failed reconnect ("`connect` when nothing answers, `auth` when the login is refused") as it stands; the owner rule (K18); how to name a KDC with a file of one's own and `KRB5_CONFIG`. H2 checks the section's messages |
| `docs/scripting.md` | `--keep-going` wherever `-k` stood; `-k` never prompts and needs no `-p`; a Kerberos failure as an `error` object (kind `auth`, `command: null`, the message's start); `KRB5CCNAME` and `KRB5_CONFIG` as the two variables rsmb reads for it; an example; the two kinds a Kerberos failure can have and what a script does with each; that an old script's `-k` changed its meaning (K17) |
| `docs/architecture.md` | "Login": the branch, `read_ticket` before the dial, `pick_ticket` and the pinned principal, `kerberos_setup`, the relay (why the crate is given a loopback address, the select, the limit, that it uses `open_tcp` and so the proxy, no fallback), `kdc_for`, the error wording; "Start-up": `-k`; "Timeouts, dead connections, reconnect": the KDC's 10 s, `SessionExpired`; "Errors and exit codes": Kerberos is `auth`; the banner; the environment variables read (`KRB5CCNAME`, `KRB5_CONFIG` join `RSMB_HISTORY`, `XDG_STATE_HOME`, `HOME`) |
| `docs/known-limitations.md` | Kerberos leaves "Out of scope". A section "Kerberos": a file cache only, no `default_ccache_name`; no password, key or certificate; the SPN is `cifs/<-H as given>`, no canonical name, an address needs its own SPN; **a TGT-only login needs a forwardable and renewable TGT** (the crate's fixed options; which `kinit` defaults fail; the two ways out; the same options as impacket); one KDC, the first in `krb5.conf` or the server, no DNS, no `include`, no second KDC; no cross-realm; the obtained service ticket is not kept or written, so every login with a TGT asks the KDC; one round trip more than needed for that (K10); 10 s for the KDC, fixed; expiry by this host's clock with no allowance, while a server allows five minutes; a session ends with its ticket and is replaced, not re-authenticated: a transfer running at that moment fails; the loopback UDP socket (K12); a cache whose principal claims an absurd number of parts aborts the process (crate); a cache cut inside an entry is read up to the cut; tested against Samba's Heimdal KDC only, no Windows KDC, no MIT KDC, no impacket cache; the proof that nothing goes around the proxy rests on the KDC's log naming the source address. The proxy section: "every connection" now includes the KDC's. Also: the KDC is the realm's name when no file names one, which is right for Active Directory and wrong for a realm whose name is not a host; a cache owned by another user is refused, root's use of a user's cache included; **a cache file that triggers the crate's allocation ends the process with exit 134 and no `error` object, under `--json` too**; `-k` changed its meaning |
| `docs/testing.md` | the harness additions of 7.1 (`Ep::Dc`, `Ep::DcProxied`, `dc()`, `spn`, `krb_cache`, `expired_copy`, `krb5_conf`, `kdc_requests`, `Client::kerberos`, `KRB5_CONFIG=/dev/null` for every process); that a tapped Kerberos test holds the service ticket and why; every new test with what it protects; the counts (a Kerberos start-up 2 / 2, a reconnect and `ls` 8); what is not in the gate (7.5); the step times; the harness's default `KRB5CCNAME` (a path that does not exist) and why; H2 |
| `docs/test-environment.md` | seven containers; the topology row and the alias; a section "The Kerberos server": the image, why provisioning needs `xattr_tdb`, what is in the realm (6.1 table), `mkcache`, the KDC's log lines and the level that prints them, the minimum ticket lifetime, that the keys are in the image; `check.sh` point 9; the lock table (nothing new but X71's pause); `ctl.sh pause samba-dc`; build time and image size; the alias `rsmb.test` and why; the four users; `check.sh` exporting `KRB5_CONFIG=/dev/null` |
| `docs/development.md` | a Kerberos login by hand, with a cache only the user can read and without the host's configuration (finding 16): `(umask 077; docker exec rsmb-samba-dc mkcache -f tester cifs/10.213.45.12 > "$XDG_RUNTIME_DIR/rsmb-k.cc")`, then `KRB5_CONFIG=/dev/null KRB5CCNAME="$XDG_RUNTIME_DIR/rsmb-k.cc" target/release/rsmb -k -H 10.213.45.12` (the service ticket is in the cache, so no KDC is asked); that a Kerberos test in `tests/fast.rs` names `dc()`; that a direct TGT-only run needs `krb5_conf`; dependencies: none added, and why no Kerberos crate |
| `docs/library-decision.md` | Kerberos moves from "not tested" to measured: what works (2.3), and four behaviours to design around: the KDC client with its own sockets, UDP first and its 36 s and 66 s; the fixed KDC options (2.4); the cache lookup that ignores the end time; the parser's allocation (2.8); `load_ccache(None)` without a default |
| `docs/measuring.md` | one paragraph: a tap in SOCKS5 mode in front of a TGT-only Kerberos login would see the KDC's connection as a second connection of frames it cannot read; such runs are not tapped |
| `docs/performance.md` | start-up: Kerberos 2 requests (3 round trips) with a service ticket, 5 round trips with a TGT, beside NTLM's 3 requests (4 round trips); counted, not timed at the slow endpoint |
| `docs/transfers.md` | one line: a transfer that is running when the ticket ends fails as on a lost connection |
| `docs/reviews.md` | the Stage 5 section, after the review |
| `AGENTS.md` | the index row for this plan and the changed rows (`scripting.md`: `--keep-going`; `commands.md`, `architecture.md`, `testing.md`, `test-environment.md`, `known-limitations.md`, `library-decision.md`); "Working commands": an rsmb line with `-k`, `mkcache`, the gate times; "What this project is" stays; principles, **only those the work has shown** (candidates with evidence from section 2: point a library that dials by itself at a loopback relay of your own when its traffic must take your route, 2.5; give a test its own principal when a shared log must show who asked, 7.4; a server may accept what a client should refuse: check a credential's end yourself, 2.6 and 2.7) |

## 10. Assumptions this plan could not verify

Marked so that the adversary can attack them. Each says what would show it wrong.

1. **Windows.** No Windows KDC or server was at hand. Whether a Windows KDC refuses the crate's
   fixed options for a TGT that is not forwardable and renewable, whether its replies need
   anything the relay does not carry, and whether a Windows server accepts the crate's AP-REQ
   are unknown. The brief's claim stays "tested against Samba only".
2. **The relay inside rsmb** was run in the probe, with the probe's small copy of the dialer,
   not in `src/session.rs`. That `tokio::select!` with `conn` borrowed mutably by the setup
   future compiles as written in 5.5, and that the proxy's login (port 1081) works for the KDC
   dial, is expected (the same `open_tcp`), not run.
3. **Round trips** of a Kerberos login are counted from the messages; nothing with latency
   stood in front of the DC. A KDC far away adds its two round trips to every login with a TGT.
4. **A TGT that is forwardable and not renewable** was only made with Heimdal's `kinit`; MIT's
   could not be made non-renewable against this KDC. The rule "both flags" rests on the two
   refusals of 2.4 and on Heimdal's source being the KDC's.
5. **impacket caches** were not made (2.2). That impacket writes version 0x0504 with the same
   entry form, and forwardable, renewable TGTs, is known from its source, not measured here.
6. **`expired_copy`** (the scan for four times in a row) was not run. A cache whose ticket
   bytes hold four such values by chance would be changed in the wrong place; the test would
   then fail to parse, not pass wrongly.
7. **The KDC's log as a counter**: one line per request held for 14 runs, with one exception
   (a request refused for a TGT expired beyond five minutes had no such line). S71 counts
   successful requests only. A Samba update could change the line's wording; `check.sh` tests
   the wording first.
8. **Parallel runs and the users `kdirect`, `kproxy` and `kother`**: exactness of "+1" assumes one run of
   S71 at a time. Two gates at once on one machine would disturb each other here, as they
   would in Stage 4's log count.
9. **The budget** (482 of 600, 66 of 150) adds expected durations to the last measured run.
   The DC's build (61 s alone) adds to `up.sh` from nothing, which is not counted time.
10. **`krb5.conf` in the wild**: the parser reads the common form. A file that puts a realm on
    one line with its braces, or relies on `include`, yields no KDC and rsmb asks the realm's name;
    the message says which files it looked at.
11. **`/tmp/krb5cc_<uid>` as the default** is MIT's built-in default; a host configured for KCM
    or KEYRING has no such file. The message for the missing file says so; rsmb does not read
    `default_ccache_name`.
12. **The pause in X71** freezes the whole DC; that a paused KDC accepts the TCP connection
    and then says nothing was measured with the probe's relay (10.0 s), which had one timer
    around dial and exchange. With the two stages of 5.6 the message is the second stage's;
    the binary was not run.
13. **The eighth request count** of F75 (NEGOTIATE, SESSION_SETUP, TREE_CONNECT and the 5 of a
    listing) is derived from the 9 of an NTLM reconnect minus one SESSION_SETUP, not measured.
14. **Samba's rule that a ticket of two minutes or less cannot be used** is as measured on
    4.23.8; nothing in the plan needs a shorter ticket.
15. **The host's Kerberos**: this machine has MIT tools, an `/etc/krb5.conf` and `smbclient`
    4.25.1, which tries Kerberos by default. The gate keeps away from all of it: `check.sh`
    exports `KRB5_CONFIG=/dev/null`, every rsmb process of the tests gets that and a
    `KRB5CCNAME` that does not exist, caches come from the container. That the existing
    `smbclient` checks of `check.sh` behave the same with `KRB5_CONFIG=/dev/null` was tried for
    two calls (2.11), not for the whole script. A host without Kerberos tools was not tried;
    nothing in the design calls them.
16. **The realm's name as the KDC** was measured through the proxy with a network alias. That
    a real domain's name resolves to a KDC is how Active Directory is set up, not something
    this environment can show; an MIT realm named after nothing in DNS needs the file.
17. **The owner check** was not run. A cache on a file system that reports another owner for
    the user's own files (some network mounts) would be refused; a user who runs rsmb as root
    with another user's cache must copy it.
18. **F73's file of another owner** is `/etc/hostname`: present and root-owned on this host;
    the test skips the case with a printed line where it is not.

## 11. Adversary findings and what changed

One adversary attacked the first version of this plan on 2026-11-18. All seventeen findings
were checked by the orchestrating session against the plan, the code and the documents, and
accepted. The user confirmed decision K2 as written: a TGT that is not forwardable and
renewable stays a documented limit with a message that names `kinit -f` and a service ticket in
the cache. What this plan measured for the revision is in 2.11.

| # | Finding | Evidence | What changed |
|---|---|---|---|
| 1 | **Without a `kdc` in `krb5.conf` the KDC was `-H`**: in Active Directory the file server is usually not a domain controller, and the message sent the user to `/etc/krb5.conf`, which needs root | smbclient-ng (impacket) without `--kdcHost` connects to the domain's name, port 88; a domain's name resolves to its controllers. Here (2.11): through the proxy with the alias `rsmb.test`, the KDC saw 1 request from the proxy and the proxy logged `connected to rsmb.test:88`; direct, the host cannot resolve the name | The fallback is `<realm in lower case>:88`, resolved by the proxy when there is one (K4, D11, 5.4, `kdc_for`). The tail of a "could not be asked" message points at `KRB5_CONFIG` with a file of the user's own and at `kvno` (5.7). The DC has the alias `rsmb.test` (6.2), `check.sh` checks it (6.3). Direct TGT-only runs use `krb5_conf(dir, "10.213.45.12")` (7.1; F71, F72, the control of S71, S72, X71); S71 and X72 assert `connected to rsmb.test:88`; S72 asserts the direct failure and its tail; assumption 16 |
| 2 | **K3 made "the KDC cannot be reached" kind `auth`**, against the documents' own rule | `docs/commands.md`, "Proxy": failures are kind `connect`; "Timeouts and reconnect" and `docs/scripting.md`: a failed reconnect is `connect` when nothing answers, `auth` when the login is refused | Two kinds (K3, D12): a failed `open_tcp`, no answer within the 10 s and a closed connection on the way to the KDC are `Kind::Connect`, the message still starting `Kerberos: asking the KDC …`; KRB-ERRORs and everything about the cache stay `auth` (5.7 with a kind column, 5.11, 5.12). S72 and X71 assert `connect`, `kdc_relay_carries_the_exchange` asserts the kind of every case. The documents' rule is left as it stands (section 9) |
| 3 | **The harness neutralised `KRB5_CONFIG` but not `KRB5CCNAME`**: F74's `-k` without a cache variable would read `/tmp/krb5cc_<uid>` or a developer's `KCM:`, and its assertion was a negative | 7.1 and F74 of the first version; this host has `kinit` | Every rsmb process of the harness gets `KRB5CCNAME=/nonexistent/rsmb-test/krb5cc` beside `KRB5_CONFIG=/dev/null` unless the test sets them (7.1). F74 asserts positively: exit 3, kind `auth`, the message names that path and `No such file`, before port 1 is dialled |
| 4 | **The relay had two 10 s timers** (an outer `timeout(limit, …)` around `open_tcp(&kdc, limit)`), so which message a silence gave depended on the millisecond | 5.6 of the first version | `open_tcp` runs under its own limit and reports in its own words; what is left of the limit bounds the write and the read, with one message of its own (5.6, 5.7). The unit test runs the silent cases 20 times and wants one message (7.3) |
| 5 | **5.5 step 5 blamed the server for errors the crate raises itself** before anything is sent or on the KDC's answer | `authenticator.rs`: `etype_from_code` (an unsupported encryption type), `parse_ticket`, the TGS reply's decryption, `parse_krb_error` | `kerberos_refusal` (5.5): only an error with `e.status()` is the server's; without a status the message is `the ticket in the cache or the KDC's answer could not be used (<the crate's text>)`. Unit test `kerberos_refusals` |
| 6 | **The new wrong-password check of `check.sh` ran the host's `smbclient`**, which tries Kerberos by default and reads the host's `/etc/krb5.conf`, against assumption 15 | `smbclient` 4.25.1 on this host; `client use kerberos = desired` is its default | `check.sh` exports `KRB5_CONFIG=/dev/null` at its top and the check passes `--use-kerberos=off` (6.3). Measured so: `NT_STATUS_LOGON_FAILURE` (2.11). Assumption 15 rewritten |
| 7 | **K17 was wrong**: an anonymous run needs neither `-p` nor `--no-pass`, nor does a prompt at a terminal, so an old `rsmb -H h -N -k -C ls` becomes a Kerberos login without any refusal | `src/main.rs`: the password is the empty string when the user name is empty | K17 rewritten: the meaning changes, loudly with `-p` or `--no-pass`, silently without (5.2). The README, `docs/commands.md` (flag row, "Differences from smbclient-ng"), `docs/scripting.md`, `docs/known-limitations.md` and the manual's topic say so (5.12, section 9) |
| 8 | **F75 used `kdirect`**, which 6.1 reserved for S71 | 6.1 and F75 of the first version; S71 counts that user's lines exactly | A fourth user `kother` for F75 (6.1; the image builds with it, 2.11) |
| 9 | **The synopsis said `-u` and `-d` exclude `-k`**, while 5.2 lets them be given | 5.12 of the first version: `[-k \| [-u …] [-d …] …]` | `[-k] [-u <user>] [-d <domain>] [-p <password> \| --no-pass]`, with the sentence that `-k` excludes `-p` and `--no-pass` (5.12); `args_are_sound` parses `-k -u u -d D`, the manual test wants the new synopsis |
| 10 | **`commands::filetime` takes a Windows FILETIME**, not Unix seconds | `src/commands.rs`: `secs = ft / 10_000_000 - 11_644_473_600` | 5.3 states `(u64::from(end) + 11_644_473_600) * 10_000_000`; `ticket_choice` expects the literal `2026-11-18T19:25:28Z` for 1,795,029,928 |
| 11 | **The `krb5.conf` parser**: realm blocks nest, and a bare IPv6 `kdc` would be split at its last colon | `auth_to_local_names = { … }` inside a realm; `kdc = 2001:db8::1` | 5.4: braces are counted and a `kdc` counts at depth 1 of the wanted realm only; more than one `:` without brackets is an address. Both in `krb5_conf_kdc` |
| 12 | **`kdc_for` read the environment itself**, so "unset means `/etc/krb5.conf`" was never tested | 5.4 of the first version | `krb5_files(var)` is pure and `kdc_for(files, realm)` takes the files; the environment is read once, in `kerberos_setup` (5.4, 5.5). Unit test `kdc_files`: unset and empty, the colon list, a missing file and a directory as absent, the realm's name as the fallback |
| 13 | **The cache file was trusted blindly**: one planted in world-writable `/tmp` by another local user would log the session in as the planter's principal, unseen under `--json`. And the plan did not say how a cache that triggers the crate's allocation ends | `/tmp/krb5cc_<uid>` is the default; 2.8: exit 134 | K18, D13, 5.3: the opened file must belong to `getuid()`, else an `auth` error naming both ids; `cache_owner`, F73 with `/etc/hostname`; assumptions 17 and 18. K13: the abort is not guarded (a complete check is a second parser, a partial one moves the hole by one entry) and `docs/known-limitations.md` states exit 134 and no `error` object |
| 14 | **Only the read failure named the default file**: a user whose `kinit` writes to KCM would see "expired" or "no ticket" for a stale `/tmp` file and run `kinit` in a loop | 5.3 of the first version | The hint (`KRB5CCNAME is not set, so the default file <path> was used; …`) follows the expired and the no-ticket message too when the path is the default (5.3); `ccache_path` returns whether it is; `ticket_choice` asserts the hint with and without |
| 15 | **K15 forbade tapping any TGT-only login**, but direct with `-H 127.0.0.1` the KDC's connection does not pass the tap | the SPN `cifs/127.0.0.1` exists; here (2.11): 1 connection, 2 requests in 2 flights in the tap, the KDC asked beside it | K15 rewritten; F71 asserts a TGT-only start-up of 2 requests in 2 flights in the direct run |
| 16 | **No test tied the Kerberos messages in `docs/commands.md` to the code**, as `docs_match_the_table` does for the proxy; and the `docs/development.md` example wrote a TGT to a world-readable `/tmp/k.cc` and read the host's configuration | `tests/fast.rs`, the block for the section "Proxy"; section 9 of the first version | H2: the section `## Kerberos` must hold every message stem, and the unit tests assert the same strings in the code (7.4). The example uses `umask 077`, a file under `$XDG_RUNTIME_DIR` and `KRB5_CONFIG=/dev/null` (section 9) |
| 17 | **The header and section 11 spoke of two adversaries** | `AGENTS.md`, step 2; brief, amendment 8: one adversary | Both corrected |

Also noted by the orchestrator: `cargo clippy` is counted time and was cached in the 456 s run.
Measured (2.11): 0.3 s after a change in the sources; the budget table has the row and a margin
of about 118 s (7.2).

Rejected: nothing.

## Deviations during implementation

Recorded by the implementing agent, each with the evidence that made it necessary.

| # | What | Evidence and reason |
|---|---|---|
| V1 | The `<what>` of 5.5 step 1 is `login as '<realm>\<user>' at <hostport>`, and `kerberos_refusal` puts `Kerberos: ` before it (`Kerberos: login as 'RSMB.TEST\tester' at 10.213.45.12:445: the server did not accept the ticket (…)`), not `Kerberos login as …` | K3, 5.12 and the manual promise that **every** Kerberos failure starts `Kerberos: `; with the plan's `what` the server's refusals would not. `kerberos_refusals` asserts the start for all four cases |
| V2 | When `-u` is not given and `-d` differs from the cache's realm, the mismatch message says `not to the realm '<d>'` instead of `not to '' in '<d>'` | F73's case `-d OTHER.TEST` (no `-u`, as with `-k` one usually gives none) would otherwise print `not to '' in 'OTHER.TEST'`. With `-u` the wording is the plan's (`not to 'other'`, `not to 'u' in 'D'`); H2's stem `not to '` holds for both |
| V3 | The DC's image took **84.7 s** to build from nothing (plan 2.2: 60.8 s); `up.sh` now says "from nothing: about 150 s" (Stages 1 to 4: 68 s, plus 85 s) instead of "about 130 s" | `time docker compose build samba-dc` on 2026-11-18: `real 1m24.658s` (95 packages from the network, then provisioning). The whole environment was not rebuilt from nothing to measure the sum: that would delete and rebuild the 5.96 GB `rsmb-samba` image; the figure is the sum of the two measured builds |
| V4 | The unit test of the owner check is `cache_owner_check`, not `cache_owner` | `mod tests` does `use super::*`; a test function named `cache_owner` would shadow the function under test, and the test would call itself |
| V5 | `Client::kerberos(ep, cache, extra, env)` has an `env` parameter (the plan: `ep, cache, extra`); `run_sh` gets the harness's `KRB5_CONFIG` and `KRB5CCNAME` too | F71's second, TGT-only session needs `KRB5_CONFIG` naming the KDC. `run_sh` starts rsmb through bash (`piped`, `cat_output`); finding 3's rule is "every rsmb process the harness starts" |
| V6 | `kdc_relay_carries_the_exchange`: the 20 silent runs (KDC and proxy) run **at once** (20 relays beside each other, about 0.2 s) rather than one after another (4 s); the proxy that answers reply 5 is tried with the name `kdc.invalid` **and with the fake KDC's own address**, and the fake KDC's accept count is 0 for both | The unit-test step has a budget of about 4 s (7.2). With a name the host cannot resolve, "nothing connected to the fake KDC" could not fail; with the fake KDC's own address a fallback to a direct connection would succeed, so the 0 means something (`AGENTS.md`: "A test that something goes through X needs a case where going around X would succeed"). Each of the 20 runs still has its own timer and must give the one message |
| V7 | An answer from the KDC longer than 65,507 bytes is kind `auth`, `the KDC's answer of <n> bytes does not fit a datagram` | 5.6 says "an error" without kind or words; the KDC was reached and answered, so it is not `connect`. Not reachable with a real KDC (a ticket that large does not fit a SESSION_SETUP either); no test |
| V8 | F73's file of another owner asserts `belongs to uid <the file's owner>` (read with `metadata().uid()`), not the literal `uid 0` | The same test on a host where `/etc/hostname` belongs to another non-root user would otherwise fail for the wrong reason. Here the owner is 0 (2.11) |
| V9 | The budget of 7.2, measured: the final gate **477 s counted of 600** (expected about 482): `check.sh` 31 s (expected 28), unit tests 2 s (4), `tests/fast` 13 s (15), through the proxy 15 s (18), `tests/slow` 28 s (33), `tests/serial` 385 s (380), tshark 3 s; `./test.sh fast` 62 s of 150 (expected 66). After T1 (seven containers, no Stage 5 test yet) the full gate was 450 s; after T4, 465 s | `./test.sh` on 2026-11-18, `=== all passed (full): total 480 s, counted 477 s of 600 s budget: within budget`; `./test.sh fast`: `total 63 s, counted 62 s of 150 s budget`. No step over 540 s, nothing to cut |
| V10 | After the review: `kdc_relay` serves only the first sender it hears from and ignores every other address; `known-limitations.md` and `architecture.md` say that the crate's own UDP socket (`smb2` `kdc.rs`, `send_udp`) listens on every interface during a TGT-only login | K12 said the loopback socket is the only socket; the crate's `UdpSocket::bind(0.0.0.0:0)` takes the first datagram from anyone (the reviewer ended a login with one datagram from another host). rsmb cannot change it. `kdc_relay_serves_the_first_sender_only` |
| V11 | After the review: `KRB5CCNAME` and `KRB5_CONFIG` are read with `var_os` and handled as bytes; the cache and the `krb5.conf` files are opened with `O_NONBLOCK` and refused unless `fstat` says regular file (`open_regular`), instead of a type check before an `open` | `std::env::var(..).ok()` took a non-UTF-8 value for unset and used the default file; `File::open` on a FIFO waits for a writer inside the future `interruptible` drives, so SIGINT and SIGTERM could not end rsmb (exit 137 after 5001 ms under `timeout -k`). Opening non-blocking and checking the opened file has no window between check and open. A `KRB5_CONFIG` file that is not regular counts as unreadable (absent): the same rule as for a file that cannot be read. `kerberos_cache_odd_files`, `non_regular_files_are_not_read`, `ccache_paths`, `kdc_files` |

Assumptions of section 10 that the implementation settled: **2** (the `select!` with `conn` borrowed by the setup compiles as written; the KDC dial through port 1081 with the proxy's login works: S71's third case); **6** (`expired_copy` finds the four times of every ticket in an MIT cache and the copy parses: F73's `expired at`); **12** (a paused DC accepts the KDC's TCP connection and the second stage's message is the one given: X71, `a TGT-only login with a paused KDC failed after 10.024824059s`); **13** (a Kerberos reconnect and `ls /` is 8 requests: F75 passes with exactly 8, SESSION_SETUP 1).

Manual checks while building T4, against the DC (`docker logs rsmb-samba-dc` beside each):

```
== service ticket, direct
-           15  2026-11-18 20:22:18  hello.txt
d            0  2026-11-18 20:22:18  sub/
d            0  2026-11-18 20:22:18  upload/
kerberos works
exit=0 kdc+0
== TGT only, direct, conf
kerberos works
exit=0 kdc+1
Kerberos: TGS-REQ tester@RSMB.TEST from ipv4:10.213.45.1:49244 for cifs/10.213.45.12@RSMB.TEST [renewable-ok, canonicalize, renewable, forwardable]
== TGT only, proxy, /dev/null
… 3 entries …
exit=0 kdc+1
Kerberos: TGS-REQ tester@RSMB.TEST from ipv4:10.213.45.20:42502 for cifs/dc.rsmb.test@RSMB.TEST [renewable-ok, canonicalize, renewable, forwardable]
client[4] 10.213.45.1: connected to dc.rsmb.test:445
client[5] 10.213.45.1: connected to rsmb.test:88
== TGT -F
{"command":null,"kind":"auth","message":"Kerberos: asking the KDC 10.213.45.12:88 for a ticket for cifs/10.213.45.12: refused (KDC_ERR_BADOPTION): …","type":"error"}
exit=3
```
