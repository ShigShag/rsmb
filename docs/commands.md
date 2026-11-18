# Commands

Everything a user or a program can type, with what each command costs in SMB requests. The same
content is in the binary: `help`, `help <command>`, `help all` inside a session, and
`rsmb --help-all` without a server (`--json` for the machine-readable form). A test
(`docs_match_the_table` in `tests/fast.rs`) fails when this file and the command table disagree:
every command needs a section headed exactly ``## `name` ``, with its syntax and every example
from the table, and every start-up flag must be named here.

Request counts are for Samba 4.23.8 (the test server). "0 requests" means the command never
touches the network. Transfers are described in full in [`transfers.md`](transfers.md).

The examples of `get`, `put`, `mkdir`, `rm` and `rmdir` are written for **a remote working
directory you may write to that holds a file `old.txt` and an empty directory `olddir`, and a
local working directory that holds `notes.txt` and a directory `photos`**; those of `lcd` and
`lls` for the same local directory (`examples_run` builds exactly that, in a scratch directory
under `upload/`).

## Starting rsmb

```
rsmb -H <host> [-P <port>] [-u <user>] [-d <domain>] [-p <password> | --no-pass]
     [--proxy socks5://[user:pass@]host:port] [--timeout <seconds>]
     [-S <file>] [-C <command>]... [-N] [-k] [--json]
rsmb --help-all [--json]
```

| Flag | Meaning |
|---|---|
| `-H`, `--host <HOST>` | server name or address; required unless `--help-all` |
| `-P`, `--port <PORT>` | default 445 (`help all` shows `[default: 445]`) |
| `-u`, `--user <USER>` | default empty = anonymous; `Guest` = guest. `DOMAIN\user` is split at the first backslash when `-d` is not given |
| `-d`, `--domain <DOMAIN>` | default empty |
| `-p`, `--password <PASSWORD>` | conflicts with `--no-pass`; visible in the process list |
| `--no-pass` | empty password, never prompt |
| `--proxy <URL>` | connect through a SOCKS5 proxy, `socks5://[user:pass@]host:port` (`socks5h://` is the same; one trailing `/` is accepted); the proxy resolves the server's name. User and password are percent-decoded (`%40` for `@`, `%3A` for `:`, `%25` for `%`), 1 to 255 bytes each; an IPv6 proxy is written `socks5://[::1]:1080`. Anything else is a usage error whose message never repeats the URL. Visible in the process list, like `-p`. See "Proxy" below |
| `--timeout <SECONDS>` | response deadline, default 30 (`[default: 30]` in `help all`), at least 1 (see "Timeouts and reconnect"); connecting has its own 10 s, through a proxy too, which `--timeout` does not change |
| `-S`, `--startup-script <FILE>` | run the commands in the file, one per line, first |
| `-C`, `--command <LINE>` | run one command line; repeatable, in order, after `-S` |
| `-N`, `--not-interactive` | no interactive shell after the commands |
| `-k`, `--keep-going` | do not stop at the first failing command; the exit code is that of the first failure |
| `--json` | one JSON object per stdout line ([`scripting.md`](scripting.md)) |
| `--help-all` | print the manual (as JSON objects with `--json`) and exit 0; needs no server |
| `-h`, `--help`, `-V`, `--version` | short help, version |

Start-up costs 3 requests (NEGOTIATE, 2 × SESSION_SETUP) on one TCP connection. Without `-p` and
`--no-pass`, and with a user name, rsmb asks for the password on the terminal (`Password for
DOMAIN\user:`), but only when stdin is a terminal; otherwise it exits 2 with `no password: give
-p or --no-pass` before anything touches the network. Guest is `-u Guest --no-pass`; anonymous is
no `-u` at all. A server that maps unknown users to guest answers a wrong user name with a guest
session; rsmb refuses that login (exit 3) and says to use `-u Guest --no-pass` on purpose.

## How commands are given

- `-S <file>` first, then every `-C` in order.
- Then: if stdin is a terminal and neither `-N` nor `--json` was given, the interactive shell.
  If **neither `-C` nor `-S` was given** and stdin is not a terminal, or it is one and `--json`
  was given, commands are read from stdin, one per line, until end of input. Otherwise rsmb
  exits. **`--json` never starts the line editor** (its prompt and echo would go to stdout): lines
  typed at a terminal are read like lines from a pipe, under the same rules: the first failing
  command ends the run unless `-k`, Ctrl-C during a command ends the run with 130 after the
  cleanup, Ctrl-C between commands exits 130, and nothing asks to overwrite. With `-C` or `-S`, stdin is never read, so a caller
  that leaves a pipe open on rsmb's stdin does not make it wait.
- A blank line and a line whose first non-blank character is `#` are skipped and print nothing.
- A non-interactive run stops at the first failing command with that command's exit code. With
  `-k` it goes on, and exits with the code of the first failure. `exit` and end of input end the
  run. If stdout cannot be written (the reader closed
  the pipe) the run ends at once with exit 1, also with `-k`: no further command is sent. In the interactive shell a failed start-up line does not stop the shell.
- **The overwrite prompt.** In the interactive shell, a `get` or `put` of one file (no `-r`, no
  `-f`) onto an existing target asks `<message> Overwrite? [y/N] `; an answer starting with `y` or
  `Y` runs the same line again with `-f`, anything else (and Ctrl-C, Ctrl-D) prints the
  `already_exists` error. The answer is not added to the history. Nothing else ever asks: not a
  recursive transfer (it reports every existing target as an error and names `-f`), not `rm`,
  not `-S` or `-C` lines, never without a terminal.
- In the interactive shell: history (Up/Down, kept in `$RSMB_HISTORY`, else
  `$XDG_STATE_HOME/rsmb/history`, else `~/.local/state/rsmb/history`; `RSMB_HISTORY=` turns it
  off), Tab completion, Ctrl-C interrupts the running command and returns to the prompt, Ctrl-C at
  the prompt clears the line, Ctrl-D on an empty line exits 0. The prompt is the location, e.g.
  `//server/share/tree/d0> `; drawing it sends nothing.

## Quoting and paths

- Tokens are separated by ASCII white space only. U+3000 (ideographic space) is part of a name:
  `cd 東京　大阪` is one argument.
