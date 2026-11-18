# Known limitations

What rsmb does not do, does differently, or was not tested, for the finished program (all five
stages): the client's own limits by topic, the scope, the server tested against, the library
(`smb2` 0.43.0, `docs/library-decision.md`) and the test environment. The rules of transfers are
in [`transfers.md`](transfers.md), the proxy's behaviour in [`commands.md`](commands.md), "Proxy".
This list was checked against the unverified assumptions and the "not in the gate" lists of the
four stage plans, the reviews and `performance.md`'s "Not done"; what was resolved instead is
recorded in `docs/plans/stage-4.md`, "Deviations during implementation".

## Server compatibility

- Tested against **Samba 4.23.8 only** (Alpine 3.24.4 container). No claim is made for Windows
  servers or for NAS firmware. Neither candidate crate has been run against Windows here.
- Smallest dialect tested is SMB 2.02 (`server min protocol`). SMB1 is not supported.

## Connections and timeouts

- A silent server is noticed after between `--timeout` and max(`--timeout`, 15 s): the library
  counts the server's silence from its last frame, not from the request. Measured 15.0 s with
  `--timeout 5` for a blackholed link and for a paused server (`tests/serial.rs`).
- One reply must be complete within 6 × `--timeout` (180 s by default), however much of it is
  arriving. A very large directory over a very slow link needs a larger `--timeout`: `ls huge/`
  (one 11.4 MiB reply) on `wan-bad` took 121.1 s with the default in the final bench (96.4 s in
  the Stage 1 bench, 119.6 s when measured for the plan), and failed after 34.0 s with `--timeout 5` although bytes were
  flowing (`docs/performance.md`).
- A silent link is not retried inside the command; the next command that sends opens a new
  connection.
- After Ctrl-C during a command that was on the wire, the next command that sends reconnects (4
  round trips), because the abandoned requests hold credits and replies.
- Tab in a directory that is not cached gives up after 5 s (the terminal beeps) and forces a
  reconnect for the next command; Ctrl-C cannot interrupt a Tab. Run `ls` there first on a slow
  link: it has no such bound and fills the cache.
- After `use` of a share that requires encryption, everything on the connection is encrypted, to
  every share, until the connection is replaced.
- A connection cut by a reset rather than a FIN was not tested; a cut at the moment a command
  starts is handled by the rerun, which the tests cannot tell apart from a cut noticed while idle.
- Connecting has a fixed **10 s** for the whole dial, direct and through a proxy (the TCP
  connect and, with `--proxy`, the SOCKS5 handshake and the proxy's own connect to the server).
  `--timeout` does not extend it and there is no flag for it, so a proxy or a link that needs more
  than 10 s to complete its handshake and connect is not supported.
- An interrupt during `shares` (its list or its access probes) or `use` is not tested; an
  interrupted listing (`ls`) is.
- A login refused on a reconnect (the server's `auth` or the proxy's) is not exercised end to end;
  the start-up failures are.

## Protocol and server

- Servers that refuse information class 37 (`FileIdBothDirectoryInformation`) or several
  QUERY_DIRECTORY requests in one compound cannot be listed; there is no run-time fallback to
  another shape. Only Samba was tested.
- A server that never grants 4 credits cannot be listed (the listing compound needs CREATE, a
  query, the probe and CLOSE), and `cd` needs 2.
- DFS: paths on a DFS share get the server and share prefix as the library writes them; untested
  against a real DFS share. DFS links are not followed: a path that crosses one fails with the
  server's status.
- Guest needs `-u Guest --no-pass` (or no `-u` for anonymous); an unknown user name that the server
  maps to guest is refused, with a message that says so.
- The share list is limited to 65,511 bytes of RPC stub (about 780 shares); a larger list fails
  with a clear error, never a truncated one. An anonymous session running `shares` is tested
  (`shares_fast`, `shares_edge`); a guest session (`-u Guest`) is not.
- No listing reply close to the 16 MiB frame limit was exercised: the largest measured was
  `huge/`, about 11.4 MiB in one frame. The opt-in `xl/` would fill it and run the listing's
  second phase with 8 MiB buffers.
- A server that answers an empty directory's first query with `NO_SUCH_FILE` (no `.` and `..`
  entries) gets an empty listing; only the unit test `query_outcomes` covers it, Samba never sends
  it.
