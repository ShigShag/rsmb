# Architecture

How rsmb is built: the finished program, after Stage 5 (Kerberos from the ticket cache). The design is about round trips: one TCP
connection, one tree connect per share, `cd` in one compound, a directory listing in one compound,
cached listings for completion, nothing sent between commands, transfers that keep up to 16 small
files or one stream window in flight, walks that keep up to 16 listings in flight. The decisions
behind it are in [`plans/master-plan.md`](plans/master-plan.md), [`plans/stage-1.md`](plans/stage-1.md),
[`plans/stage-2.md`](plans/stage-2.md), [`plans/stage-3.md`](plans/stage-3.md),
[`plans/stage-4.md`](plans/stage-4.md) and [`plans/stage-5.md`](plans/stage-5.md); this document
describes the code as it is. The rules of
transfers, each with its test, are in [`transfers.md`](transfers.md).

## Files

| File | Holds |
|---|---|
| `src/main.rs` | `Args` (clap) and `options()` (the flags as the manual prints them), the start-up rules (password, `-S`/`-C`/stdin), the runtime, signal handling, `run_blocking`, the non-interactive runner, exit |
| `src/session.rs` | `Target`, `Proxy`, the dialer and the transport halves, login (NTLM, or Kerberos: the ticket cache, `krb5.conf`, the KDC relay, the Kerberos messages), `Session` (connection, tree map, share, working directory, cache, kept share list, the registry of remote temporary files and the writer slot), the `cd` check and `open_close`, the listing (`plan`, the entry parser, both phases), the share list, `Cache` and its `invalidate`, reconnect, `Error`, `Kind`, `classify`, `valid_name` and `crate_path`, `Remote` (every request of a transfer, `mkdir`, `rm`, `rmdir`), `finish` and `cleanup`. **All use of `smb2` is in this file** |
| `src/transfer.rs` | `get`, `put`, `cat`: temporary names (`temp_name`, `is_temp_name`), `local_component`, `LocalTemp`, the target rules (`local_target`, `remote_target`), the recursive transfers, the local walk of `put -r`, `Progress` |
| `src/walk.rs` | `walk`: the one traversal routine (16 listings in flight or waiting, results in queue order, a stop, the loop check by file id, the caps for servers without ids), generic over the function that lists a directory; `Listing`, `Dir`, `Report`. The pure parts of `tree` and `find`: `Glob`, `parse_size`, `parse_depth`, `Filter`, `draw_tree`, `end_report`, `TREE_MAX` |
| `src/commands.rs` | `COMMANDS` (the command table), `CmdId`, `Arg`, `Args`, `run_line` (and the overwrite prompt's `Flow::Confirm`), argument and flag splitting (flags with a value), the command functions (`mkdir`, `rm`, `rmdir`, `lcd`, `lpwd`, `lls`, `tree`, `find` and their `run_walk` here; `get`, `put`, `cat` call `transfer`), `Out` (text or JSON lines, raw bytes for `cat`, item errors, `diag`, `written`), `entry_obj`, help rendering, time formatting |
| `src/shell.rs` | `tokenize`, `partial`, `quote`, the completer (remote and local paths), the prompt, history, the overwrite question, the interactive loop |

Module dependencies: `main` → `shell`, `commands`, `session`; `shell` → `commands`, `session`,
`transfer::expand`; `commands` → `session`, `transfer`, `shell::tokenize`, and `crate::options` for
the manual, `walk`; `transfer` → `session`, `walk`, `commands::Out`; `walk` → `session` (types
and `display_path`);
`session` → `walk::Listing`. No traits of our own, no generics over transports or front ends; the
walk is generic over a closure so that its unit tests can list a fake tree.

## Threads, runtime, signals

- One multi-thread tokio runtime with 2 workers, built in `main`. It must be multi-thread: the
  crate runs a receiver task per connection that has to keep running while the main thread sits
  in the line editor.
- The main thread owns the terminal and runs one command at a time with `block_on`. Between
  commands nothing runs but the crate's own tasks, which send nothing on an idle connection.
- Both front ends run a line through `run_blocking(rt, sess, out, line, force)`. It sets `BUSY`
  **before** the command and clears it **after the cleanup**, and hands the line to
  `interruptible()`, which creates and enables the `INTERRUPT.notified()` future, then `select!`s
  the command against it. If the interrupt wins, the command's future is dropped (every
  `LocalTemp` in it removes its file, the `JoinSet` of a recursive transfer aborts its tasks, the
  stream writer stays in its slot), then `Session::cleanup(true)` runs, itself under a second
  `interruptible()` (a further signal ends it; what it had not removed comes from `take_left()`),
  then `Session::abandon`, then an `interrupted` error (with `left_behind` if anything is left) and
  the `done` line; the result is exit code 130. A signal between the dropped command and the
  cleanup finds `BUSY` set and no waiter: it is dropped, and the cleanup runs.
- The SIGINT and SIGTERM streams are created before the line editor is first used: rustyline
  saves the SIGINT handler it finds and restores it after each line, so a handler installed later
  would be lost after the first prompt. One task handles them:
  - SIGINT: while a command (or the start-up connect) runs, interrupt it; at no command, outside
    the interactive shell (a non-interactive run waiting on stdin), exit 130; in the shell,
    nothing (at the prompt Ctrl-C is a key, rustyline returns `Interrupted` and the shell
    prompts again).
  - SIGTERM: remember it; interrupt the command if one runs, otherwise exit 130. Before that
    exit `shell::restore_terminal()` puts back the terminal attributes `shell::run` saved before
    the first prompt (rustyline has the terminal raw while it reads, and a Tab runs inside it), so
    the user's terminal stays usable. The shell exits 130 after an interrupted command when
    SIGTERM was the cause.
- `notify_waiters` stores no permit, so a signal while nothing runs does not interrupt the next
  command.

## Start-up

1. clap parses (a bad flag: clap's message and exit 2, or with `--json` among the arguments one
   JSON `error` of kind `usage` on stdout and exit 2; `--help` and `--version` as clap prints them). `--help-all` prints the manual and exits 0.