- `'…'` is literal. In `"…"`, `\"` is `"` and `\\` is `\`; any other backslash stays. Outside
  quotes a backslash takes the next character literally. Pieces that touch form one token
  (`a' 'b` is `a b`); `''` is an empty token. No variables, no globbing, no comments inside a
  line.
- Paths use `/` or `\` as separator. A backslash has to be quoted or doubled
  (`cd 'tree\d0'`, `cd tree\\d0`); `/` needs nothing. A leading separator starts at the share
  root, otherwise the path is relative to the working directory; `.` and `..` work (`..` at the
  root stays at the root).
- A token is a flag only if it is exactly one of the flags the command declares and comes before
  `--`; everything else is an argument, so `cd -rf` and `ls --help.txt` work on files with those
  names. The flags without a value are exactly `-r` and `-f` (`get`, `put`) and `-p` (`mkdir`),
  each its own token: `-rf` is not a flag, so a file called `-rf` stays reachable (`get -rf`).
  The first `--` is dropped.
- **Flags that take a value** (`tree -L <n>`; `find -name`, `-iname`, `-type`, `-size`,
  `-maxdepth`): the next token is the value, whatever it is, so `find -name -rf` looks for the
  name `-rf` and `find -name --` for `--`. A flag with a value at the end of the line is a usage
  error (`-name needs a value`), and so is one given twice. Flags and the directory may come in
  any order (`find /tree -name x` is `find -name x /tree`); a directory called `-name` is
  `find -- -name`.
- **Local paths** (`get`'s second argument, `put`'s first, `lcd`, `lls`) are relative to the local
  working directory, which `lcd` changes; a leading `~` is `$HOME`. A transfer takes them as
  absolute paths when it starts, so nothing depends on a later `lcd`.
- The working directory keeps the spelling you typed; the server compares names without case.
- **Names in text output are escaped.** A control character (C0, DEL, C1) or a bidi control
  (U+202A to U+202E, U+2066 to U+2069, U+200E, U+200F, U+061C) in a name, a share comment or an
  error message that names a remote or local path is printed as `\u{9b}` (lower-case hex), so a
  name from the server cannot start an escape sequence, move the cursor or reorder the text
  around it: `ls`, `find`, `tree`, `shares`, `pwd`, `lls`, `lpwd`, error lines, the transfer
  lines, the progress line, the prompt, the overwrite question and the Tab candidate list. A
  backslash is not escaped, so a name that really contains `\u{9b}` looks the same; check with
  `--json`. **`--json` carries the exact name** (JSON escapes only what JSON requires), the name
  sent back to the server is the exact one (`cd`, `get`, `rm` of it work), `get` creates the
  local file under the exact name, and `cat` writes raw bytes. The text a command prints for
  itself (`help`, the manual) is not touched. What Tab inserts into the line is the exact name,
  quoted; the line editor shows the line as it is (`known-limitations.md`).

## Completion (Tab)

- The first word completes to a command name. After `use`: share names (the share list is
  fetched once, 7 requests, on the first such Tab if `shares` has not run; Tab never asks for
  the access). After `help`: command names and `all`. After `cd`, `mkdir`, `rmdir`, `tree`, `find`: remote directories; after `ls`, `get`, `cat`,
  `rm` and as `put`'s second argument: remote directories and files. As `get`'s second argument,
  `put`'s first, and after `lcd` (directories only) and `lls`: **local paths**, read from the local
  directory with `read_dir` (with `~`), the same quoting and `/` after a directory, no request.
  After a flag that takes a value (`find -name `, `tree -L `) Tab offers nothing and sends
  nothing; flag names are not completed.
- A remote path is completed from the cached listing of its directory. A directory that is not
  cached is listed once (5 requests); every further Tab there costs 0 requests until `refresh`.
  `ls` refreshes the cached listing of the directory it shows; `tree` and `find` store every
  listing they fetched (not those of `get -r` and `put -r`), so Tab below a walked directory
  costs nothing. Two directories of one walk whose names differ only in letter case are stored
  by neither, nor is what is below them, even when the walk lists only one of them.
- The completed word is written in its canonical quoted form (`dir\ with\ space/`); a directory
  gets a `/`, a file a space. Names match by exact bytes: the NFC and NFD forms of a name are
  different candidates.
- Several candidates: the common prefix is inserted and the terminal beeps; a second Tab lists
  them.
- The network work of one Tab is bounded by **5 s**. When the bound passes, the Tab gives no
  candidates (the terminal beeps) and the connection is replaced by the next command that sends.
  Ctrl-C does not interrupt a Tab (the line editor has the terminal in raw mode). On a link too
  slow to list a large directory in 5 s, run `ls` there first: it has no such bound and fills the
  cache.

## `shares`

```
shares
```

Lists every share of the server, hidden ones (ending in `$`) included, from srvsvc
`NetrShareEnumAll` on `IPC$`, and your access to each. The list is kept for completing `use` until
`refresh` (the access is not: Tab never probes it).

- **Access** is the share-level access the server reports for this login, the `MaximalAccess` of
  a TREE_CONNECT: `rw` (read and write), `r-` (read only), `-w` (write only), `--` (the server
  refused the TREE_CONNECT with access denied), `?` (no answer, for example any other error). Read
  is FILE_READ_DATA, write is FILE_WRITE_DATA or FILE_APPEND_DATA. It is **not** a promise for
  every directory or file inside: a share can say `rw` while its root directory or a folder below
  refuses you (on the test server `share` says `rw`, yet only `upload/` takes new files from
  `tester`). Per-directory access is not shown: a listing carries no rights, and asking would cost
  an open per directory. Nothing is created on the server to find out.
- Measured on Samba 4.23.8 as `tester`: `share`, `encrypted`, `tiny` `rw`; `readonly` (`read only =
  yes`), `guest` and `IPC$` `r-`. Anonymous: `guest` and `IPC$` `r-`, every other share `--`.
- Costs: 7 requests in 7 flights for the list the first time on a connection (TREE_CONNECT to
  `IPC$`, CREATE, 2 × WRITE, 2 × READ, CLOSE), 6 after (the `IPC$` tree stays connected); then 2
  requests per share (TREE_CONNECT and TREE_DISCONNECT), up to 16 shares in one compound and as
  many compounds in flight as the server's credits pay for. On the test server 19 requests in 8
  flights, then 18 in 7: one round trip more than the list alone. The probed trees are
  disconnected in the same flight and do not join the tree map: a later `use` still costs its
  TREE_CONNECT.
- On a connection that does not encrypt yet, the server refuses to disconnect a share that
  requires encryption (`encrypted`), so that tree stays connected on the server until the
  connection ends: one more per `shares` (`known-limitations.md`).
- **A connection lost while the access is asked** does not hide the list that was fetched: it is
  printed with `?` (`null` in `--json`) for every share whose answer did not arrive, then the
  command fails with `connection_lost` (exit 1; `... the access of the shares shown as ? / null
  is unknown`). It is not run again (it has printed); the next command reconnects
  (`shares_lost_during_the_probe`).
- On a server with few credits the probe is slower: a compound of up to 16 shares needs 2 credits
  per share, so with 16 credits there are 8 shares per compound and one compound in flight at a
  time: ceil(shares ÷ (credits ÷ 2)) compounds one after another, 39 for the 306 shares of
  `samba-edge`. Each is one round trip, so at 100 ms each way that is about 7.8 s (computed, not
  measured: the edge server is on the fast link, where the whole command takes 65 ms). There is no flag to
  skip the probe.
- Prints one line per share: name (20 columns), type (`disk`, `printer`, `device`, `ipc`,
  `unknown`; 8 columns), access (4 columns), comment. With `--json` one `share` object each, with
  `access` as `"rw"`, `"r"`, `"w"`, `"none"` or `null`.
- A share list larger than 65,511 bytes of RPC stub (about 780 shares) fails with a clear
  message; rsmb never prints a truncated list.

```
share                disk     rw
readonly             disk     r-
guest                disk     r-
encrypted            disk     rw
tiny                 disk     rw
IPC$                 ipc      r-   IPC Service (Samba 4.23.8)
```

Example:

- `shares` — list every share, hidden ones (ending in $) included, with your access to each

## `use`

```
use <share>
```

Selects a share; the working directory becomes its root.

- Costs: 1 request (TREE_CONNECT) the first time the share is used on a connection, 0 after.
  Tree connects are kept for the session; nothing disconnects them (the probes of `shares`
  disconnect only their own trees).
- Errors: `not_found` (no such share), `access_denied` (for example `share` as guest).
- A share with `smb encrypt = required` (the test server's `encrypted`) makes the client encrypt
  every later request on this connection, to every share, until the connection is replaced.

Example:

- `use share` — select the share 'share' and stand at its root

## `cd`

```
cd [<dir>]
```

Changes the working directory; without an argument, to the share root.

- Costs: 0 requests when the target is the share root or a directory above the current one
  (`cd /`, `cd ..`, `cd`, `cd .`). Any other target: 2 requests (CREATE of the directory with
  `FILE_READ_ATTRIBUTES | SYNCHRONIZE`, CLOSE) in one compound, 1 round trip. No listing.
- Errors: `not_found`, `not_a_directory`, `access_denied`; `usage` without a selected share. The
  working directory is unchanged after a failure.
- A directory that may be entered but not listed (`traverse-only/` on the test server) can be
  entered.

Examples:

- `cd tree/d0` — enter tree/d0 below the working directory
- `cd /` — go to the share root (sends nothing)
- `cd ..` — go one directory up (sends nothing)

## `pwd`

```
pwd
```

Prints `//host/share/a/b` (`//host` with no share selected, `//host/share/` at the root). 0
requests. With `--json` one `pwd` object.