- `shares` costs 7 requests (6 after the first) for the list, which is not compounded and is kept
  after the first call, plus 2 requests per share for the access column (one more round trip for
  up to 16 shares when credits allow). **On a server with few credits the probe is serial**: a
  compound of n shares needs 2n credits, so 16 credits give 8 shares per compound and one
  compound in flight, ceil(shares ÷ (credits ÷ 2)) round trips one after another: 39 for the 306
  shares of `samba-edge` (65 ms on the fast link), about 7.8 s at 100 ms each way (computed, not
  measured). There is no flag to skip the probe.
- A connection lost while `shares` asks for the access prints the list with `?` (`null`) for the
  shares not answered, then fails with `connection_lost` (exit 1); it is not run again
  (`shares_lost_during_the_probe`).
- A broken or hostile server cannot make rsmb loop or grow: directory entries that overlap (`next`
  shorter than the entry and its name), a SUCCESS answer to a directory query or to a pipe READ
  that carries nothing where progress is needed, and a share-list fragment that adds nothing and
  is not the last are malformed answers (`other`); the 65,511-byte share-list limit is enforced
  while the fragments are collected (`overlapping_entries_are_malformed`,
  `empty_success_is_malformed`, `share_fragments_are_bounded`).
- **The access column** of `shares` is the share-level `MaximalAccess` the server reports in its
  TREE_CONNECT answer for this login. It does not say whether a given directory or file inside can
  be read or written: the share `share` says `rw` while its root directory refuses `tester` new
  files. rsmb shows no per-directory access (a listing carries no rights; asking would cost an
  open per directory). Only Samba 4.23.8 was measured (`share`, `readonly`, `guest`, `encrypted`,
  `tiny`, `IPC$`, 300 read-only shares on edge, anonymous refusals); what Windows or other servers
  put into `MaximalAccess` was not checked. Read is FILE_READ_DATA and write FILE_WRITE_DATA or
  FILE_APPEND_DATA; DELETE, which Samba also grants on `read only = yes` shares, is not shown.
- On a connection that does not encrypt yet (no `use` of an encrypted share before), Samba refuses
  the TREE_DISCONNECT of a share that requires encryption (ACCESS_DENIED), so each `shares` leaves
  one tree per such share connected on the server until the connection ends (seen with
  `smbstatus -S`: two `encrypted` trees after two `shares`; none once `use encrypted` made the
  connection encrypt). rsmb cannot encrypt for that tree without connecting it through the crate,
  which would make the whole connection encrypt.

## The SOCKS5 proxy

- **SOCKS5 only** (`socks5://`, `socks5h://`): no SOCKS4, SOCKS4a, HTTP CONNECT, proxy chains,
  UDP or GSSAPI login at the proxy. No proxy is taken from the environment (`ALL_PROXY` and the
  like are ignored), and there is no prompt or file for the proxy's password: the credentials are
  in the URL and so **visible in the process list**, like `-p`; they are never printed.
- The proxy's **own** name is resolved on this host; only the server's name goes to the proxy.
- With `--proxy`, `-H` is at most 255 bytes (a SOCKS5 request carries no longer name). An IPv6
  server is given without brackets (`-H ::1`); brackets around `-H` are not stripped (`-H
  '[::1]'` goes to the proxy as the name `[::1]` and fails).
- **Every connection goes through the proxy, the KDC's of a Kerberos login included** (by name,
  resolved by the proxy).
- **No fallback**: a proxy that cannot be reached is the command's error even when the server
  could be reached directly (tested with a closed proxy port and with the proxy gone mid-session;
  a proxy that goes silent while the server is reachable directly was not tried, the code path is
  the same).