2. Usage checks, exit 2: the `--proxy` URL (one message for every refused URL, which never
   repeats it: it may hold a password), with `--proxy` a `-H` longer than 255 bytes, an unreadable
   `-S` file, `-N` with nothing to run on a terminal.
3. Password: `-p`; else empty for `-k`, `--no-pass` or an empty user; else a prompt through `rpassword`
   **only if stdin is a terminal** (rpassword itself reads `/dev/tty`, so the check is on stdin);
   else a usage error before anything touches the terminal or the network. `-k` (Kerberos)
   conflicts with `-p` and `--no-pass` in clap and never asks; nothing about the ticket cache is
   checked in `main`: `login` reads it before it dials, and the same code runs on a reconnect.
4. Runtime, signals, `Session::connect` (interruptible). A failure exits 3. In the interactive
   shell one line on stderr (`Session::describe`, built by `banner`) names the user, ` (Kerberos)`
   with `-k`, the server, the proxy's address when there is one (never its user or password) and
   the dialect.
5. `-S` lines, then `-C` lines; then the shell, or stdin lines (only when neither `-C` nor `-S`
   was given), or exit. The shell starts only when stdin is a terminal and neither `-N` nor
   `--json` was given: `--json` never starts the line editor, whose prompt and echo would go to
   stdout, so lines typed at a terminal under `--json` go through the stdin loop with its rules
   (the first failure ends the run unless `--keep-going`, Ctrl-C exits 130). `docs/scripting.md`
   has the rules.

Environment variables read: `RSMB_HISTORY`, `XDG_STATE_HOME` and `HOME` (the history file), and
with `-k` `KRB5CCNAME` (the cache) and `KRB5_CONFIG` (where the KDC is named). Nothing else:
no proxy variable, nothing of the system's Kerberos library (rsmb does not link one).

## Session

### Dialer and transport

`dial(target)` calls `open_tcp(target, DIAL_TIMEOUT)` (10 s) and hands the stream, split with
`into_split()` into our two halves, to `Connection::from_transport`. `open_tcp` is the one place
that opens a socket (`grep TcpStream::connect src/` finds it twice, both there); the crate gets
the transport halves and has no reviver, so a reconnect is the same `login` → `dial` → `open_tcp`
as at start-up, through the proxy when there is one.

- **Direct**: TCP to the server, `TCP_NODELAY`, all within the limit; on a timeout
  `connect to <host>:<port>: no answer within 10 s`.
- **Through the proxy** (`--proxy`, parsed by `parse_proxy` into `Proxy { host, port, auth }`):
  TCP to the proxy, `TCP_NODELAY` on that socket (the one rsmb owns), then `tokio-socks`'
  `Socks5Stream::connect_with_socket` or `connect_with_password_and_socket` with the target as
  `(&str, u16)`: a name goes to the proxy as a name (address type 3), an address literal as an
  address, so **the server's name is never resolved on this host** (`socks5_handshake_bytes`
  reads the bytes at a fake proxy; S64 checks that no name-service module is loaded). The whole
  dial, the proxy's own connect to the server included, has the 10 s; `--timeout` does not change
  it. A flag records that the TCP connection to the proxy stood, so a timeout says
  `proxy <p>: no answer within 10 s` before it and `proxy <p>: connected to the proxy, but the
  SOCKS5 handshake did not finish within 10 s (the proxy is silent, is not a SOCKS5 proxy, or
  cannot reach <target>)` after it: once the TCP connection stands, a paused proxy, an SMB server
  taken for a proxy and a proxy whose connect to the server hangs all look the same, and
  splitting the handshake to see which message was answered would need calls `tokio-socks` does
  not offer. `proxy_error` puts every `tokio_socks::Error` in rsmb's words (kind `connect`, the
  prefix `proxy <host>:<port>: `, the target named when the proxy answered for it, never a
  credential; the table is in `commands.md`, "Proxy").
- **No fallback.** With a proxy, `open_tcp` never dials the server itself, at start-up or on a
  reconnect: a proxy that cannot be reached is the command's error although the server might be
  reachable (`proxy_failures`, `cut_through_the_proxy`). No environment variable is read.
- `hostport(host, port)` writes `host:port`, an IPv6 address in brackets; every message and the
  banner use it.

- `Tx::send` writes the 4-byte length and the frame in one `write_all`.
- `Rx::receive` reads the 4-byte header (first byte 0), then the body with `read` in a loop and
  reports every read to a `smb2::transport::ReceiveProgress` (`begin_frame`, `record`,
  `end_frame` on every way out, also on an error). `receive_progress()` returns that counter.
  **This is not optional:** without it the crate cannot see inside a frame, and a large reply that
  takes longer than the response deadline to arrive counts as silence; the connection would be
  torn down while the server is sending. A listing reply is one frame of up to 16 MiB here.

### Login

`login(target)`: `dial`, `set_response_timeout(--timeout)`, `set_send_timeout(max(--timeout,
15 s))` (a frame being written when the link goes silent would otherwise block for the crate's
own 20 s even after the receive side declared the server silent, so an upload would fail later
than any other command; `silent_link_mid_put`), NEGOTIATE, `Session::setup` (NTLM)
with user, password and domain. 3 requests. Guest is the user `Guest` with an empty password,
anonymous the empty user. When the server answers a named login with a guest session (Samba maps
unknown users to guest), the crate refuses it (`GuestSessionRefused`) and rsmb says to use
`-u Guest --no-pass`. The keepalive stays at the crate's default (it only probes while a request
is outstanding, so an idle session sends nothing). `login` returns the connection and, with
Kerberos, who the cache says we are.

### Kerberos (`-k`)

`login` branches on `target.kerberos`, the same code at start-up and on every reconnect:

1. **`read_ticket`, before the dial** (a cache that cannot be used costs no connection):
   `ccache_path(var_os("KRB5CCNAME"), getuid())` (taken as bytes, so a name that is not UTF-8 is
   the variable's; `FILE:<path>` or a path; unset or empty `/tmp/krb5cc_<uid>`, flagged as the
   default; another type is an error naming it), `open_regular` (opens with `O_NONBLOCK`, so a
   FIFO does not wait for a writer, a wait that the signal branch of `interruptible` could not end,
   and refuses what `fstat` says is not a regular file), the owner check on the same opened file
   (`fstat` uid against `getuid`, so check and read see the same file; `cache_owner`),
   `read_to_end`, the crate's `parse_ccache`, then `pick_ticket` with
   the clock.