Example:

- `pwd` — print where the session stands

## `ls`

```
ls [<dir>]
```

Lists a directory, by default the working directory. `ls` always asks the server and refreshes
the cached listing that completion uses.

- Costs on Samba: **5 requests in 1 round trip** (CREATE, 2 queries of 8 MiB − 64 KiB, a 64 KiB
  probe query, CLOSE, one compound) for any directory up to about 15.9 MiB of entries (about
  138,000 short names). Above that the directory is listed a second time with the handle kept
  open. On a server with 64 KiB buffers and 16 credits (`samba-edge`): 16 requests in 1 round
  trip up to about 7,000 short names, `many/` (10,000) in 4.
- Prints one line per entry, sorted by name (byte order of UTF-8): `d` or `-`, the size
  right-aligned in 13 columns, the modification time **in UTC** as `YYYY-MM-DD HH:MM:SS` (`-` when
  the server sends none), the name, `/` after a directory. `.` and `..` are not shown. With
  `--json` one `entry` object each (with the 64-bit file id).
- Errors: `not_found`, `access_denied` (a directory you may not list), `not_a_directory` (`ls` of
  a file is this error, not a one-entry listing), `usage` without a selected share.
- A name with an unpaired UTF-16 surrogate (possible on Windows, not on Samba) is shown with
  U+FFFD and cannot be opened by that name.

Examples:

- `ls` — list the working directory
- `ls /sizes` — list the directory sizes below the share root
- `ls "special/dir with space"` — list a directory whose name has spaces

## `tree`

```
tree [-L <n>] [<dir>]
```

Draws a directory (default: the working directory) and everything below it.

- **Output.** The first line is the start, from the share root. Then one line per entry, depth
  first, by name (bytes) within a directory, files and directories mixed: the prefix of its
  ancestors (`│   ` while an ancestor has entries still to come, four spaces after its last),
  `├── ` or `└── ` for the last entry of its directory, the name, `/` after a directory, and
  ` [note]` when the directory was not entered (`forbidden/ [access denied]`,
  `self/ [already listed as /loops]`, `up/ [leads above the start, to a directory this walk
  does not list]`, `x/ [gone]`). Then an empty line and `N directories, M
  files`, counting what was drawn:

  ```
  /tree/d0/d0/d0
  ├── d0/
  │   └── file.txt
  ├── d1/
  │   └── file.txt
  ├── d2/
  │   └── file.txt
  ├── d3/
  │   └── file.txt
  └── file.txt

  4 directories, 5 files
  ```

  Nothing is printed before the walk has ended. With `--json` no drawing: one `entry` object
  (with `depth`) per drawn entry in the same order, then the end of the walk (below).
- **Depth.** The start is depth 0 and is not an entry; its entries are depth 1. `-L n` draws
  depths 1 to n and lists directories of depth 0 to n − 1; a directory at depth n is drawn
  without its content and without a note. `-L 0`, a negative or a non-number is `usage`.