- A dial that times out after the TCP connection to the proxy stood cannot say whether the proxy
  is silent, is not a SOCKS5 proxy, or cannot reach the server; the message names all three.
- The message for a server the proxy cannot reach is as good as the proxy's answer: microsocks
  answers an unknown name with "general failure", which rsmb reports as such.
- **A proxy that holds back small segments** slows what has many requests in flight. microsocks
  does not set `TCP_NODELAY` on its socket to the server: at the slow endpoint, walks and many
  small files took 1.1 to 1.3 times as long, a 50 MiB download 1.6 times (one run each while
  planning, `docs/performance.md`, "Through a SOCKS5 proxy"). Start-up, `cd` and `ls` cost the
  same. rsmb cannot change the proxy's socket.
- **Only microsocks 1.0.5 was tested.** Its answers (general failure for an unknown name, one
  `recv` per handshake message, "no login" preferred when both are offered) are its own; OpenSSH
  `-D`, Dante and Tor were not tried.
- **Latency between client and proxy** was never produced (the environment has the proxy next to
  the client): the extra TCP connect plus 2 round trips per connection (3 with a login) are
  counted from the messages, not timed.
- An IPv6 proxy was tested against a fake proxy only (`socks5_handshake_bytes`) and once by hand
  against a python proxy; an IPv6 SMB server not at all. The test proxy's own credentials are
  plain (`tester`/`testpass`); odd credentials were tested at a fake proxy.
- The proxied timings are single runs from planning; `bench-rsmb.sh` has no proxy mode (there is
  no smbclient-ng column to stand beside: it has no proxy option).
- Only `tests/fast.rs` runs a second time through the proxy; `tests/slow.rs` and
  `tests/serial.rs` have their own proxied cases (the login port, the 1.1 GiB file, failures,
  cuts, signals, a silent proxy). A server whose SYN is dropped behind the proxy is covered by its
  message only (`dial_says_how_far_it_got`).

## Names and paths

- A name with an unpaired UTF-16 surrogate is shown with U+FFFD in its place and cannot be opened
  by that name. A walk (`tree`, `find`, `get -r`) does not enter such a directory and reports it
  as a failure (`invalid_name`, exit 1); it cannot tell it from a directory whose name really
  holds U+FFFD, which is refused alike.
- The working directory keeps the spelling you typed (`cd TREE` shows `TREE`).
- **Names are escaped in text output**: a control character (C0, DEL, C1) or bidi control
  (U+202A-202E, U+2066-2069, U+200E, U+200F, U+061C) in a name or comment from the server, or in a
  local name, is printed as `\u{9b}` (`safe` in `src/session.rs`, used by every text printer, the
  error and progress lines, the prompt and the Tab candidate list). `--json` carries the exact
  name; the name sent back and the local file name of `get` are the exact ones. Not covered: what
  Tab inserts into the line (the exact, quoted name: the line editor shows the edit line as it is),
  and other characters that can mislead (look-alikes, zero-width joiners, combining marks, other
  invisible format characters). A name that really holds the text `\u{9b}` looks like the
  escaped one. Verified with the C1 and bidi cases Samba accepts; a raw ESC from a real server was
  not seen (`hostile_names_are_escaped_in_text`).
- Cache keys fold case with Rust's Unicode tables, not the server's: two directories that differ
  only in case share a cache entry, which only completion can show (`ls` always asks). A walk
  stores neither of two such directories, nor what is below them, even when it lists only one of
  them (`walk_feeds_the_cache`); `ls` and Tab keep the
  collision, which needs the user to visit both.
- Names typed with characters that are illegal in SMB names (`:`, `?`) are sent as typed by `cd`
  and `ls` (the server answers `invalid_name`); the transfer commands and `mkdir`, `rm`, `rmdir`
  refuse them before anything is sent. The library's private-use mapping is not used.
- A backslash in a path has to be quoted or doubled (`cd 'tree\d0'`); `/` needs nothing.

## Commands and output

