# Scripting rsmb

How a program (a script, an AI agent, the test harness) drives rsmb. Everything here is also in
the binary: `rsmb --help-all` (or `--help-all --json`) needs no server. The commands themselves
are in [`commands.md`](commands.md).

## Giving commands

Three ways, in this order:

1. `-S <file>`: the lines of the file.
2. `-C <line>`, repeatable: each in order.
3. stdin, one command per line until end of input, **only when neither `-C` nor `-S` was given and
   stdin is not a terminal, or it is one and `--json` was given**.

After the commands: if stdin is a terminal and neither `-N` nor `--json` was given, the
interactive shell starts; otherwise rsmb exits. So `rsmb … -C ls` with an inherited pipe on stdin that nobody closes still
exits by itself, and `-N` is only needed to suppress the shell when running from a terminal.

- Blank lines and lines whose first non-blank character is `#` are skipped and print nothing.
- The run stops at the first failing command, with that command's exit code. With
  `--keep-going` (long form only) it goes on and exits with the code of the first failure. `exit`
  ends the run.
- If stdout cannot be written (the reader closed the pipe, `| head -1`) the run ends at once with
  exit 1 and `rsmb: cannot write to stdout` on stderr, also with `--keep-going`.
- rsmb never prompts when stdin is not a terminal: a user name without `-p`, `--no-pass` or `-k` is
  a usage error (exit 2) before anything is sent. `-k` (Kerberos from the ticket cache) never
  prompts and needs no `-p`.
- **`-k` changed its meaning.** It used to be the short form of `--keep-going`; it is now
  Kerberos. An old script that passes `-k` with `-p` or `--no-pass` now exits 2 (`usage`); one
  without either (an anonymous run such as `rsmb -H h -N -k -C ls`) is now a Kerberos login as the
  principal of the default cache: an `auth` error when there is no cache, a login as that
  principal when there is one. Write `--keep-going`.
- `RSMB_HISTORY=` (empty) disables the history file; scripts do not need it.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | every command succeeded (or the shell ended with exit or Ctrl-D) |
| 1 | a command failed (the first failure stops a non-interactive run unless --keep-going) |
| 2 | usage: a bad flag, a bad command line, an unknown command, no share selected, no password without a terminal |
| 3 | connecting or logging in failed at start-up, a proxy or Kerberos failure included |
| 130 | interrupted: SIGINT or SIGTERM dropped a running command or the start-up connect; SIGTERM also ends the interactive shell (Ctrl-C there only returns to the prompt) |

## `--json`

Every stdout line is one JSON object with a `type`. Nothing else is written to stdout, and stderr
stays empty. Key order is not fixed (the objects below happen to be sorted).

**Names are exact in `--json`.** A name, path or comment from the server is the string the server
sent (an unpaired UTF-16 surrogate is U+FFFD), with only JSON's own escaping; control and bidi
characters are **not** replaced, unlike in text mode where they are shown as `\u{9b}`. A program
that prints a name to a terminal must filter it itself; a name from `--json` can be passed back to
`cd`, `get` or `rm` as it is.

- After **every command line that was run**, one `done` object is the last line of that command:
  `{"command":"<the line, trimmed>","ok":true|false,"type":"done"}`. A failed command prints an
  `error` first. Skipped lines (blank, `#`) print nothing.
- A failed start-up (connect, login, usage) prints one `error` with `"command":null` and no `done`.
- A flag error (`rsmb --json --bogus`, a missing `-H`, a bad value) is also one `error` object,
  kind `usage`, on stdout, exit 2, stderr empty; `--json` can stand anywhere among the arguments.
  Without `--json` clap's text goes to stderr and stdout stays empty. `--help` and `--version`
  print their usual text, with or without `--json`.
- **`--json` never starts the line editor**, whatever stdin is, so every stdout line is one JSON
  object. With a terminal on stdin and no `-C` or `-S`, lines typed there are read one by one like
  lines from a pipe, **under the rules of a non-interactive run**: no prompt, no history, no
  completion; the first failing command ends the session with its exit code unless `--keep-going`; Ctrl-C
  during a command ends the run with 130 after the cleanup; Ctrl-C between commands exits 130;
  nothing asks to overwrite (`already_exists`); Ctrl-D ends it. A password prompt (no `-p`, a
  user name) still goes to the terminal device, not to stdout. With `-C` or `-S` the commands run
  and rsmb exits, as with `-N`.