- **Limit.** `tree` collects at most 500,000 entries (about 44 to 68 MB, 167 MB when every
  directory holds one entry). Past that it draws nothing and fails with `other`: `tree /x: more
  than 500000 entries; limit the depth with -L, or use find`.
- **Cost on Samba**: one listing per directory, **4 requests in 1 round trip** (CREATE, a query
  of 1 MiB, a 64 KiB probe, CLOSE in one compound), at most 16 in flight on the one connection;
  a directory above 1 MiB of entries costs one round trip more per MiB (`huge/`: 19 requests in
  15 flights). Measured: `tree/` (342 listings) 1,368 requests, 4.8 s at the slow endpoint
  (200 ms round trip); smbclient-ng takes 69 s for the 85 directories of `tree/d0`
  (`performance.md`).
- **The end of the walk** and the exit status: as for `find` (below). In text mode the notes are
  said twice: on the line and on stderr at the end.
- Errors: `not_found`, `not_a_directory`, `access_denied` (the directory itself), `usage` (no
  share, a bad `-L`), `other` (the limit, or a directory that failed for another reason),
  `connection_lost`, `interrupted`. **Run again once after a lost connection** (it has printed
  nothing yet). The listings feed the cache that Tab reads.

Examples:

- `tree /tree/d0/d0` — draw a directory and everything below it
- `tree -L 2 /tree` — two levels only (one directory in it denies listing: reported, still exit 0)

## `find`

```
find [<dir>] [-name <glob>] [-iname <glob>] [-type f|d] [-size [+|-]<n>[BKMGTP]] [-maxdepth <n>]
```

Lists every entry below a directory (default: the working directory) that passes all the tests
given; with no test, every entry. The directory itself is not printed.

| Flag | Matches |
|---|---|
| `-name <glob>` | the entry's name (its last path component) matches the pattern |
| `-iname <glob>` | the same without regard to letter case (below) |
| `-type f` / `-type d` | files only / directories only |
| `-size [+\|-]<n>[BKMGTP]` | files of more than (`+`), less than (`-`) or exactly `n` units |
| `-maxdepth <n>` | at most `n` levels below the directory (1: its own entries); at least 1 |

- **Output.** One line per match, the path **from the share root** whatever the argument and the
  working directory, `/` after a directory (`/tree/d0/`, `/tree/d0/file.txt`); nothing else on
  stdout. With `--json` one `entry` object per match (with `depth`, 1 = directly in the start).
- **Order: the same on every run.** Breadth first: the start's entries, then the entries of each
  of its directories in name order, and so on; within a directory by name (bytes); a directory's
  own line always comes before what is in it. The matches of one directory are printed (and
  flushed) as soon as its listing is taken, so output starts at once. A walk takes its listings
  in the order it queued them, at most 16 in flight or waiting, so a slow listing holds the
  output of those queued after it (measured: nothing on `tree/` and `fanout/`, 7 % on the whole
  share behind `huge/`).