- `ls <file>` is an error (`not_a_directory`); there is no listing of one file.
- Times are shown in UTC.
- A password given with `-p` is visible in the process list; without `-p` rsmb prompts on a
  terminal.
- The line editor was tested on a 200-column pseudo-terminal with `TERM=xterm`; narrow terminals,
  wide characters at the end of a line and terminal multiplexers were not tried.
- `ls` of 100,000 lines through a terminal was not timed (the pty tests interrupt it).
- `--json` never starts the line editor: lines typed at a terminal under `--json` follow the
  rules of a non-interactive run (the first failure ends it unless `--keep-going`, no prompt, no history,
  no completion).

## Transfers

- **Nothing is resumed and nothing is retried.** A failed transfer starts again from byte 0. A
  recursive transfer that failed is run again with `-f` and transfers everything again; there is
  no "skip what exists" mode.
- A file that **grows** on the server during a download arrives with the size it had at open; a
  file **rewritten in place with the same size** during a download arrives as a mix of old and new
  bytes (the library opens it sharing read, write and delete). Neither is detected; comparing the
  change time at the end would cost a round trip per file and was not measured for reliability.
- Timestamps, attributes and permissions are not kept; symbolic links are not created, and local
  symbolic links and special files are not uploaded (one error each).
- Names SMB does not allow (`"*:<>?\|`, a control character, a trailing space or period) are
  refused, also on upload from Linux, where they are legal.
- Names in one local directory that differ only in letter case are not uploaded, neither of them
  (folded with Rust's `to_lowercase()`). A server that folds names differently (or normalises
  Unicode) under `put -r -f` can let one uploaded file replace another; Samba on Linux does not
  normalise, so this was not testable.
- A directory reached twice (a link, or a server that reuses file ids across file systems under one
  share) is copied once and reported; a local tree that loops through a bind mount is cut at the
  loop. A walk on a server without file ids stops after 100,000 directories and says so.
- **The read-only attribute is untested**: this Samba does not present mode 0444 as read-only, so
  what `rm` or a replacing rename does to a read-only file on Windows was not seen. rsmb reports the
  server's status and does not clear the attribute.
- One file per `rm`, no pattern, no recursion; `rmdir` removes an empty directory only.
- **Leftovers after `kill -9`** (or a crash, or power loss): a `.rsmb-….part` stays where the data
  was going, locally or on the server; the next transfer of the same name removes it (locally it
  is never adopted: the name is removed and the download goes to a new file, so a planted file with
  any mode never becomes the result). A leftover on
  the server is locked until the server notices that a silent connection is dead (measured: "in
  use" right after a silent link, free again within the 60 s the test allows); until then `rm` and
  a new `put` of that name fail with "in use". A leftover that a failed command could not delete is
  named once, in that command's error (`left_behind`), and not tried again by later commands.
- **A link at the temporary name on the server.** The upload never writes through an object that
  is there (create-only; a collision removes the object itself, a link included, and creates
  again). One case is not caught: a *dangling* symbolic link `.rsmb-….part -> nowhere` planted by
  someone with write access to the directory. Samba then creates `nowhere` and the final name
  becomes that link. Closing it costs one request and one round trip on every `put`
  (`docs/transfers.md`, "On the server").
- Names of the temporary form (`.rsmb-` + 16 hex digits + `.part`) are not transferred.
- `-f` does not replace a target that appeared during the transfer (locally never; on the server
  the rename's replace is the server's decision and does replace it).
- **Two uploaders of one remote name**: when two rsmb processes upload different content to the
  same remote name at the same moment and the second's whole upload falls between the first's CLOSE
  and its rename (in practice a small file), the final name holds the second's complete file while
  the first reports success and the second `not_found`. Never a partial file; the reports are the
  wrong way round. Not mitigated (`docs/performance.md`, "Not done", has the fix).
- A share where one may create but not rename or delete leaves temporary files, named in the
  error. A local file system without locks gives no protection against two rsmb writing one target;
  one without hard links has a window in which a file that appears is replaced.
- `cat --json` prints at most 1 MiB of UTF-8 text; anything else is an error that says to use
  `get`.
- After an interrupted transfer, or one that failed locally once data was being read (a full local
  disk, a closed stdout under `cat`), the next command reconnects (4 round trips). A refusal
  before any data was read (`cat --json` of a file over 1 MiB, `get` into a directory that cannot
  be written) keeps the connection.
- On a server with few credits (`samba-edge`, 16) the stream runs with 64 KiB requests and small
  files about five at a time; Samba there grants a few credits more than its configured 16 at
  moments (up to 19 requests outstanding were seen), which the client uses.
- **A single `get` always streams** (3 round trips), also for a small file: its size is not known
  without asking, and a blind 256 KiB read would move 256 KiB of a large file for nothing.
- Sixteen files near the small-path threshold (256 KiB, a compound charging 6 credits each) in
  flight together were not tested; such files moved one at a time only (`threshold_boundary`).
- The sync before each local rename was measured on this machine's NVMe disk only (2,000 small files in 1.5 s
  at the fast endpoint); a rotating disk or a network file system was not measured. A local disk
  that hangs in the sync holds the cleanup after an interrupt until a further signal ends it.