2. **`pick_ticket`** (pure, unit-tested): the default principal must have one name part; `-u` and
   `-d`, if set, must equal user and realm without ASCII case; the candidates are the entries the
   crate's own lookup would take (`cifs/<host>@<realm>`, `krbtgt/<realm>@<realm>`), and the latest
   end among the expired ones is remembered; then **every entry whose end time is not after now is
   removed** (the crate takes the first match whether it has expired or not, and the server
   answers an expired ticket with `STATUS_INVALID_PARAMETER`, or accepts a just-expired one and
   ends the session at once); then a service ticket for `cifs/<-H>`, else a TGT, else an error
   (expired with the time, or no ticket). The messages for the default file carry a hint that
   `KRB5CCNAME` was not set.
3. **`Session::connect` pins the principal**: after the first login `target.user` and
   `target.domain` are the cache's user and realm, so a reconnect that finds another principal in
   the cache fails `pick_ticket`'s match and names both.
4. **`kerberos_setup`**: NEGOTIATE as for NTLM, then `smb2::Session::setup_kerberos_from_ccache`
   with the user and realm as the cache spells them (the crate needs that spelling) and SPN
   `cifs/<-H as given>`. With the service ticket in the cache the crate builds the AP-REQ at once:
   1 SESSION_SETUP, no KDC. With a TGT only the crate runs a TGS exchange with
   `KerberosCredentials::kdc_address`.