- **Patterns** (`-name`, `-iname`): matched against the whole name, as Unicode characters (not
  bytes, not UTF-16 units).

  | In the pattern | Means |
  |---|---|
  | `*` | any run of characters, also none; a leading `.` is not special; `**` is `*` |
  | `?` | exactly one character: `简`, `𠮷`, `😀` are one each; `caf?.txt` matches the NFC `café.txt`, `caf??.txt` the NFD one |
  | `[abc]`, `[a-c]`, `[一-龥]` | one character of the set; ranges by code point |
  | `[!…]`, `[^…]` | one character not in the set |
  | `]` first in a class (`[]a]`, `[!]]`), `-` first or last | a member |
  | `\x` | the character `x`, outside and inside a class (`[\]]` is the class holding `]`); a lone `\` at the end is a backslash |
  | `[z-a]` (a backwards range) | holds nothing |
  | `[[:digit:]]` and other `[:class:]` | refused (`usage`): write the range, `[0-9]` |
  | `[` with no closing `]` | the character `[` |
  | anything else, `{`, `}`, `,` included | itself (no `{a,b}` alternatives) |
  | `/` | refused (`usage`): a pattern matches one name, never a path |

  `-iname` takes two characters as equal when they are the same, or their lower-case forms are
  one character each and equal, or their upper-case forms are: `ｆｕｌｌ*` finds `ＦＵＬＬ…`,
  `привет.txt` finds `Привет.txt`, `GRÜßE.TXT` finds `Grüße.txt`, `INDEX.TXT` finds `ındex.txt`,
  `Σ` finds `σ` and `ς`; `GRÜSSE.TXT` does not find `Grüße.txt` (`ß` upper-cases to two
  characters), `istanbul.txt` does not find `İstanbul.txt`. **Names are never normalised**: an
  NFC pattern does not match an NFD name (`パン*` typed precomposed does not find the NFD
  `パン…`).

  **Differences from the server's own `find`** (BusyBox on the test server, with which the
  matcher agrees on 60 patterns over `special/` and `cjk/`): that `find` also folds `İ` with `i`,
  and its folding is not symmetric (`index.txt` finds one of `INDEX.txt` and `ındex.txt`, rsmb
  both); inside a class it takes `\` as a member and a backwards range as its two ends; it knows
  `[:class:]`. GNU `find` may differ at other edges.

  **Quoting.** The tokeniser does no globbing, so `find -name *.txt` works unquoted. Outside
  quotes the tokeniser takes a backslash for itself: write a glob escape in single quotes
  (`-name '\[abc\].txt'`), doubled (`-name \\[abc\\].txt`) or as a class
  (`-name '[[]abc[]].txt'`). From a shell, the shell's quoting comes first:
  `rsmb … -C "find -name '*.txt'"`.
- **Sizes.** In bytes, strictly, no rounding: `+n` more than `n` units, `-n` less than `n` units,
  `n` exactly. Units `B` or `c` (bytes), `K`, `M`, `G`, `T`, `P` (powers of 1024, upper or lower
  case), none meaning bytes. A file of 1,048,577 bytes matches `+1M`, one of 1,048,576 matches
  `1M` and neither `+1M` nor `-1M`. **A directory never matches `-size`** (SMB reports 0 for a
  directory). A lower-case `b` is refused (`b means 512-byte blocks in find`), and so are `C`, a
  fraction, an unknown unit and a value above 64 bits. Unlike Unix `find`, which rounds a size up
  to the unit (`-size -1M` there means empty files) and whose `b` is 512-byte blocks; unlike
  smbclient-ng, whose `+` and `-` include the boundary.
- **Cost on Samba**: one listing per directory listed, **4 requests in 1 round trip**, 16 at a
  time; the tests cost nothing (they read what the listing carries): `find /many -name 'f0999?'`
  is the one listing of `many/`, 9 requests. A directory is entered whether or not it matches.
- **The end of the walk.** After a walk that ran to its end, in this order: every directory
  whose listing was refused (text, on stderr: `rsmb: /tree/forbidden: access denied; not
  listed`; `--json`: `denied`); every directory not entered (`skipped`): one with the file id of
  a directory that was listed (`rsmb: /loops/self: not entered: already listed as /loops`; reason
  `already_listed`, `same_as` the path that was listed; a link back to the start or to a
  directory the walk listed, or the second of two paths to one directory: of two
  paths the first in breadth-first order is listed, which may be the link; when the listing of
  that first path fails, for any reason, the second is listed instead and is not reported),
  one that leads to the parent directory of the start, which the walk does not list (`rsmb:
  /loops/up: not entered: leads above the start, to a directory this walk does not list`;
  reason `outside`, `same_as` `null`; no path is named because the walk never listed that
  directory, and it is not the textual parent when the start is itself a link),
  one that vanished between its parent's listing and its own (`gone`), one past the limits for
  a server without file ids (`no_file_id`); then any other failure of one directory (an `error`
  with `path`; also a directory whose name the server sent with an unpaired UTF-16 surrogate,
  shown as U+FFFD: it cannot be sent back, so it is not entered, `invalid_name`, exit 1, never
  `gone`); then, with `--json`, one `walk_summary`. Each kind sorted by path.
- **Exit status.** **0** when the walk ran to its end, also with denied and skipped
  directories: nothing that can be listed is missing. **1** when a directory failed for another
  reason or a server without file ids hit the limit (`find /x: incomplete: 2 directories could
  not be listed`), and for the start's own error (`not_found`, `not_a_directory`,
  `access_denied`). Unlike `get -r`, where anything not copied is exit 1: there something asked
  for did not arrive.
- **What is not whole.** After an interrupt, a lost connection or a closed stdout no end report
  and no `walk_summary` follow: without a `walk_summary` the output is not whole. A closed
  stdout (`find /fanout | head -n 5`) stops the walk: nothing more is started, the listings in
  flight are awaited; the run exits 1.
- On a terminal a progress line on stderr (`find /x  listing: N directories`), cleared before
  every line that goes to stdout; never with `--json` or when stderr is not a terminal.
- Errors: `not_found`, `not_a_directory`, `access_denied` (the directory itself), `usage` (no
  share, a bad flag value, before any request), `other`, `connection_lost`, `interrupted`.
  **Run again after a lost connection only while it has printed nothing** (the start's listing
  was lost, or nothing had matched yet); once it has printed it fails with the tail
  `'<line>' was not run again: the results above are incomplete`. The listings feed the cache
  that Tab reads.

Examples:

- `find /tree -name file.txt` — every entry called file.txt below /tree
- `find /sizes -size +1M` — files larger than 1 MiB
- `find /special -iname '*.TXT' -type f` — files ending in .txt in any letter case
- `find /tree -maxdepth 2 -type d` — directories at most two levels down
- `find /cjk -maxdepth 1 -name '简体中文*'` — a pattern with non-Latin characters
- `find /special -name '[[]abc[]].txt'` — a name with a literal [

## `get`

```
get [-r] [-f] <remote> [<local>]
```

Downloads a remote file, or with `-r` a directory and everything below it.

- **Where it goes.** No `<local>`: into the local working directory, under the remote name.
  `<local>` an existing directory, or ending in `/`: into it. Otherwise (without `-r`) `<local>`
  is the file's own path, and its directory must exist. With `-r`, `<local>` must be an existing
  directory and the tree goes to `<local>/<name>/…`, always (a tree is not renamed while it is
  copied); for the share root `<name>` is the share's name. A leading `~` is `$HOME`. Relative
  paths are taken against the local working directory when the command starts.
- **Never a partial file.** The data is written to `.rsmb-<16 hex digits>.part` in the target's
  directory (locked against a second rsmb), synced, and only then renamed to the target. An
  interrupted or failed `get` removes it; nothing is ever under the target name that did not
  fully arrive. Bytes written must equal the size the server gave at open, else the transfer fails
  (`other`, "changed during the transfer"). `docs/transfers.md` has every rule.
- **Overwrite.** An existing local file is not replaced without `-f`: `already_exists`, 0
  requests (the interactive shell asks `Overwrite? [y/N]`). `-f` replaces only what was there when
  the transfer started; a file that appears meanwhile is not replaced. A directory is never
  replaced (`is_a_directory`).
- **Cost on Samba**, one file: 6 requests plus one READ per 512 KiB: **7 requests in 3 round
  trips** up to 512 KiB (CREATE with 4 QUERY_INFO in one flight, READ, CLOSE). With `-r`:
  directory listings 16 at a time, 4 requests and 1 round trip each; files of at most 256 KiB 16
  at a time, 3 requests and 1 round trip each; larger files one after another as above.
- **Output.** One line per file that arrived: `get <remote> -> <local>  <bytes> bytes  <s> s`
  (`--json`: a `transfer` object). With `-r` then one summary line, `<n> files, <bytes> bytes,
  <s> s; <m> not transferred` (`--json`: `summary`). On a terminal a progress line on stderr.
- **Recursive transfers go on past what they cannot transfer**: a directory that denies access, a
  link back to a directory already visited (`loops/self`), a name the local file system cannot
  have, a name of the temporary form, an existing target without `-f`, a symbolic link in the
  local destination tree (not followed; its subtree is left out). Each is one error with `path`;
  the command then fails with `other` ("`<n> of <m> not transferred`"), exit 1. No summary after
  an interrupt.
- Errors: `not_found`, `is_a_directory` (a directory without `-r`; the message says `-r`),
  `already_exists`, `access_denied`, `invalid_name` (a name of the temporary form), `local_io`,
  `other`, `connection_lost`. Not run again after a lost connection.

Examples:

- `get /sizes/65537.bin` — fetch one file into the local working directory
- `get /sizes/1.bin copy.bin` — fetch it under another name
- `get -f /sizes/1.bin notes.txt` — replace the local file notes.txt
- `get -r /tree/d0/d0` — fetch the directory d0 and everything below it into ./d0

## `put`

```
put [-r] [-f] <local> [<remote>]
```

Uploads a local file, or with `-r` a directory and everything below it.

- **Where it goes.** No `<remote>`: into the remote working directory, under the local name as
  given (a symbolic link keeps its own name and uploads the content of its target; only `.`,
  `..` and `/`, which have no name, are resolved). `<remote>` ending in `/` or `\`: into that directory. Otherwise
  (without `-r`) `<remote>` is the file's own path. **A trailing `/` is the only way to say "into
  this directory"**: `put a.txt docs` with a directory `docs` fails with `is_a_directory`
  ("write 'docs/'"), with or without `-f`. With `-r` the tree goes to `<remote>/<name>/…`
  (default: the working directory). The named local file may be a symbolic link to a file.
- **Never a partial file.** The data goes to `.rsmb-<16 hex digits>.part` in the target's
  directory, is renamed to the target only after the server confirmed every byte and the flush;
  the local file must not change while it is read (size and modification time, else `other`,
  "changed while it was read"). A failed `put` deletes its temporary file, on the same connection
  within 2 s or on one new connection; what could not be deleted is named in the error and in
  `left_behind`.
- **Overwrite.** An existing remote file is not replaced without `-f`: `already_exists` (the
  interactive shell asks). Files above 256 KiB are checked before any data is sent (2 requests,
  also with `-f`); smaller ones let the rename refuse (the temporary file is then deleted). A
  directory is never replaced. A target that appears during a `put` without `-f` is not replaced.
- **Cost on Samba**: a file of at most 256 KiB **7 requests in 3 round trips** (CREATE + WRITE +
  FLUSH, CLOSE, the rename CREATE + SET_INFO + CLOSE); refused because the target exists (no `-f`): 13 in 6 round trips; with `-f` onto an existing file
  the same 7 in 3 (`transfer_costs`).
  Larger: the check (2), CREATE and 3 QUERY_INFO, one WRITE per 512 KiB, FLUSH, CLOSE, the rename
  (3): 14 for 1 MiB + 1 byte. With `-r`: directories created level by level, 16 at a time, 2
  requests each (open-or-create: an existing directory is merged, a file of that name is
  `not_a_directory`); small files 16 at a time; larger ones one after another.
- **Not uploaded by `-r`** (one error each, the rest goes on, exit 1): symbolic links and special
  files (not followed, not read), names SMB does not allow (`invalid_name`; rsmb does not map
  them), names that differ only in letter case from a sibling (neither is uploaded), names of
  the temporary form, a directory that cannot be read (not created on the server either).
- **Output** as for `get`: `put <local> -> <remote>  <bytes> bytes  <s> s`, then the summary.
- Errors: `already_exists`, `is_a_directory`, `access_denied` (also: the server refused to
  replace the target), `disk_full`, `invalid_name`, `local_io`, `other`, `connection_lost`.
  Drops the cached listing of the target's directory (with `-r` also everything below the
  tree's root). Not run again after a lost connection.

Examples:

- `put notes.txt` — upload into the remote working directory
- `put notes.txt renamed.txt` — upload under another name
- `put notes.txt olddir/` — upload into the directory olddir (the trailing / says directory)
- `put -f notes.txt old.txt` — replace the remote file old.txt
- `put -r photos` — upload the directory photos and everything below it

## `cat`

```
cat <file>
```

Writes a remote file to stdout as it arrives: binary data unchanged, no newline added, never the
whole file in memory (the stream path, 7 requests on Samba up to 512 KiB). With `--json`: one
`content` object with the file as `text`, for a file of at most 1 MiB that is valid UTF-8; any
other file is an error of kind `other` whose message says to use `get`. A reader that closes the
pipe (`cat … | head`) ends the run with exit 1. Errors: `not_found`, `is_a_directory`,
`access_denied`. Not run again after a lost connection (it has printed while it read).

Examples:

- `cat /tree/d0/file.txt` — print a remote file

## `mkdir`

```
mkdir [-p] <dir>
```

Creates one directory: **2 requests in 1 round trip** (CREATE with the create disposition and
CLOSE in one compound). Its parent must exist (`not_found`, the message names `-p`). With `-p`
every directory of the path is opened or created from the top, 2 requests and 1 round trip per
component; one that exists is no error, and a file in the way (the last component included) is
`not_a_directory`, so `mkdir -p` never reports a directory that is not there. Errors:
`already_exists` (a directory or a file has the name), `not_found`, `not_a_directory`,
`access_denied`, `invalid_name` (a character SMB does not allow: `"*:<>?\|`, a control
character, a trailing space or period). Drops the cached listing of the parent (with `-p` of
every parent). Not run again after a lost connection: the error says it may or may not have
taken effect.