- If the connection is lost between a `put`'s rename and its answer, rsmb infers the outcome from
  whether the temporary file is still there, which another client could also have changed; this
  cannot be hit on purpose and is untested (`transfers.md`).
- `cat` of a large file to a reader that stalls holds up to the 4 MiB read window; a stall longer
  than the server keeps the handle or the session fails with the server's status. Not measured.
- No test interrupts an upload on wan-bad, where the cleanup may exceed its 2 s and delete on a
  new connection (slower, still correct). A second signal between a dropped command and its
  cleanup is dropped: the cleanup (at most 2 s plus a reconnect) runs to its end, and only a
  further signal ends it (`transfers.md`).
- Downloads onto a local file system that folds case or normalises names were not tested; the
  rule is covered by hard links (`same_local_file_twice`) and `target_appears_during_get`.
- No "skip what exists" mode, no resume (above), and no `put` in two flights (a compound of our
  own would bring a small `put` from 3 round trips to 2 and close the two-uploader window; not
  built, `docs/performance.md`, "Not done").

## `tree` and `find`

- **A walk waits behind one very slow listing** once the 15 queued after it have returned: results
  are taken in the order they were queued, so that the output and the choice between two paths to
  one directory are the same on every run. Measured: nothing on `tree/` and `fanout/`, 3.2 s (7 %)
  on the whole share behind `huge/` at the slow endpoint; a directory of a million entries would
  hold the walk for the time of its own listing.
- **A very large directory costs one round trip per MiB inside a walk** (`huge/`: 15 flights, about
  2 s more than `ls` at the slow endpoint): the walk's buffer is 1 MiB so that 16 listings fit a new
  connection's credits.
- `tree` collects at most **500,000 entries** and then draws nothing (use `-L` or `find`).
  Measured synthetically: 44 to 68 MB at the limit, 167 MB when every directory holds one entry;
  no walk of that size ran (`xl/` is opt-in).
- `find`'s memory grows with the number of **directories** (the queue, the visited ids with one
  path each, the folded cache keys: about 230 bytes and a key per directory), not capped;
  `fanout/` (2,001 directories) 8.8 MB, `find /huge` under 64 MB.
- **A directory with the file id of one already listed is not entered** ("already listed as"), and
  of two paths to one directory the **first in breadth-first order** is the one listed, which may be
  the link and not the real directory (`links/a/to-c` is listed, `links/b/c` reported); if the
  listing of the first fails, the second is listed instead. A directory that leads to the start's
  parent is reported as leading above the start (`outside`), without a path. A server
  that reuses file ids across file systems mounted below one share would lose the second
  directory that way, though its content differs (no fixture can produce it here).