5. **The relay.** The crate's KDC client opens its own sockets (UDP first: 3 attempts of 10 s,
   then TCP the same way), with no hook: left alone it would dial around the proxy, take 36 s for
   a closed port and 66 s for a silent KDC, and report both as a lost connection. So the crate is
   always given **a loopback address of ours** as the KDC: a `UdpSocket` on `127.0.0.1:0`, open
   only while that login runs. `kdc_relay` takes each datagram the first sender (the crate's
   socket) sends there and ignores every other address (so another local process cannot make
   rsmb dial the KDC through the user's proxy with its credentials, or read an answer), dials the
   KDC with **`open_tcp`** (so `TCP_NODELAY`, through the proxy with its login when there is one,
   the KDC's name resolved by the proxy, never around it), writes the 4-byte length and the
   request in one write, reads the answer and sends it back as a datagram. Because the relay *is*
   the crate's UDP peer, the crate's UDP-first costs nothing and its fallback to TCP never runs.
   `kerberos_setup` runs the relay beside the setup in a `biased` `select!`: the relay returns
   only its error, so the login ends exactly when the KDC exchange failed. **One timer at a
   time**: `open_tcp` has the 10 s (`DIAL_TIMEOUT`) and reports in its own words; what is left of
   the 10 s bounds the write and the read (`connected to the KDC, but it did not answer within
   10 s`), so the same silence always gives the same message. An answer that is a KRB-ERROR (it
   starts `0x7e` and parses with the crate's `parse_krb_error`) is not handed on: `kdc_error`
   words it, and the login ends. A new TCP connection per datagram (the crate sends one, or the
   same again after its own 10 s). The crate's own socket is another matter: it binds
   `0.0.0.0:0` and takes the first datagram from anyone (`smb2` `kdc.rs`, `send_udp`), so during
   the TGT-only login that socket listens on every interface on an ephemeral port; see
   `docs/known-limitations.md`.
6. **`kdc_for`**: the KDC is the first `kdc` of the realm in the first of `krb5_files(KRB5_CONFIG)`
   that names one (`krb5_kdc` counts braces, so a `kdc` in a nested block or another realm's block
   is not taken; `[v6]:port`, a bare IPv6 address, `host:port`, `tcp/` and `udp/` prefixes;
   `http` KDCs skipped; a file that is not a regular file counts as unreadable: absent), else the
   realm's own name in lower case, port 88. The environment is read
   in `kerberos_setup` only, so both functions are tested without it.
7. **Errors.** `kerberos_refusal` turns the crate's error from the setup into ours: a lost
   connection as `classify` says; an error with a status that `classify` calls `auth` keeps
   `login refused`; any other status is `the server did not accept the ticket (<status>)`; no
   status means the crate raised it itself (an unknown encryption type, a ticket or a KDC answer
   it cannot parse or decrypt, a signature that fails) and reads `could not be used`. Every
   Kerberos message starts `Kerberos: `; kind `auth` for the cache and for refusals, `connect` for
   a KDC that could not be asked (the dial, silence, a closed connection). The tables are in
   `commands.md`, "Kerberos".

rsmb writes no cache and keeps no service ticket it obtained: every TGT-only login asks the KDC
once. No new dependency: the crate does the Kerberos, `libc::getuid` and tokio's UDP socket the
rest.

### Tree map and the rule "every method that sends starts with `tree()`"

`Session::tree(share)` first calls `ensure_connected()`, then returns the tree from the map (key:
lower-case share name) or connects it (1 TREE_CONNECT) and stores it. Nothing disconnects a tree
of the map. `IPC$` joins the map with the first `shares`; the access probes of `shares` (below)
never join it.

Every `Session` method that sends (`use_share`, the `cd` check, `list`, `shares`) goes through
`tree()`, and nothing else calls `ensure_connected()` except `Session::access`, which needs no
tree. So a command that sends nothing never touches the network, dead connection or not.

After a tree connect to a share with `smb encrypt = required`, the crate encrypts every later
request on that connection, to every share, until the connection is replaced. rsmb keeps that
behaviour; the tests that count named requests never select `encrypted`.

### Paths

`resolve_path(cwd, arg)`: `/` and `\` separate, a leading one starts at the root, `.` and empty
components are dropped, `..` removes one and stops at the root. The working directory keeps the
spelling as typed (finding the server's spelling would cost a request).

`wire_path`: components joined with `\`, no leading separator, the root is empty. On a DFS tree
(`tree.is_dfs`) the crate sets the DFS flag on every request of that tree, and the path then
starts with `<server>\<share>`, as the crate's own `format_path` writes it. Untested against a
real DFS share. Names go out as the user typed them; the crate's private-use name mapping
(`smb2::name`) is not used.

## `cd`

1. No share: usage error.
2. `target = resolve(arg)`. If `target` is the root or a prefix of the working directory
   (component-wise, exact bytes; the working directory itself included), set it and send nothing.
3. Otherwise one `execute_compound`: CREATE of the directory (`FILE_DIRECTORY_FILE`, access
   `FILE_READ_ATTRIBUTES | SYNCHRONIZE`, `FileOpen`, share access read/write/delete) and CLOSE on
   `FileId::SENTINEL`. The CREATE's status decides. 2 requests, 1 flight.

That access mask gives SUCCESS for a 0711 directory (enterable, not listable) and ACCESS_DENIED
for a 0700 one; masks with `READ_DATA` would refuse the 0711 directory
(`docs/library-decision.md`).

## Listing

`Session::list(dir)` → `plan(max_transact, credits)` → `list_dir(conn, tree, path, b, n, what)` → sort
by name (byte order of UTF-8) → into the cache.

**`plan`** picks the buffer `b` of a full query and the number `n` of full queries:

```
c = max(credits, 4) - 3                 # CREATE, the probe and CLOSE cost one credit each
b = min(max_transact, 8 MiB - 64 KiB, c x 64 KiB), a multiple of 64 KiB, at least 64 KiB
n = clamp(min((16 MiB - 128 KiB) / b, c / (b / 64 KiB)), 1, 13)
```

So the compound never asks for more credits than the connection has (with at least 4), and
n × b + 64 KiB stays below the 16 MiB transport frame. Samba after login: b = 8,323,072, n = 2,
257 of 513 credits. Edge (64 KiB, 16 credits): b = 65,536, n = 13, all 16 credits.

**Phase 1**, one compound: CREATE (`FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE`),
n queries of `b` (class 37 `FileIdBothDirectoryInformation`, pattern `*`, the first with
`RESTART_SCANS`), one probe query of 64 KiB, CLOSE. A failed CREATE is the command's error (that
is where `not_found`, `access_denied`, `not_a_directory` arrive). Each query answer goes through
`query_outcome`: SUCCESS → entries; `NO_MORE_FILES` → complete; `NO_SUCH_FILE` on the first query
→ complete and empty (what a server without `.` and `..` answers for an empty directory); anything
else → error. When any query ended the listing: done, **1 flight**.

**Phase 2**, when the directory is larger than n buffers: the handle of phase 1 is already closed,
so the entries are dropped and the directory is listed again with the handle kept open:
`[CREATE, QD(b) × n]`, then `[QD(b) × n]` (the first with the real file id) until a query answers
`NO_MORE_FILES`, then one CLOSE. An error after the CREATE still sends the CLOSE, unless the
connection is lost.

**Parser.** Our own, for class 37 (`NextEntryOffset` at 0, `LastWriteTime` 24, `EndOfFile` 40,
`FileAttributes` 56, `FileNameLength` 60, `FileId` 96, the name from 104). The crate's
`parse_entries` fails the whole buffer on one unpaired UTF-16 surrogate; ours replaces the bad unit
with U+FFFD and lists the rest. The output buffer is read in place in the response frame (its
offset and length from the response body), and each frame is dropped once parsed. `list_dir`
keeps `.` and `..` (the walk takes the file ids of the directory and its parent from them);
`Session::list` drops them. A server cannot make it loop or grow: an entry whose `NextEntryOffset`
is not 0 and does not reach past its own name (they would overlap) is malformed, and a SUCCESS
answer that holds no entry is malformed too, since the loop that asks again would never end
(`overlapping_entries_are_malformed`, `empty_success_is_malformed`). The share list is held to the
same rules: an empty pipe READ is malformed, and the stub is checked against its limit while the
fragments are collected (`add_fragment`).

**Cost:** on Samba 5 requests in 1 flight for any directory up to about 15.9 MiB of entries (an
entry is 104 bytes plus the UTF-16 name, padded to 8: about 138,000 names of 7 characters, 27,000
of 250). Above that the directory is sent twice. On a server with 64 KiB buffers and 16 credits:
1 flight up to 832 KiB of entries (about 7,000 short names), then 1 + ⌈buffers ÷ 13⌉ + 1 flights.
Memory: the reply frame and the entries are held at once; `ls huge/` (100,000 entries, 11.4 MiB
reply) peaks well under the 150 MB the gate allows (`ls_huge_slow` prints the figure;
`docs/performance.md`).

The `entry` carries the 64-bit file id. Measured on Samba: a symlinked directory carries its
target's id (`loops/self` has the id of `loops`), the share root's `..` has id 0, a dangling link
is not listed.

## Cache

`Cache`: `(share, path)` → listing, both keys folded with `to_lowercase()`. Capped at 500,000
**units**, one per entry plus one per listing (an empty listing costs about 230 bytes, which a cap
on entries alone would not see): about 50 MB when directories hold many entries, about 115 MB if
every listing were empty. At the cap the listing stored longest ago goes first, found through an
ordered index (`order`: tick → key, `pop_first`), so eviction is logarithmic: a walk stores
thousands of small listings, and the linear search of Stage 2 was quadratic (2.6 of the Stage 3
plan: 1.6 s for 80,000 puts at 40,000; `cache_units_and_order` puts 200,000 into 100,000 within
2 s). A single listing above the cap is not stored. Filled by `ls` (always), by completion on a
miss, and by `tree` and `find` (`Cache::feed`, below); emptied by `refresh`; kept across a
reconnect. `ls` always asks the server; completion reads the cache and lists only on a miss; a
walk never reads it. Commands that change a directory drop the affected listings ("Cache
invalidation" below).

**What a walk stores.** `run_walk` offers every listing it took, its entries without `.` and `..`,
sorted by name, as `Arc<Vec<Entry>>` (exactly what `Session::list` stores for `ls`; `tree` keeps
the same `Arc` for its drawing), through `Session::feed` → `Cache::feed`. Sound because
`Remote::list` and `ls` call the same `list_dir` with the same class and both return the whole
directory; filters and the depth limit decide what is printed and entered, never what a listing
holds. Not stored: a directory that was denied or failed, and the listings of `get -r` and
`put -r`. **Two directories, one key:** keys fold case, and Samba serves `cs/Foo` and `cs/foo`
as two directories; `Cache::feed` keeps the folded keys one walk has stored in a map of the walk
(`fed`): the collision is read from the names in the parent's listing (two subdirectories with one
folded key: both keys marked given up and removed), whether or not the walk lists both, and
what is below a directory not stored is not stored either; a second listing with a stored key
still removes it. So neither is cached and Tab in either asks the server
(`cache_feed_collision_seen_in_the_parent`). `ls` and Tab keep the same collision as
before (`known-limitations.md`).

## Share list

`Session::shares`: the tree for `IPC$` from the map; CREATE `srvsvc`; WRITE a DCE/RPC bind and
READ the ack; WRITE `NetrShareEnumAll` and READ the response fragments (READs repeated
while the status is `BUFFER_OVERFLOW`) until the last; CLOSE; then the stub is wrapped into one
response PDU and parsed with the crate's parser. 7 requests the first time on a connection, 6
after. The PDU length field is 16 bits, so a stub above 65,511 bytes (about 780 shares) fails with
a clear error instead of a truncated list. The list is kept for completing `use` until `refresh`.

`Session::access` (only the `shares` command calls it; Tab after `use` takes the list alone): for
every listed share a TREE_CONNECT and a related TREE_DISCONNECT, sent as raw requests
(`probe_access`). The crate's `Tree::connect` drops the answer's `MaximalAccess`, and a `Tree`
cannot be built outside the crate, so a probed tree could not join the map anyway; the related
TREE_DISCONNECT makes the server close the tree its TREE_CONNECT just made (the tree id sent,
0xFFFFFFFF, is never valid on its own), in the same flight. Up to 16 shares go into one compound
(`ACCESS_CHAIN`: 64 worked on Samba, 256 lost the connection), and as many compounds are in flight
as the credits at the start pay for (`credits / (2 × per compound)`, at least 1): on the test
server 6 shares, 12 requests in 1 flight; on edge (16 credits) one compound of 8 shares at a time.
`MaximalAccess` becomes `rw`, `r`, `w` or `none` (`access_name`: read is FILE_READ_DATA, write is
FILE_WRITE_DATA or FILE_APPEND_DATA, GENERIC_ALL and the generic bits count); ACCESS_DENIED on the
TREE_CONNECT is `none`, any other failure `None` (printed `?`, `null` in JSON). A lost connection
is not an error of the call: `access` returns the answers so far and the error, `shares` prints
the whole list (`?` for the shares without an answer) and then fails with `connection_lost`. A lost
connection shows as an error in each answer of the compound, not as an error of the call, so
`probe_access` looks at every answer.

## Timeouts, dead connections, reconnect, interruption

- **The deadline.** The only timer on requests is the crate's response deadline (`--timeout`,
  default 30 s), with one exception: completion (below). A request to a server that has gone
  silent fails after between `--timeout` and max(`--timeout`, 15 s): the crate extends the
  deadline while the server's last frame is less than 15 s old, counted from that frame. A reply
  that keeps arriving is not silence (the receive half reports its progress), but the crate
  extends one request's deadline to at most 6 × `--timeout`, so **one reply must be complete
  within 6 × `--timeout`** (180 s by default). Connecting has its own 10 s (`DIAL_TIMEOUT`, the
  whole dial, through a proxy too), and so does the KDC exchange of a Kerberos login with a TGT
  (the relay's dial, then what is left of the 10 s for the request and the answer).
- **A Kerberos session ends with its ticket.** The server then answers every request
  `STATUS_NETWORK_SESSION_EXPIRED`, which the crate reports as `SessionExpired`; `classify` makes
  it `connection_lost` (`the server ended the session (with -k: the ticket's lifetime is over)`),
  and the ordinary path follows: the connection is marked dead, a reading command runs once more,
  and that login reads the cache again. Nothing re-authenticates a session in place.
- **No probe.** A dead connection is known when the crate's receiver task saw it close
  (`is_disconnected()`) or when a request failed.
- **`ensure_connected`** (only from `tree()`): if the connection is disconnected, log in again
  (the same `login` as at start-up) and empty the tree map. Share, working directory, cache and
  share list stay. A failed reconnect keeps the kind of its cause (`connect` when nothing answers, `auth`
  when the login is refused or the ticket cache cannot be used, …; message `reconnect failed: …`) and leaves the dead connection in
  place, so the next command that sends tries again.
- **`mark_dead`** (`conn.mark_dead()`) is called in three places, each before anything else is
  sent: by `run_parsed` when a command returned `connection_lost` (the crate reports that kind also
  while the socket is still open: a deleted session or tree, an expired session, an I/O error);
  by the completer for the same error; by `abandon`.
- **`in_flight` and `abandon`.** Every sending method sets `in_flight` before its first request
  and clears it after its last answer (`Session::remote` sets it, `finish` clears it). A future dropped in between leaves it set; `abandon()` then
  marks the connection dead. Reason: a dropped request keeps its credits and its reply until the
  server answers, no CANCEL is sent, and the crate refuses to wait for credits when nothing is
  outstanding, so the next request would fail at once or wait behind up to 16 MiB of reply. A new
  connection costs four round trips and is in a known state.
- **The retry**, in `commands::run_parsed` only: if a command fails with `connection_lost`, the
  connection is marked dead (unless the failed command's own cleanup has just replaced it: the
  session counts reconnects in `generation`, and `replaced_by_cleanup_since` tells that case
  apart); if it was **closed** (not silent) and the command's table entry says
  `rerun`, the command runs once more from its start (its first `tree()` reconnects). A **silent**
  server is not tried again inside the command: a second attempt would add the dial timeout or the
  deadline once more. Nor is a command that started on a dead connection (`Session::is_dead`)
  run again when its reconnect fails: that would dial twice (20 s instead of 10 s) and report the
  second failure. The next command that sends reconnects. The navigation commands that send
  (`shares`, `use`, `cd`, `ls`) may rerun, and so does `tree`; they print only after their last
  request (`shares` prints its list, then fails if the access probe lost the connection, so the
  rule below, nothing that has printed is run again, applies to it too). The transfer and file commands that send (`get`, `put`, `cat`, `mkdir`, `rm`, `rmdir`) do not:
  their error gets the tail `'<line>' was not run again and may or may not have taken effect`.
  `find` (`rerun: false` in the table) is the one exception `run_parsed` makes, and no command is
  run again once it has printed: the rerun condition is `(cmd.rerun || find) && !Out::written()`,
  that is while nothing of this command has gone to stdout
  (`Out` sets the flag in `write` and `raw` and clears it in `begin`); once it has printed, its
  error gets the tail `'<line>' was not run again: the results above are incomplete`.
- **Completion** runs its network work (a listing, the share list) under a 5 s bound. When it
  passes: `abandon()`, no candidates, the terminal beeps. Ctrl-C cannot interrupt a Tab: the line
  editor holds the terminal in raw mode, so the key is read only after the completer returns.
  `Session::list_cached` remembers a listing the server refused (access denied, not found; not a
  lost connection) as an empty one, so the next Tab in that directory sends nothing; `refresh` and
  the next `ls` of the directory replace it. `ls` itself never reads the cache.

## Errors and exit codes

`Kind` has 15 values; `Kind::name()` is the JSON `kind`: `not_found`, `access_denied`,
`not_a_directory`, `is_a_directory`, `already_exists`, `not_empty`, `disk_full`, `invalid_name`,
`auth`, `connect`, `connection_lost`, `interrupted`, `local_io`, `usage`, `other`. `Error` also
carries `path` (the item of a recursive transfer) and `left` (temporary files that could not be
removed), printed as `path` and `left_behind`. `classify(e, what)` maps the crate's `ErrorKind`
(and first the statuses `DIRECTORY_NOT_EMPTY` → `not_empty`, `CANNOT_DELETE` → `other`; a sharing
violation is `other`, "in use by another client"); a response frame with an error status is turned
into the crate's `Protocol` error first, so there is one mapping. Messages read `<what>: <reason>` (`/nope: not
found`, `share 'nope': not found`); a lost connection reads `connection lost: …`. A Kerberos
failure is not classified: it is built where it happens (the cache, the relay, `kerberos_refusal`),
starts `Kerberos: ` and is `auth` or `connect` (no kind of its own).

Exit codes: `usage` 2, `interrupted` 130, any other error 3 at start-up and 1 later.

## The command layer

`COMMANDS` is the one table: name, syntax, summary, details, flags, argument kinds (what they
complete to), how many are required, examples, `rerun`. Dispatch is one `match` over `CmdId`, so
a table entry without a function does not compile. `help`, `help <cmd>`, `help all`,
`rsmb --help-all`, completion of command names and dispatch all read the table. `help all`
renders the start-up flags from clap's own metadata. Adding a command: `docs/development.md`.

**`Args`.** `split_args(cmd, tokens)` returns `Args { flags, opts, pos, open }`: a table entry
declares a flag with a value by a space in its name (`"-L <n>"`, `"-name <glob>"`); after such a
flag the next token is its value whatever it is (`find -name -rf`), `opts` keeps `(flag, value)`,
and `open` is set when the line ends before the value. `parse` turns `open` and a flag with a
value given twice into usage errors; `opt(a, name)` reads a value; `positional_index` returns
`None` while a value is open, so the completer offers nothing there. Dispatch gets `&Args`;
`transfer::get` and `put` keep `(flags, positionals)`. `run_line(sess, out, line, force)`: with `force` the command runs as if `-f` had been given. When a single `get` or
`put` (no `-r`, no `-f`) fails with `already_exists` and `out.interactive` is set (only
`shell::run` sets it), `run_line` prints nothing and returns `Flow::Confirm(message)`; the shell
asks `Overwrite? [y/N]` with history off for that line and, on `y`, runs the line again with
`force`. Nothing is ever read from the terminal inside a running command.

`Out` writes text or JSON lines to a buffered stdout and remembers the first write error; `done`
flushes, and a failed write turns the line's result into `local_io`; `run_blocking` then ends the
run with exit 1 whatever `--keep-going` says (`Out::broken`). Nothing uses
`println!` (it panics on a closed pipe). In text mode errors go to stderr as `rsmb: <message>`,
after stdout has been flushed. **Everything text mode prints that holds a name or a message goes
through `safe`** (`src/session.rs`): `Out::text` (one line, escaped), `Out::diag` and the text
branch of `Out::error`, the progress line, the prompt, the overwrite question and the Tab
candidate `display`; control and bidi characters become `\u{..}`. `--json` never calls it.
`Out::doc` is the same without it, for the manual and the help (our own text, several lines), and wraps each line at spaces to 80 columns, a continuation under the line's second column (`wrap`); `--json` is never wrapped. `Out::raw` writes `cat`'s bytes as they are; `Out::begin` remembers
the line being run, so `Out::item_error` can print an error about one item of a recursive transfer
with its `command`; `transfer` and `summary` lines are flushed as they are written. `Out::diag`
writes a note on stderr (`rsmb: …`, stdout flushed first) in text mode and nothing with `--json`,
where an object carries it; `Out::written` says whether this command has written to stdout.
`entry_obj(dir, entry, depth)` builds the `entry` object for `ls`, `tree` and `find`.

## The interactive shell

`rustyline` 19 with `CompletionType::List`, history (1000 lines, consecutive duplicates dropped; each line is appended to the file as it is
entered, so SIGTERM loses nothing), the prompt
`sess.location() + "> "` (building it sends nothing). The session is shared between the loop and
the completer as `Rc<RefCell<Session>>`; the completer calls `Handle::block_on` for a listing.

Tokens (`tokenize`): ASCII white space separates; `'…'` literal; in `"…"` only `\"` and `\\`
escape; outside quotes a backslash takes the next character. `partial` is the forgiving variant
for the line up to the cursor. `quote` puts a backslash before white space, quotes and
backslashes, so `tokenize(quote(x)) == [x]`. Completion replaces the whole token under the cursor
with `quote(dir part + name)` plus `/` or a space; prefixes match by exact bytes.

## Remote, the registry, the writer slot, cleanup

`Remote` is what a command needs to overlap requests, and the only way `transfer.rs` and `walk.rs`
reach the server: a clone of the connection and of the tree, the walk's buffer size, the small-file
threshold (`small` = min(256 KiB, negotiated read and write size)), and handles on the session's
registry, writer slot and dirty flag, plus the sender half of a channel. No `smb2` type appears in
its methods: `list` (a walk listing, 4 requests, 1 flight), `stat` (2 / 1; `None` for not found),
`mkdir(path, existing_ok)` (2 / 1; `FileCreate`, or `FileOpenIf` which also opens an existing
directory and answers `NOT_A_DIRECTORY` for a file), `remove(path, dir)` (3 / 1), `read_small`
(3 / 1), `download` (the stream path down, into a sink), `put_small` and `upload` (5.3 of the plan;
`transfers.md`). Every path handed to a crate call goes through `crate_path`: each component must
be `valid_name` (no character the crate would map into the private-use area), joined with `/`.

Every command that uses a `Remote` has one shape:

```rust
let remote = sess.remote().await?;   // tree() (a dead connection is replaced here), in_flight set
let r = work(&remote, …).await;
sess.finish(remote, r).await          // cleanup if something is registered, in_flight cleared
```

- **The registry** (`Arc<Mutex<Vec<Temp>>>`): the remote temporary files that exist or may exist,
  added before the CREATE that makes one, taken out when the rename was confirmed, a delete
  succeeded or found nothing, or the CREATE itself failed; emptied at the end of every cleanup
  (`take_left`), so it is empty between commands. A name already there is refused, so two
  transfers with the same temporary name are never in flight together. A name whose rename was
  sent and not answered is marked, so the cleanup can say whether the rename probably happened.
- **The writer slot** (`Arc<tokio::Mutex<Option<FileWriter>>>`): the one stream upload's writer.
  `upload` holds the lock around each `write_chunk` and `finish`; a dropped command future drops
  the guard and the writer **stays** in the slot (the crate documents `write_chunk` as safe to drop),
  so the cleanup can end it with `abort()` instead of losing the handle.
- **`Alive`**: a clone of the `Remote`'s sender, held by the blocking task that writes a small
  downloaded file. The cleanup's first step waits until every sender is gone (`recv()` returns
  `None`), so after an interrupt nothing of the command still runs, on the wire or on the disk.
- **`finish`**: drops the `Remote`; marks a connection that a lost-connection error showed gone;
  runs `cleanup(reconnect)` when something is registered (`reconnect` false after a silent link);
  appends what is left to the error (message and `left`); clears `in_flight`; a `Remote` marked
  dirty (a download or upload that failed with requests possibly still on the wire) has its
  connection replaced, if it is still the one the `Remote` was made on (`generation`).
- **`cleanup(reconnect)`**: (1) the wait for the senders; (2) if the connection is not known to be
  gone, within `CLEANUP_WAIT` = 2 s for all of it: `abort()` on the writer, then every registered
  name deleted, all in flight together (a `JoinSet`); (3) if that ran out of time, the connection is
  gone, or a delete was refused with a sharing violation or for want of credits
  (`cleanup_verdict`), `mark_dead()` and, if `reconnect`, one new connection (`tree()`, the Stage 1
  reconnect) that carries the deletes only; (4) the display paths still registered, which `take_left()` returns and **empties** the registry
  of: a leftover is reported once, by the command that left it, so no later command retries the
  delete (a sharing violation, a new connection each time) or names it again.

## The transfer engine

Two paths, both the crate's own calls (`transfers.md` has the table): the **stream path**
(`Tree::download` / `Tree::create_file_writer_exclusive`, a window of at most 4 MiB) for every single `get`
and `cat`, a single `put` above the threshold, and every large file of a recursive transfer; the
**small path** (`read_file_compound_sized_with_info` / `write_file_compound_exclusive` + the rename) for
files of at most the threshold in a recursive transfer and a single small `put`.

**Local I/O.** The stream path's sink and source are plain closures in `transfer.rs` that call
`std::fs` and the progress line inside the command's own future, which is polled on the main
thread only (`Runtime::block_on`), so no runtime worker is blocked. A small file's local half is
one `spawn_blocking` closure: for a download create the temporary file, write, sync, rename; for an
upload read the whole file. The local walk of `put -r` is one `spawn_blocking` closure. Every local
path is made absolute when the command starts.

**`LocalTemp`** (the local temporary file): created with `O_EXCL` and `O_NOFOLLOW` and locked with
`File::try_lock`, then checked to be the file the path names; a file already there is never
adopted: with its lock held by another rsmb the transfer is refused, otherwise it is stale and its
name is removed and the file made again (`transfers.md`); `write`; `commit(target, replace)` syncs and renames (with replace) or hard-links and
unlinks (without); `Drop` removes the file unless it was committed.

**The phases** of a recursive transfer never overlap: the walk (at most 16 listings), the small
files (at most 16, each one task), the large files (one after another, the crate's window). The
file list is held in memory between them and sorted by relative path.

**Progress** is one line on stderr, redrawn at most every 100 ms and once at 100 %, then cleared;
only when stderr is a terminal and not with `--json`. No crate for it.

## The walk

`walk::walk(list, start, max_depth, enter, on_dir)`: one coordinating loop, a queue of directories
and a `JoinSet`. The start is listed first, alone; its error is the walk's. Its own id (from `.`,
which `list_dir` keeps) seeds the set of visited ids, and so does its parent's (from `..`), but
only below the share root: Samba gives the root's `..` the id 0, and a server that gave it a real
id would otherwise put an id into the set whose path does not exist (`walk_root_parent_is_not_seeded`).
A directory whose id was seen is not listed at once: it waits in `State::dups` behind the path
queued under that id. If that listing succeeds, the waiting paths go to `Report::links` with the
path it was listed under ("already listed as", never called a link: of two paths to one directory
the walk lists the first in its order, which may be the link); if it fails (gone, denied, any
other error) the next waiting path is queued in its place, and when none waits yet the id is
forgotten so that a later path takes it (`walk_lists_the_second_path_when_the_first_fails`). A
path whose id is the start's parent's (seeded with no path) goes to `links` with `None`: the
report says it leads above the start and names no path, since the walk never listed that
directory (`end_report`: reason `outside`). A directory whose name holds U+FFFD (the lossy
decoding of an unpaired surrogate: the name we would send back is another name) is not entered
and goes to `failed` with kind `invalid_name` (`walk_does_not_enter_a_name_it_could_not_decode`);
a directory without an id (0) is entered up
to depth 100 and at most 100,000 times per walk, else `capped`; a listing refused with access
denied goes to `denied`, any other failure to `failed`; a lost connection or an interrupt ends the
walk after the listings in flight have returned, and nothing more is spawned. `list` is a
parameter (a closure), so the loop check, the caps, the width and the order are unit-tested on
fake trees; in the product it is `Remote::list`.

**Queue order.** Every listing gets a sequence number when it is spawned; a result that arrives
before an earlier one waits in a `BTreeMap`, and results are handed on strictly by number. A
listing is spawned only while `spawned − taken < 16`, so listings in flight plus results waiting
are never more than 16. Everything that depends on the order then follows a breadth-first walk
done one listing at a time, whatever the network does: which directories are queued and in which
order, which of two paths to one file id is listed, the calls of `on_dir`, the vectors of
`Report`. Same tree, same result (`walk_takes_results_in_queue_order`, F50; completion order gave
different output on every run, 2.10 of the plan). The price is waiting behind a slow listing once
the 15 queued after it have returned: nothing measured on `tree/` and `fanout/`, 3.2 s (7 %) on
the whole share behind `huge/` (15 flights). `get -r` walks the same way; its counts are unchanged.

**The stop.** `on_dir` returns whether the walk goes on. After `false` nothing more is spawned, the
listings in flight are awaited and dropped (no request is abandoned on a live connection), and the
walk returns `Ok(report)`; the caller knows it stopped (`walk_stops_when_told`). `get -r` always
returns `true`; `tree` stops at its limit, `find` at a closed stdout.

## `tree` and `find`

Both are in `commands.rs` and share `run_walk(sess, out, label, start, max_depth, each)`:
`sess.remote()`, the walk with `Remote::list`, a `Progress` line (`find /x  listing: N
directories`), the cache feed, then `sess.finish()`. `each(out, progress, dir, depth, entries)`
gets every listed directory and says whether to go on; it gets the progress line so that it can
clear it before it prints. They register no temporary file, so `finish` only settles
`in_flight`. Both check the share and every flag value (`Filter::parse`, `parse_depth`) before
any request.

- **`find`** filters each listing with `Filter` (`-name`, `-iname`, `-type`, `-size`, ANDed; no
  request) and prints the matches of one directory, flushed, as its listing is taken: text the
  path from the share root with `/` after a directory, JSON `entry` with `depth`. A closed stdout
  (`Out::broken`) returns `false`: the walk stops, `find` prints nothing more and the run ends with
  exit 1. **Memory:** the listings in flight or waiting (16 at most), the queue, the set of visited
  ids with one path each, the walk's `fed` map: it grows with the number of directories, not
  entries (`find /huge` peaks under 64 MB, F43).
- **`tree`** keeps every listing (`HashMap<path, Arc<Vec<Entry>>>`, the same `Arc` the cache got)
  and counts entries; past `TREE_MAX` = 500,000 it stops the walk and fails with `other`, drawing
  nothing (measured synthetically: 44 to 68 MB at the cap, 167 MB when every directory holds one
  entry). Then the notes (why a directory was not entered) from the `Report`, and `draw_tree`:
  depth first, entries in listing order, `├── `/`└── ` and `│   `/four-space prefixes, one level of
  recursion per depth; the start's line, the drawing, an empty line and `N directories, M files`.
  JSON: one `entry` per drawn entry with `depth`, in the same order.
- **The matcher** (`Glob`): tokens `Lit`, `Any`, `Star`, `Class { negated, ranges }` over `char`;
  the usual matching with one backtrack point (the last `*`), so linear in name × pattern and never
  slow (`glob_is_linear`). With `fold` (`-iname`) two characters are equal when they are the same
  or their one-character lower-case or upper-case forms are equal; a class matches a character if
  it holds it or one of those forms. A `/` and `[:` in a class are usage errors. The rules and how
  they differ from the server's own `find` are in `commands.md`.
- **The end of a walk** (`end_report`): the denied directories, the skipped ones
  (`already_listed` with `same_as`, `outside` for a path to the start's parent, `gone` for a
  `not_found` failure, `no_file_id` for `capped`),
  each sorted by path, the other failures, and whether the walk is incomplete (any other failure,
  or `capped`). `end_of_walk` in `commands.rs` prints them (`Out::diag` in text mode, objects with
  `--json`), then `walk_summary`, then fails with `other` if incomplete. Nothing of it after an
  interrupt, a lost connection, the `tree` limit or a closed stdout.
- **Reruns:** `tree` runs again after a closed connection (`rerun: true`, it prints after the
  walk); `find` only while `Out::written()` is false (above).

## Cache invalidation

`Cache::invalidate(share, dir, subtree)` drops the listing of `dir`, and with `subtree` every
listing at and below it. Commands that change a directory call `Session::invalidate` before their
first request, whatever their outcome: `put` (the target's directory), `put -r` (the directory the
tree goes into, and everything at and below the tree's root), `mkdir` (the parent; with `-p` the
parent of every component it opens), `rm` (the file's directory), `rmdir` (the parent, and
everything at and below the directory). `get`, `cat`, `lcd`, `lpwd`, `lls`, `tree`, `find` drop
nothing (the walks store, above).