Examples:

- `mkdir newdir` — create a directory
- `mkdir -p a/b/c` — create a path of directories

## `rm`

```
rm <file>
```

Deletes one file: **3 requests in 1 round trip** (CREATE with delete access, SET_INFO, CLOSE). One
name, no pattern (`rm *` is a file called `*`, which SMB does not allow: `invalid_name`), not
recursive, never asks. A name of the temporary form (a leftover `.rsmb-….part`) is deleted like
any other. Errors: `not_found`, `is_a_directory` (the message names `rmdir`), `access_denied`,
`other` (in use by another client, or the server will not delete it). Drops the cached listing
of its directory. Not run again after a lost connection.

Examples:

- `rm old.txt` — delete one file

## `rmdir`

```
rmdir <dir>
```

Removes one empty directory: **3 requests in 1 round trip**. Errors: `not_found`,
`not_a_directory`, `not_empty` (it has entries; there is no recursive delete), `access_denied`;
the working directory or one above it is `usage` (`cd` out of it first; compared without regard
to case, as the server does: `rmdir ../CW` from `cw` is refused). Drops the cached
listings of the parent and of everything at and below the directory. Not run again after a lost
connection.

Examples:

- `rmdir olddir` — remove an empty directory

## `lcd`

```
lcd [<dir>]
```