| `type` | Fields | Printed by |
|---|---|---|
| `share` | `name`, `share_type` (`disk`, `printer`, `device`, `ipc`, `unknown`), `hidden` (name ends in `$` or type bit 31), `comment`, `access` (`"rw"`, `"r"`, `"w"`, `"none"`: the share-level access the server reports for this login, `none` when it refuses the TREE_CONNECT; `null` when it gave no answer for that share, which includes every share not answered when the connection was lost during the probe: the `share` objects are then followed by an `error` of kind `connection_lost` and the exit code is 1. Not a promise for every directory or file inside, see `commands.md`) | `shares` |
| `entry` | `name`, `path` (from the share root, starting with `/`), `is_dir`, `size` (integer), `mtime` (RFC 3339 in UTC, or `null`), `file_id` (decimal string: 64 bits do not fit every JSON reader's numbers); from `tree` and `find` also `depth` (integer, 1 = directly in the start directory) | `ls`, `tree`, `find` |
| `denied` | `path`: a directory of a walk whose listing the server refused; what is in it is unknown | `tree`, `find`, after the entries |
| `skipped` | `path`, `reason` (`already_listed`: a directory with its file id was listed already, a link back to the start or a second path to one directory; `outside`: it leads to the start's parent directory, which the walk does not list; `gone`: it vanished between its parent's listing and its own; `no_file_id`: the server gives no file ids and the walk is at its limit), `same_as` (for `already_listed` the path under which that directory was listed, else `null`; never a path the walk did not list) | `tree`, `find`, after the `denied` objects |
| `walk_summary` | `command` (`tree` or `find`), `path` (the start), `directories` (listings that succeeded, the start included), `entries` (every entry those listings held), `matched` (the `entry` objects printed), `denied`, `skipped`, `failed` (integers), `seconds` | `tree`, `find`: once, **only after a walk that ran to its end** |
| `access` | `name` (`""` for the share root), `path` (from the share root, starting with `/`), `is_dir`, `mask` (integer: the MaximalAccess the server computed for this login, or `null`), `attributes` (integer: the file attributes of the server's answer, 16 directory, 1 read-only; or `null`), `error` (an error kind when this entry of `access -e` could not be asked about, else `null`); a directory has `ls`, `put` (a new file into it), `mkdir`, `rmdir`, a file `get`, `replace` (`put -f` over it), `rm`, each `true` (allowed), `false` (refused) or `null` (unknown), never the other type's keys | `access`: one per path, all after the last answer |
| `pwd` | `host`, `share` (or `null`), `path` (`/` or `/a/b`) | `pwd` |
| `error` | `kind`, `message`, `command` (or `null` at start-up); `path` (string: only for an error about one item of a recursive transfer, the remote path for `get`, the local path for `put`); `left_behind` (array of strings: only when temporary files could not be removed, and only on the error of the command that left them, never again on a later one; remote ones as `/dir/.rsmb-….part` on the selected share, local ones as absolute paths). Each extra field is present only when it has something to say | any failure |
| `transfer` | `direction` (`get` or `put`), `remote` (from the share root, starting with `/`), `local` (absolute path), `bytes` (integer), `seconds` (number) | `get`, `put`: one per file that arrived, written as it arrives |
| `summary` | `direction`, `files` (arrived), `bytes`, `failed` (not transferred: files, directories and other entries), `directories`, `seconds` | `get -r`, `put -r`: once at the end, also after a fatal error, **not** after an interrupt |
| `content` | `path`, `size`, `text` | `cat` with `--json` (at most 1 MiB of UTF-8) |
| `local_entry` | `name`, `path` (absolute), `is_dir`, `size`, `mtime` (RFC 3339 in UTC, or `null`) | `lls` |
| `lpwd` | `path` | `lpwd` |
| `done` | `command`, `ok` | after every command line |
| `command` | `name`, `syntax`, `summary`, `details`, `flags` (array of `{flag, help}`), `examples` (array of `{line, does}`) | `help`, `help <cmd>`, `help all`, `--help-all` |
| `option` | `flag` (`-H, --host <HOST>`), `help` | `help all`, `--help-all` |
| `exit_code` | `code`, `meaning` | `help all`, `--help-all` |
| `topic` | `name`, `text` | `help all`, `--help-all` |
| `json_object` | `name` (a value of `type`), `fields` | `help all`, `--help-all` |

Error kinds: `not_found`, `access_denied`, `not_a_directory`, `is_a_directory`, `already_exists`,
`not_empty`, `disk_full`, `invalid_name`, `auth`, `connect`, `connection_lost`, `interrupted`,
`local_io`, `usage`, `other`. Match on the kind, not the message.

**Transfers.** A recursive transfer prints, in this order: `transfer` and item `error` objects (with
`path`) as they happen, one `summary`, one final `error` if anything was not transferred (kind
`other`, or the kind of the fatal event: `connection_lost`, `disk_full`, `local_io`), and `done`.
The exit status is 1 if anything was not transferred. `transfer` and `summary` lines are flushed as
they are written, so a program can read a long transfer line by line. **No transfer ever prompts
without a terminal**: an existing target without `-f` is `already_exists`. An interrupted transfer
prints `interrupted` (with `left_behind` if a temporary file could not be removed) and no summary;
the `transfer` lines already written are the record of what arrived.

**Walks** (`tree`, `find`). The objects come in this order: the `entry` objects (`find`: as each
directory's listing is taken, flushed per directory; `tree`: all after the walk), every `denied`
then every `skipped` (each sorted by path), an `error` with `path` for any other directory that
failed, one `walk_summary`, then, if a directory failed, the command's `error` (kind `other`,
`incomplete`), and `done`. **No `walk_summary` means the result is not whole**: after an
interrupt, a lost connection (`find` once it has printed: the error says the results are
incomplete), `tree`'s limit of 500,000 entries or a closed stdout, the command ends without it
and `done` has `ok: false` (or the run ends). Denied and skipped directories leave `ok` true and
the exit status 0; read `walk_summary.denied` and `.skipped`. The order of `find`'s `entry`
lines is **the same on every run**: breadth first, the start's entries, then the entries of
each of its directories in name order, and so on; a directory's own line before what is in it.
`tree`'s order is the drawing's: depth first, by name.

**Asking before acting** (`access`). A column says what the server would allow at that moment,
for this login: a directory's `put` means "a new file into it", a file's `replace` means "`put -f`
over it". `null` means rsmb cannot tell, which a script treats as "try it". Act and check the exit
code all the same: the answer is not a lock, and `known-limitations.md` names the cases where it
and the command differ (a drop box whose inherited permissions keep you from renaming your own new
file says `put: true`, and `put` then fails with `access_denied` and `left_behind`). A path that
cannot be asked about is the command's error when it is given alone (exit 1, an `error` object),
and an `access` object with `error` set as an entry of `access -e` (exit 0). A large `access -e`
prints nothing until it has every answer (`/huge`, 100,000 entries: about 22 s on a fast link).

Example lines (from the test server):

```
{"access":"r","comment":"","hidden":false,"name":"readonly","share_type":"disk","type":"share"}
{"access":"r","comment":"IPC Service (Samba 4.23.8)","hidden":true,"name":"IPC$","share_type":"ipc","type":"share"}
{"command":"shares","ok":true,"type":"done"}
{"host":"10.213.45.10","path":"/","share":"share","type":"pwd"}
{"file_id":"67539330","is_dir":true,"mtime":"2026-11-17T22:07:08Z","name":"d0","path":"/tree/d0","size":0,"type":"entry"}
{"command":"cd nope","kind":"not_found","message":"/nope: not found","type":"error"}
{"command":"cd nope","ok":false,"type":"done"}
{"command":null,"kind":"connect","message":"connect to 127.0.0.1:1: Connection refused (os error 111)","type":"error"}
{"command":null,"kind":"connect","message":"proxy 10.213.45.20:1081: user name or password refused","type":"error"}
{"command":null,"kind":"auth","message":"Kerberos: ticket cache /tmp/krb5cc_rsmb: No such file or directory (os error 2)","type":"error"}
{"bytes":65537,"direction":"get","local":"/home/user/65537.bin","remote":"/sizes/65537.bin","seconds":0.004,"type":"transfer"}
{"command":"get -r /loops","kind":"other","message":"/loops/self: not copied: the same directory as /loops (a link, or a server that reuses file ids)","path":"/loops/self","type":"error"}
{"bytes":5,"direction":"get","directories":1,"failed":2,"files":1,"seconds":0.007,"type":"summary"}
{"command":"get -r /loops","kind":"other","message":"get -r /loops: 2 of 3 not transferred","type":"error"}
{"path":"/tree/d0/file.txt","size":8,"text":"tree/d0\n","type":"content"}
{"attributes":16,"error":null,"is_dir":true,"ls":true,"mask":2032127,"mkdir":true,"name":"upload","path":"/upload","put":true,"rmdir":null,"type":"access"}
{"attributes":128,"error":null,"get":true,"is_dir":false,"mask":1507743,"name":"mine.txt","path":"/upload/s/mine.txt","replace":true,"rm":true,"type":"access"}
{"attributes":null,"error":"other","get":null,"is_dir":false,"mask":null,"name":"b.fifo","path":"/upload/s/b.fifo","replace":null,"rm":null,"type":"access"}
{"depth":1,"file_id":"68435286","is_dir":true,"mtime":"2026-11-18T10:31:12Z","name":"self","path":"/loops/self","size":0,"type":"entry"}
{"path":"/tree/forbidden","type":"denied"}
{"path":"/loops/self","reason":"already_listed","same_as":"/loops","type":"skipped"}
{"path":"/loops/up","reason":"outside","same_as":null,"type":"skipped"}
{"command":"find","denied":0,"directories":1,"entries":3,"failed":0,"matched":3,"path":"/loops","seconds":0.001,"skipped":2,"type":"walk_summary"}
{"is_dir":true,"mtime":"2026-11-18T08:00:00Z","name":"photos","path":"/home/user/photos","size":0,"type":"local_entry"}
{"path":"/home/user","type":"lpwd"}
{"command":"put medium.bin","kind":"connection_lost","left_behind":["/upload/.rsmb-9c48faf373ec2be1.part"],"message":"connection lost: no answer from the server for 15 s; left on the server: /upload/.rsmb-9c48faf373ec2be1.part; remove with rm once the server has let go of them; 'put medium.bin' was not run again and may or may not have taken effect","type":"error"}
```

## Through a SOCKS5 proxy

Add `--proxy socks5://[user:pass@]host:port`; everything else is the same, the per-command
costs too. A failure of the proxy is a start-up `error` of kind `connect` (exit 3) whose message
starts `proxy <host>:<port>: ` and says whether the proxy could not be reached, refused the login
or could not reach the server ([`commands.md`](commands.md), "Proxy"); a refused SMB login through
the proxy is kind `auth`. When a standing session loses its connection and the proxy cannot be
reached any more, the command fails with kind `connect`, `reconnect failed: proxy …`, exit 1;
rsmb never falls back to a direct connection. No environment variable is read for a proxy.

```bash
rsmb --proxy socks5://tester:testpass@10.213.45.20:1081 -H fileserver.rsmb.test \
     -u tester -p testpass -d TESTDOM --json -C 'use share' -C 'ls /sizes'
echo "exit code $?"     # 3 with {"kind":"connect","message":"proxy …"} if the proxy failed
```

## With Kerberos (`-k`)

`-k` logs in from the ticket cache: no `-p`, no prompt, user and realm from the cache
([`commands.md`](commands.md), "Kerberos"). rsmb reads two environment variables for it:
`KRB5CCNAME` (the cache file: `FILE:<path>` or a path; not set: `/tmp/krb5cc_<uid>`) and
`KRB5_CONFIG` (the `krb5.conf` files that may name the realm's KDC; not set: `/etc/krb5.conf`;
without a `kdc` there, the realm's name in lower case, port 88). A cache that holds the service
ticket for `cifs/<-H>` needs no KDC at all.

A Kerberos failure at start-up is the one `error` object with `"command":null`, exit 3, no
`done`, and a message that starts `Kerberos: `. Its kind says what to do:

- `auth`: the cache (missing, another type, another owner, not a cache, expired, no ticket for
  the server, another principal than `-u`/`-d`) or a refusal by the KDC or the server. Get new
  tickets (`kinit -f`, `kvno cifs/<host>`) or fix the command line; retrying the same does not
  help.
- `connect`: the KDC could not be asked (refused, no answer within 10 s, closed, or the proxy
  could not reach it). Retry later, or name the KDC with a `krb5.conf` of your own in
  `KRB5_CONFIG`.

On a reconnect the command's `error` says `reconnect failed: Kerberos: …`, exit 1. A session ends
when its ticket does: a reading command is run again on a new login that reads the cache as it
then is (it works with new tickets; with the old ones it fails with kind `auth`, `reconnect
failed: Kerberos: the ticket for … expired at …; run kinit`), and a command that changes
something (`mkdir`, `put`, `rm`, …) fails with `connection_lost` and is not run again; the next
command logs in again.

```bash
kinit -f alice@CORP.EXAMPLE                     # or: KRB5CCNAME=/path/to/cache
rsmb -k -H fs1.corp.example --json -C 'use docs' -C 'ls'
echo "exit code $?"     # 3 with {"kind":"auth","message":"Kerberos: …"} if the cache is no good
```

## Driving rsmb as a coprocess

Start one process with `--json --keep-going` and a pipe on stdin; write one command line, read
lines until the `done` line, repeat. The session (connection, share, working directory, cache)
lives as long as the process, so every command after the first costs only its own round trips.
Close stdin (or send `exit`) to end it. `--keep-going` keeps the session alive after a failed
command.

## Examples

bash, one-shot:

```bash
rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'cd tree/d0' -C ls
echo "exit code $?"
```

bash, JSON, names of the entries:

```bash
printf 'use share\nls /sizes\n' |
  rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM --json |
  python3 -c 'import json,sys; [print(o["name"]) for o in map(json.loads, sys.stdin) if o["type"] == "entry"]'
```

python, as a coprocess:

```python
import json, subprocess

p = subprocess.Popen(
    ["rsmb", "-H", "10.213.45.10", "-u", "tester", "-p", "testpass", "-d", "TESTDOM",
     "--json", "--keep-going"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

def cmd(line):
    p.stdin.write(line + "\n"); p.stdin.flush()
    out = []
    for raw in p.stdout:
        obj = json.loads(raw)
        if obj["type"] == "done":
            return obj["ok"], out
        out.append(obj)

cmd("use share")
ok, objs = cmd("cd nope")          # ok is False, objs[0]["kind"] == "not_found"
ok, objs = cmd("ls /tree")
print([o["name"] for o in objs if o["type"] == "entry"])
p.stdin.close(); p.wait()
```

bash, find files by pattern and fetch them (the paths are from the share root, so each works as
an argument from any working directory; `jq -r` gives one path per line):

```bash
rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM --json \
     -C 'use share' -C 'find /sizes -type f -size +1M -name "*.bin"' |
  jq -r 'select(.type == "entry") | .path' >paths.txt
# check that the walk was whole before trusting the list: a walk_summary line was printed
args=(); while read -r p; do args+=(-C "get -f '$p'"); done <paths.txt
rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' "${args[@]}"
```

Quoting: the pattern goes through the shell and then through rsmb's tokeniser; `-C "find -name
'*.txt'"` hands rsmb `find -name '*.txt'`, which rsmb takes as the pattern `*.txt` (it does no
globbing, so `*.txt` unquoted works inside rsmb too). A path with a `'` in it needs the
tokeniser's own quoting (`it\'s.txt`).

bash, may I put a file into /upload, and what may I change in /tree (output from the test
server; `/tree` belongs to root, so nothing in it may be changed):

```bash
rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM --json -N \
     -C 'use share' -C 'access /upload' -C 'access -e /tree' |
  jq -c 'select(.type == "access") | if .is_dir then {path, put} else {path, replace, rm} end'
# {"path":"/upload","put":true}
# {"path":"/tree/d0","put":false}
# {"path":"/tree/d1","put":false}
# {"path":"/tree/d2","put":false}
# {"path":"/tree/d3","put":false}
# {"path":"/tree/forbidden","put":false}
# {"path":"/tree/unreadable.txt","replace":false,"rm":false}
# a value null means rsmb cannot tell: try it and check the exit code
```

bash, upload a directory and check the summary:

```bash
rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM --json --keep-going \
     -C 'use share' -C 'cd upload' -C 'put -r photos' |
  jq -c 'select(.type == "summary" or (.type == "error" and .path != null))'
# {"bytes":…,"direction":"put","directories":…,"failed":0,"files":…,"seconds":…,"type":"summary"}
# exit code 0 only if failed is 0; otherwise one error object per item, with its path
```