- **A link that leads out of the start's subtree** to a directory that is neither the start nor
  its parent is followed (Samba presents it as a directory and its id was never seen): the walk
  then covers that directory too, each directory once, as `find -L` would. It ends; it is not a
  loop.
- With no `.` entry in a listing (not Samba) the start is not seeded, so a link back to it is
  entered once before the loop check catches the second; reasoned from the code, not run.
- **Servers without file ids**: a walk enters at most 100,000 directories and goes at most 100
  deep there; past that it reports `no_file_id` and exits 1. Covered by unit tests only.
- **Patterns.** No normalisation (an NFC pattern does not match an NFD name). `-iname` maps one
  character at a time: `ß` is not `SS`, `İ` is not `i`; it differs from the server's `find` (BusyBox)
  for `İ`, `ı` and the sigmas, where the server's folding is not symmetric. Inside a class `\`
  escapes and a backwards range holds nothing (the server's `find` differs on both). `[:class:]` is
  refused, there are no `{a,b}` alternatives and no `**` across directories; a pattern matches one
  name, never a path.
- **`-size`** takes `B` or `c` for bytes and refuses `b` (512-byte blocks in Unix `find`); it compares
  bytes strictly and never rounds (Unix `find` rounds up to the unit); a directory never matches it.
- No `-mindepth`, `-mtime`, `-newer`, `-path`, `-empty`, `-exec`, `-delete`, `-ls`, no negation or
  `-o`, one start directory, a file is not a start; `tree` has only `-L`.
- **`find` is not run again after a lost connection once it has printed** (that would print the
  first part twice); it fails, says the results are incomplete, and prints no `walk_summary`.
- A walk's listings in the cache are as old as the walk; after a walk larger than the cache (500,000
  units) the cache holds what was listed last.
- The progress line of `find` on a real terminal, between dense output, was looked at in the pty
  tests only.
- Untested against Windows: junctions and directory symlinks there carry the id of the reparse
  point, not of their target, so a loop through one may end by the depth limit only.
- Patterns were compared with BusyBox `find` only (the containers'); GNU `find` may differ at
  other edges (an unclosed `[`, `[^…]`), `commands.md` has the table.
- The `links/` fixture was walked on `samba` only, not on `samba-edge` or through the 64 KiB
  listing. A silent link during a walk is not tested (the path is the one tested for `ls`).
- No `-mindepth` (not built until someone asks).

## Kerberos (`-k`)

- **A ticket cache file only.** `KRB5CCNAME` as `FILE:<path>` or a path, else
  `/tmp/krb5cc_<uid>`; `KCM:`, `KEYRING:`, `DIR:`, `MEMORY:` and `API:` caches are not read, and
  `default_ccache_name` of `krb5.conf` is not read (a host configured for KCM gets the message
  for the missing default file, which says so). No Kerberos with a password, a key or a
  certificate (PKINIT); no flag for the KDC or the cache.
- **`-k` changed its meaning**: it used to be the short form of `--keep-going`. An old command line
  with `-k` and `-p` or `--no-pass` is refused (exit 2); one without either is now a Kerberos login
  as the principal of the default cache, which succeeds when that cache is usable.
- **The service name is `cifs/<-H as given>`**: no canonical name, no reverse lookup. `-H` must be
  the server's name in the realm; an address needs a ticket or a service name for that address,
  a short name or a DNS alias is another service name.
- **A TGT-only login needs a forwardable and renewable ticket-granting ticket.** rsmb asks for a
  forwardable, renewable service ticket (`0x40810010`, the options of RFC 4120 and of impacket),
  and a KDC that follows RFC 4120, as Samba's Heimdal does, refuses that (`KDC_ERR_BADOPTION`)
  for a TGT that is not both: MIT `kinit` without `-f` where `krb5.conf` does not set
  `forwardable = true`, and Heimdal's `kinit` by default (forwardable, not renewable). The two
  ways out are named in the message: `kinit -f` (with `-r` where needed), or the service ticket
  in the cache (`kvno cifs/<host>`). smbclient-ng (impacket) sends the same options. Reading the
  TGT's own flags would need a second cache parser (the crate's does not expose them), so the
  request does not follow the TGT: decision K2 of `docs/plans/stage-5.md` still stands.