Changes the local working directory, which `get`, `put`, `lls` and local completion use for
relative paths. Without an argument `$HOME` (not set: `usage`). A leading `~` is `$HOME`. Sends
nothing and needs no share. Errors: `local_io` with the system's text; the directory is unchanged.

Examples:

- `lcd photos` — change the local working directory
- `lcd` — go to the home directory

## `lpwd`

```
lpwd
```

Prints the local working directory (absolute). Sends nothing, needs no share. With `--json` one
`lpwd` object.

Examples:

- `lpwd` — print the local working directory

## `lls`

```
lls [<dir>]
```

Lists a local directory (default: the local working directory) in the line format of `ls`, sorted
by name, names that start with a dot included (so a leftover `.rsmb-….part` shows): `d` or `-`,
the size, the modification time in UTC, the name, `/` after a directory. A symbolic link shows
what it points to; a dangling one is a file of size 0. Sends nothing, needs no share. With
`--json` one `local_entry` object each. Errors: `local_io`.

Examples:

- `lls` — list the local working directory
- `lls photos` — list a local directory

## `refresh`

```
refresh
```

Empties the listing cache and forgets the share list, so the next Tab asks the server again. 0
requests.

Example:

- `refresh` — make the next Tab ask the server

## `help`

```
help [<command>|all]
```

`help` lists the commands; `help <command>` shows one with its examples; `help all` prints the
whole manual, the same as `rsmb --help-all`: what rsmb is, the start-up flags (from the flag
parser itself, so they cannot drift), how commands are given, quoting and paths, every command,
the `--json` objects, the exit codes, transfers, timeouts and reconnect, tree and find, two
complete examples. 0 requests.
With `--json` the manual is printed as `command`, `option`, `exit_code`, `json_object` and `topic`
objects.

Examples:

- `help` — list the commands
- `help cd` — show the command cd
- `help all` — print the whole manual

## `exit`

```
exit
```

Ends the session (0 requests). The exit code is that of the first failed command of a
non-interactive run with `-k`, otherwise 0. Ctrl-D at the prompt does the same.

Example:

- `exit` — end the session

## Exit codes

| Code | Meaning |
|---|---|
| 0 | every command succeeded (or the shell ended with exit or Ctrl-D) |
| 1 | a command failed (the first failure stops a non-interactive run unless -k) |
| 2 | usage: a bad flag, a bad command line, an unknown command, no share selected, no password without a terminal |
| 3 | connecting or logging in failed at start-up, a proxy failure included |
| 130 | interrupted: SIGINT or SIGTERM dropped a running command or the start-up connect; SIGTERM also ends the interactive shell (Ctrl-C there only returns to the prompt) |

## Timeouts and reconnect

- `--timeout` (default 30 s) is the response deadline of the SMB library; rsmb adds no timer of
  its own around requests, except the 5 s bound of a Tab. A server that has gone **silent** fails
  the request after between `--timeout` and max(`--timeout`, 15 s) with kind `connection_lost`:
  the library extends the deadline while the server's last frame is less than 15 s old. One
  **reply that keeps arriving** has to be complete within 6 × `--timeout` (180 s by default); on a
  very slow link a very large directory needs a larger `--timeout`. Connecting has its own 10 s,
  through a proxy too (the TCP connect, the SOCKS5 handshake and the proxy's own connect to the
  server); `--timeout` does not change it.
- There is no liveness probe: an idle session sends nothing. A dead connection is noticed when
  the library sees it close, or when a request fails.
- A connection that was **closed** (by the server, a NAT, a cut) is replaced by the next command
  that sends: the same dial and login as at start-up, then the share is connected again on use.
  Share, working directory and cache stay. A reading command that lost its connection midway runs
  once more from its start on a new connection; it prints only after its last request, so nothing
  is printed twice. `tree` is such a command. `find` prints while it walks, so it runs again only
  while it has printed nothing (the start's listing was lost, or nothing had matched); after its
  first line it fails with `'<line>' was not run again: the results above are incomplete`, and
  no `walk_summary` follows.
  A reconnect that fails is reported once, with its own kind (`connect` when nothing answers within
  10 s, `auth` when the login is refused) and not tried again: the command fails after 10 s, not 20.
- A server that went **silent** fails the command after the deadline; the command is not tried
  again (that would add another deadline), and the next command that sends opens a new
  connection.
- Commands that send nothing (`pwd`, `help`, `refresh`, `exit`, `cd` to the root or a directory
  above) never touch the network, dead connection or not.
- After Ctrl-C during a command that was on the wire, and after a Tab that ran into its bound,
  the connection is replaced by the next command that sends (4 round trips), because requests
  left in flight hold credits and replies the next command would otherwise wait behind. The same
  after a transfer that failed locally once data was being read (a full local disk, a closed stdout
  under `cat`); a refusal before the first byte was read (`cat --json` of a file over 1 MiB, `get`
  into a directory that cannot be written) keeps the connection.
- **In a transfer the deadline is per request, not per transfer.** Each READ or WRITE of at most
  512 KiB, each small file of at most 256 KiB and each walk listing is one reply that must be
  complete within 6 × `--timeout`, with at most 4 MiB queued in front of it: a link has to carry
  4 MiB in 180 s (23 KiB/s) with the default. A transfer may take any time.
- **Not run again after a lost connection:** `get`, `put`, `cat` (it has printed while it read),
  `mkdir`, `rm`, `rmdir`. Their error ends with `'<line>' was not run again and may or may not
  have taken effect`. `find` is not run again once it has printed; its error ends with
  `'<line>' was not run again: the results above are incomplete` (a `find` that had printed
  nothing when the connection went silent, or that started on a dead connection, gets no such
  tail: nothing is incomplete, and it changed nothing). A failed `put` may make **one** new connection inside the command, only to
  delete its temporary files (not after a silent link: there the files are named in the error and
  in `left_behind`, and the server keeps them open until it notices the dead session). When the
  connection was cut between a `put`'s rename and its answer, the message says whether the
  temporary file was still there.
- Error kinds: `not_found`, `access_denied`, `not_a_directory`, `is_a_directory`,
  `already_exists`, `not_empty` (`rmdir` of a directory with entries), `disk_full`,
  `invalid_name`, `auth`, `connect`, `connection_lost`, `interrupted`, `local_io`, `usage`,
  `other`.

## Proxy

`--proxy socks5://[user:pass@]host:port` sends every connection to the server through a SOCKS5
proxy (RFC 1928; a user name and password by RFC 1929, offered together with "no login", so a
proxy that needs none takes none).

- **The URL.** `socks5://` or `socks5h://` (the same: rsmb always lets the proxy resolve the
  name), lower case; one trailing `/` is accepted. `socks4://`, `socks4a://`, `socks://`,
  `http://`, `https://`, an upper-case scheme, no scheme, no port, port 0 or above 65535,
  anything after the port: `usage`, exit 2. User and password are split at the last `@` and the
  first `:` before it, percent-decoded, and must each be 1 to 255 bytes after decoding; a user
  without `:password`, a bad `%` escape or bytes that are not UTF-8 are `usage`. An IPv6 proxy in
  brackets: `socks5://[::1]:1080`. The message of every refused URL is the same and **never
  repeats the URL**, which may hold a password:

  ```
  --proxy: expected socks5://[user:pass@]host:port (SOCKS5 only; percent-encode special characters in user and password, put an IPv6 host in brackets)
  ```
- **Names.** The proxy resolves the server's name: `-H` goes to it as written, a name as a name
  (address type 3), an IPv4 address as an address, an IPv6 address written without brackets
  (`-H ::1`) as an IPv6 address. This host never looks the server's name up. With `--proxy`, `-H`
  is at most 255 bytes (`usage` otherwise: a SOCKS5 request carries no longer one). The proxy's
  own name is resolved on this host.
- **Every connection** goes through the proxy: the first and every reconnect (a cut, a lost
  connection, the delete after a failed `put`). Nothing else is dialled. **There is no
  fallback**: a proxy that cannot be reached is the command's error even when the server could
  be reached directly. No environment variable (`ALL_PROXY`, `all_proxy`, `SOCKS_PROXY`, ...) is
  read: the route is what the command line says.
- **Credentials** are visible in the process list (as `-p`) and never printed: not in an error,
  not in the banner, not in `--json`.
- **Failures** are kind `connect`, exit 3 at start-up (a reconnect that fails: `reconnect failed:
  …`, exit 1), one `error` object with `"command":null` and no `done` at start-up. The message
  starts `proxy <host>:<port>: `:

  | Case | Message after `proxy <host>:<port>: ` |
  |---|---|
  | the proxy's port is closed | `Connection refused (os error 111)` |
  | the proxy does not answer the TCP connect | `no answer within 10 s` |
  | the TCP connection stood, the handshake did not finish | `connected to the proxy, but the SOCKS5 handshake did not finish within 10 s (the proxy is silent, is not a SOCKS5 proxy, or cannot reach <server>:<port>)` |
  | wrong user or password | `user name or password refused` |
  | the proxy wants a login and none was given | `wants a user name and password: --proxy socks5://user:pass@<host>:<port>` |
  | credentials given, neither method accepted | `accepts neither a session without login nor a user name and password` |
  | the answer is not SOCKS5 | `not a SOCKS5 answer (is it a SOCKS5 proxy?)` |
  | the proxy closed during the handshake | `closed the connection during the SOCKS5 handshake (is it a SOCKS5 proxy?)` |
  | the proxy cannot reach the server | `cannot reach <server>:<port>: ` + `host unreachable`, `network unreachable`, `connection refused`, `timed out on the way`, `not allowed by the proxy's rules`, `the proxy does not support CONNECT`, `the proxy does not support this kind of address`, or `the proxy reports a general failure (some proxies answer an unknown host name this way)` |

  rsmb cannot tell a silent proxy from one that is not a SOCKS5 proxy or one whose connect to the
  server hangs: all three accept the TCP connection and do not finish the handshake. A refused
  SMB login through the proxy is kind `auth`, as direct, and its message does not name the proxy.
- **Time.** The whole dial (TCP connect, handshake, the proxy's connect to the server) has 10 s;
  `--timeout` does not change it.
- **Cost.** The TCP connect to the proxy and 2 round trips to it (3 with a login), once per
  connection. Every command sends the same requests in the same flights as direct
  ([`performance.md`](performance.md), "Through a SOCKS5 proxy"). A proxy that holds back small
  segments (Nagle's algorithm on its socket to the server, as microsocks does) delays what has
  many requests in flight: walks, many small files, large transfers.
- **The banner** of the interactive shell names the proxy's address, never its user:
  `logged in as 'TESTDOM\tester' on fileserver.rsmb.test:445 through proxy 10.213.45.20:1080 (SMB 3.1.1)`.

Example: `rsmb --proxy socks5://127.0.0.1:1080 -H fileserver.example -u alice -d CORP -N -C 'use docs' -C 'ls'`.

## Differences from smbclient-ng

- smbclient-ng has no proxy option; rsmb has `--proxy socks5://…`.
- `shares` always shows the access to each share, from the TREE_CONNECT answer (1 more round
  trip for the whole list). smbclient-ng shows rights only with `shares -R`: per share a tree
  connect and a listing of the root, one after another, and it reports READ or NO ACCESS (its
  write test, which creates and deletes a directory, is off there).
- No liveness probe before each prompt, no new TCP connection per command, `cd` without a
  listing, listings in one round trip, cached completion.
- `pwd` prints `//host/share/path`; `refresh` exists because listings are cached.
- `ls` shows times in UTC, sorted by name, `/` after directories.
- Exit codes are fixed (0, 1, 2, 3, 130); a non-interactive run stops at the first failure unless
  `-k`; `--json` gives machine-readable output.
- rsmb never prompts when stdin is not a terminal.
- `get -r <dir>` keeps the structure below the named directory: `get -r /tree/d0` makes `./d0/…`.
  `put -r <dir> <remote>` puts `<dir>` into `<remote>`.
- Transfers write to a temporary `.rsmb-….part` and rename it when complete; nothing is replaced
  without `-f` (smbclient-ng overwrites). A recursive transfer reports every item it could not
  transfer and exits 1.
- Names SMB does not allow are refused instead of being mapped to look-alike characters.
- `tree` and `find` list 16 directories at a time (smbclient-ng: one after another, 4 round
  trips each). `find` prints paths from the share root and not the start itself; `-size` is in
  bytes and strict (smbclient-ng's `+` and `-` include the boundary); no `-mindepth`, `-ls`,
  `-download`, `--exclude-dir`; one start directory. A directory that denies listing is reported
  at the end and the walk goes on, exit 0.