- **One KDC**: the first `kdc` of the realm in the files of `KRB5_CONFIG` (else `/etc/krb5.conf`),
  else the realm's own name in lower case, port 88. No DNS SRV lookup, no `include` or
  `includedir`, no realm written on one line with its braces, no second KDC when the first fails,
  no tickets across realms. The realm's name as the KDC is right for Active Directory (a domain's
  name resolves to its domain controllers) and wrong for a realm whose name is not a host: then a
  `krb5.conf` of one's own in `KRB5_CONFIG` is needed (the message says how).
- **No service ticket is kept**: rsmb never writes the cache, so every login with only a TGT asks
  the KDC once, and that is one TCP connect and one exchange more than needed (5 round trips
  against 3 with the service ticket in the cache). The KDC connection is opened for that exchange,
  after the cache is read and NEGOTIATE is done, not beside the SMB dial.
- **10 s for the KDC**, fixed, like the dial.
- **Expiry by this host's clock with no allowance**, while a server allows five minutes of clock
  difference: rsmb refuses a ticket a server might still take. **A session ends with its ticket**
  and is replaced by a new login, not re-authenticated in place: a reading command running at
  that moment is run again on the new login, which reads the cache again (it works with new
  tickets; with the old ones it fails with kind `auth`, `reconnect failed: Kerberos: the ticket
  for … expired at …; run kinit`); any other command fails with `connection_lost` (a transfer
  too, as on any lost connection) and is not run again, and the next one logs in again from the
  cache. Reproduced with 130 s tickets, each command 150 s after they were made: a fresh cache
  and `ls /` ok; the old cache and `ls /` `auth`; a fresh cache and `mkdir` `connection_lost`. A session outliving its ticket was measured by hand while planning
  (the shortest ticket this KDC issues for a service is 121 s), not in the gate.
- **A cache or a `krb5.conf` that is not a regular file** is refused (the cache: `auth`, `not a
  regular file`) or, for the files of `KRB5_CONFIG`, taken as absent, before anything is read: a
  FIFO has no writer and would hang rsmb in `open`, where a signal cannot end it.
- **A cache owned by another user is refused**, root's use of a user's cache included (copy it);
  a file system that reports another owner for the user's own files (some network mounts) is
  refused too.
- **A cache file that triggers the crate's allocation ends the process**: a principal that claims
  billions of name parts makes `parse_ccache` ask for an absurd allocation, and the process aborts
  with exit 134 and the allocator's line on stderr, **without an `error` object, under `--json`
  too**. A guard would be a second cache parser (decision K13). A cache cut inside an entry is
  read up to the cut.
- **The gate runs against Samba's Heimdal KDC only** (Samba 4.23.8 AD DC); no MIT KDC, no cache
  written by impacket (impacket 0.13.0 could not make one against that KDC). A Windows Server AD
  DC in a lab domain was verified by hand, with an RC4 ticket-granting ticket and the service
  ticket obtained by rsmb through a SOCKS5 proxy, and the same Windows DC is what showed the
  crate's RC4 checksum was wrong (`docs/plans/stage-5.md`, V12); it is not in
  the gate. The round trips of a Kerberos login are counted, not timed at a slow link. That
  nothing goes around the proxy rests on the KDC's log naming each request's source address,
  with a control that shows a direct request as such.

## Out of scope (from `docs/brief.md`)

ACL display, the module system, several simultaneous sessions, mount, and the remaining
smbclient-ng commands. Authentication covers user/password/domain, guest, anonymous and Kerberos
from a ticket cache; other methods are out of scope. NT-hash login does not exist in `smb2`.

## What Samba does with odd names

Measured with `smbclient` and both spikes (`docs/test-environment.md`):

- File names stored on disk in a legacy encoding (EUC-KR, GBK, Shift-JIS) are **not listed at
  all**. Samba logs `Conversion error: illegal character` and skips them, and no option was found
  that presents them. A client cannot see these files over SMB.
- Names with characters Windows forbids are listed as 8.3 mangled names (for example
  `C69YSY~1.TXT`). Mangled names are hashes and can change with the Samba version. The four whose
  original contains a DOS wildcard (`?`, `*`, `<` / `>`, `"`) cannot be opened by that name:
  smbclient gets `NT_STATUS_OBJECT_NAME_NOT_FOUND`, both crates `STATUS_OBJECT_NAME_INVALID`. They
  can be listed but not fetched.
- SMB name lookup is case-insensitive. `special/` holds `Case.txt` and `case.txt`; `smb2` read each
  with its own content in the spike, but in general opening one by name may return the other.
- Unpaired UTF-16 surrogates in names (possible from Windows) cannot come from Samba. They can only
  be covered by a unit test on the name-decoding path.

## Library behaviour (`smb2` 0.43.0)

Details and numbers in `docs/library-decision.md`, "Library behaviours later stages must design
around":

- A silent link (blackhole, paused server) fails only after between the response timeout and
  max(response timeout, 15 s). It is reported as `ConnectionLost`.
- An abandoned read is not cancelled. The next request waits behind its response (1.76 s instead of
  0.2 s in the spike). rsmb replaces the connection instead (above).
- The crate's own directory listing is not compounded: at least 4 round trips. rsmb lists with its
  own compound (1 round trip on Samba, `docs/library-decision.md`, "Listing shape").
- An unknown user that Samba maps to guest is refused with `GuestSessionRefused`. Guest login
  needs the user name `Guest`.
- The crate's `list_shares` drops hidden (`$`) and non-disk shares. rsmb has its own srvsvc call
  (limit above; 306 shares fit).
- The crate publishes a breaking 0.x release every few days. rsmb pins `=0.43.0`, so fixes upstream
  arrive only through a deliberate upgrade.
- Kerberos: the crate's KDC client dials by itself (UDP first, 36 s for a closed port, 66 s for a
  silent KDC, around any proxy), its TGS request's checksum is not RFC 4757's for RC4, and its
  cache lookup takes an expired entry; its cache parser can be made to abort the process;
  `load_ccache(None)` has no default file. rsmb no longer uses the crate's KDC client at all
  (5 of `architecture.md`), checks expiry itself and documents the rest (above).

## Test environment and measurements

- Latency comes from netem at a router container: a clean 200 ms round trip at the slow endpoint,
  and jitter plus 1 % loss on `wan-bad`. Real VPNs vary in other ways (MTU, reordering, bursts).
- Bulk throughput at the slow endpoint is limited by TCP windows at 200 ms: about 165 Mbit/s
  (iperf3), not by the client.
- `fincore` cannot confirm a page-cache eviction on overlayfs, so `check.sh` compares read times
  instead.
- The 1,000,000-entry `xl/` fixture is opt-in and has not been measured.
- The server-side connection count needs an environment nobody else is using. The smbclient-ng
  baseline therefore counts connections through the tap only.
- The proof that the server's name is not resolved on this host (S64) loads no name-service
  module through the proxy while the control loads six on this host; on a host whose
  `nsswitch.conf` loads no module at run time, or with a static build, the probe is blind (the
  test then says so) and the proof rests on the precondition, the CONNECT bytes and the proxy's
  log. The packet capture that showed no DNS query is a recorded measurement, not a test: a
  resolver that caches negative answers or encrypts its queries would hide a leak from it.
- That `samba-private` admits only the proxy (`hosts allow`) is configured, not checked; the host
  has no route into its network either way.
- The log count that proves the proxied run of `tests/fast` went through the proxy cannot see a
  second gate running the same step at the same moment (`docs/testing.md`).
- smbclient-ng was measured with one run per walk row and three per transfer row
  (`docs/performance.md`); its numbers are its own process time, which includes Python's start.
- The bench numbers were not checked by the reviews (each review checked the code and the gate).
