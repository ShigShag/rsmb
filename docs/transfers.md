# Transfers

How `get`, `put` and `cat` move data, and every rule they keep. The design is in
[`plans/stage-2.md`](plans/stage-2.md) sections 5.3 to 5.12; this document states the rules as the
code has them, each with the test that enforces it (test names are in `tests/fast.rs`,
`tests/slow.rs`, `tests/serial.rs` and the unit tests; `docs/testing.md` lists them all). The
words request, flight and outstanding are those of [`measuring.md`](measuring.md).

**The promise:** under a final name there is only ever a complete file or the file that was there
before; nothing is overwritten without `-f`; nothing is reported as a success that did not fully
arrive; every leftover is removed or named.

Every rule here holds through a SOCKS5 proxy (`--proxy`) as it does direct: the transfer runs on
the same one connection with the same requests, and a cleanup's new connection goes through the
proxy like every other (`big_file_through_the_proxy`, `cut_through_the_proxy`,
`signals_through_the_proxy`, and the whole of `tests/fast.rs` run a second time through the proxy).

## The two paths

| Path | Used for | Down | Up |
|---|---|---|---|
| **stream** | every single-file `get` and `cat`; a single `put` of more than the threshold; in a recursive transfer every file above the threshold | `Tree::download`: CREATE + 4 QUERY_INFO in one flight, READs of 512 KiB (the server's read size if smaller) with at most 4 MiB requested and not delivered, CLOSE | a check of the target (CREATE + CLOSE, 2 requests), `Tree::create_file_writer_exclusive` (CREATE + 3 QUERY_INFO), WRITEs of 512 KiB with at most 4 MiB unconfirmed, FLUSH, CLOSE, then the rename |
| **small** | in a recursive transfer every file of at most the threshold; a single `put` of at most the threshold | `Tree::read_file_compound_sized_with_info`: CREATE + READ + CLOSE in one compound, 3 requests, 1 flight | `Tree::write_file_compound_exclusive` (CREATE + WRITE + FLUSH, then CLOSE: 4 requests, 2 flights), then the rename (3 requests, 1 flight): 7 requests, 3 flights |

- **The threshold** is 256 KiB, lowered to the server's negotiated read and write size (64 KiB on
  `samba-edge`). `threshold_boundary` puts 262,143, 262,144 and 262,145 bytes and counts the path
  each took.
- A single `get` always streams: its size is not known without asking. In a recursive transfer the
  listing has the sizes. A file that has grown past the threshold since the walk moves to the
  stream path (the compound read answers "too large").
- No compound of our own carries file data; rsmb has no chunk scheduler. All `smb2` calls are in
  `src/session.rs` (`Remote`).

**In memory:** stream path: the crate's window (at most 4 MiB), one chunk, and for an upload the
512 KiB read buffer. Small path: at most 16 files of at most 256 KiB (an upload twice: the crate
copies the data into its request). A recursive transfer also holds its file list (about 150 bytes
a file). Asserted: peak RSS ≤ 64 MB for `big.bin` each way (`big_file`), for 2,000 small files
(`two_thousand_small_files`), for `cat` of 50 MiB (`cat_output`).

**In flight:** the three phases of a recursive transfer never overlap. Walk: at most 16 listings
(64 requests on Samba). Small files: at most 16 (48 requests). Stream: the crate's window.
Asserted: ≤ 64 down and ≤ 48 up for `smallfiles/` (`two_thousand_small_files`), ≤ 16 for `big.bin`
(`big_file`). On `samba-edge` the bound is the server's credits: the client never spends a credit
it was not granted (`edge_and_encrypted`; Samba itself grants a few more than its configured 16
at moments, so up to 19 were seen outstanding there).

## Temporary names

- `.rsmb-<16 lower-case hex digits>.part`, the digits being the 64-bit FNV-1a hash of the final
  file name's UTF-8 bytes (the name, not the path). 27 bytes, so it fits wherever a final name of
  27 bytes or more fits. Always in the directory of the final name, so the rename never crosses a
  file system or a share (`fnv_vectors`, `temp_names`).
- Derived from the final name, so a second attempt replaces what a killed first attempt left
  (locally by removing the stale file and making a new one, on the server by writing over it) and
  the rename removes it (`kill_9_leaves_only_temporaries`).
- **Locally** the temporary file is created exclusively (`O_CREAT|O_EXCL`, so no link is followed
  and the file is new, with the default mode and the user's ownership) and locked
  (`File::try_lock`). **A file already under that name is never adopted**, whoever made it and
  whatever its mode: it is opened without following a link and its lock is tried. If another
  rsmb holds the lock, this transfer fails at once with "another transfer is writing this file"
  and touches nothing (`two_processes_one_target`, `local_temp_lifecycle`,
  `local_temp_is_never_adopted`); a live transfer is never unlinked under its hands, because it
  holds the lock from the moment after it created the file and checks that the path still names
  its file. If the lock is free the file is stale (a crash, or planted): its name is removed (not
  its content: nothing is written to it) and the file is created again, up to three rounds. A
  file system without locks gets no such protection (the lock error is ignored on purpose). After
  the lock the path must still name the file just made (device and inode); the window between
  another rsmb's removal of a stale name and this one's lock is closed by that check and the
  retry. The file has one link by construction (a temporary name that is a second link of a
  complete file, left by a crash between the hard link and the unlink of a commit, is removed
  like any stale one, never truncated). The lock is held until after the rename.
- **On the server** the temporary file is created create-only (`FileCreate`: the crate's
  `write_file_compound_exclusive` and `create_file_writer_exclusive`), so an upload never writes
  through whatever already has the name, a link included. If the CREATE finds the name taken (a
  stale temporary of a killed transfer, or an object of someone else), that object is removed and
  the create repeated once: the removal opens it with `FILE_OPEN_REPARSE_POINT`, so a symbolic link
  is deleted itself and not what it points to (a plain delete on Samba deletes the target); a
  second collision, or a removal the server refuses (the file is still open on the server:
  "in use"), fails the item with nothing changed. The common case costs nothing extra; the
  collision costs one failed CREATE compound, the removal (2 requests) and the repeat
  (`link_at_the_temporary_name`, `kill_9_leaves_only_temporaries`, `silent_link_mid_put`). A stream
  upload opens with no sharing: while one upload writes it, another open of it fails ("in use",
  `two_processes_one_target`). **Not covered:** a *dangling* link at the temporary name
  (`ln -s nothere .rsmb-….part`): Samba then creates the link's target and the final name becomes
  that link, because the create-only open does not see an object that "is not there"; closing this
  would cost one more request and round trip on every `put` (a create with
  `FILE_OPEN_REPARSE_POINT` before the write).
- **A name of this form is never transferred.** A leftover is a partial file by definition, and it
  may be the temporary name of another file in the same transfer. A single `get` or `put` whose
  source or target name has the form fails with `invalid_name` before anything is sent; in a
  recursive transfer each such entry is an item error, and a directory of that name is left out.
  `cat`, `rm`, `ls` and `lls` treat it like any name, so a leftover can be looked at and removed
  (`structure_round_trip`, `local_walk_rules`).
- `ls` and `lls` show such files. They are safe to delete when no transfer is running.

## The rename

**Local (`get`).** `sync_data` on the temporary file, then:

- with replace (`-f` was given **and** the target existed when the transfer started): a
  directory, a device or a pipe at the target is never replaced (`is_a_directory` / `local_io`);
  otherwise `rename` (atomic; a symbolic link at the target is replaced, its target untouched);
- without: `hard_link(temp, target)`, which fails if anything has the name however it got there,
  then the temporary name is removed. A file system without hard links falls back to "exists?
  then rename", with a window in which a file that appears is replaced.

A target that appears during the transfer is therefore never replaced, with or without `-f`
(`target_appears_during_get`, `local_temp_lifecycle`). On a local file system that folds case or
normalises names this also keeps two server names that fold into one from overwriting each other:
the second fails with `already_exists`. Two files of one recursive `get` whose existing targets are
the same local file (device and inode) are both refused (`invalid_name`), with or without `-f`
(`same_local_file_twice`). The parent directory is not synced.

**Remote (`put`).** `Tree::rename_with(temp, final, replace)`, one compound (CREATE, SET_INFO,
CLOSE). Success takes the name out of the registry. Any failure deletes the temporary file, then
words the error by one rule (`rename_errors_have_one_wording`):

| Refusal | Target found by a `stat` (2 requests) | Error |
|---|---|---|
| collision | a directory | `is_a_directory`: `<final>: is a directory; write '<final>/' to put the file into it`, with or without `-f` (Samba refuses a rename onto a directory even with replace) |
| collision, no `-f` | a file | `already_exists`: `<final>: exists on the server; -f replaces it` |
| collision, `-f` | a file | `other`: `<final>: the server did not replace it` |
| access denied, `-f` | — | `access_denied`: `<final>: the server refused to replace it (open in another program, read-only, or not permitted)` |
| anything else | — | the server's error |

A refused rename costs the crate one more request: its compound CLOSE was cascaded, so it closes
the handle on its own. A small `put` refused because the target exists (without `-f`) is therefore 13 requests in
6 flights, and with `-f` onto an existing file 7 in 3 (`transfer_costs`). With replace the server decides what is replaced; it cannot be told
"only what was there before" (`target_appears_during_put` covers the case without `-f`).

## Overwrite rules

| Case | Without `-f` | With `-f` |
|---|---|---|
| `get`, local target exists | `already_exists`, 0 requests, exit 1; in the interactive shell `Overwrite? [y/N]` | replaced by the rename when the download is complete |
| `get`, the target appears during the download | `already_exists` from the hard link; the file that appeared untouched | the same (only what was there at the start is replaced) |
| `get`, the target is a directory | `is_a_directory`, 0 requests | the same |
| `put`, large, remote target exists | `already_exists` from the check, 2 requests, nothing created | replaced by the server in the rename |
| `put`, small, remote target exists | `already_exists` from the rename; the temporary file deleted | replaced |
| `put`, the remote target is a directory (no trailing `/`) | `is_a_directory` with the hint to write `<name>/`: large from the check, small from the rename and the `stat` after it | the same |
| `put`, the target appears during the upload | `already_exists` from the rename; the file that appeared untouched | replaced |
| recursive, a file target exists | one `error` (`already_exists`, with `path`) per file, the rest goes on, exit 1, no prompt | replaced, file by file |
| recursive, a directory target exists | used as it is: directories are merged, never replaced | the same |

`-f` does not clear a read-only attribute and does not wait for another program to close the
target; it does not carry over the replaced file's permissions or times. Nothing asks for
confirmation of `-f` on a recursive transfer or of `rm` (the user's decision of 2026-11-18). The
prompt exists only for a single `get` or `put` in the interactive shell; the answer is not added to
the history, Ctrl-C and Ctrl-D mean no (`overwrite_prompt`, `overwrite_rules`).

## What "complete" means

| Transfer | Checked before the rename |
|---|---|
| `get`, stream | bytes written = the size the server gave at open; then `sync_data` |
| `get`, small | data length = the size in the same answer; then `sync_data` |
| `put`, stream | bytes confirmed by the server (`finish()`; a WRITE answered short is an error) = bytes read = the file's size when opened; size and modification time unchanged at the end; the FLUSH succeeded |
| `put`, small | the count the write returns = the length; size and modification time unchanged; the FLUSH succeeded |

A file that shrinks during a download ends the crate's download without an error; rsmb fails it
(`other`, "changed during the transfer", `file_shrinks_during_get`). A local file that changes
during an upload fails it (`source_changes_during_put`). **Not detected:** a remote file that grows
during a download (it arrives with the size it had at open) and a remote file rewritten in place
with the same size (it arrives as a mix; the crate opens it sharing read, write and delete). No
checksum is computed and nothing is read back; the tests compare sha256 on both sides.

## Recursive transfers

`get -r <dir> [<local dir>]` and `put -r <dir> [<remote dir>]` copy the directory and everything
below it **into** the second argument (default: the working directory), keeping the structure;
`<local dir>` must exist. Three phases, never overlapping:

1. **Walk.** `get -r`: `src/walk.rs`, 16 listings in flight, each 4 requests in 1 flight (CREATE,
   one query of up to 1 MiB, a 64 KiB probe, CLOSE), results taken in the order the listings were
   queued (since Stage 3; the same walk as `tree` and `find`, `architecture.md`), so which of two
   paths to one directory is copied is the same on every run. A directory whose file id was seen
   before is not entered and reported (`loops/self`, `loops/up`; `link_loops`,
   `walk_visits_each_directory_once`);
   a server without file ids is walked to depth 100 and at most 100,000 such directories, and
   what is cut is reported (`walk_depth_cap_without_ids`, `walk_breadth_cap_without_ids`). `put -r`:
   the local walk, in one blocking task: entries sorted by name; directories entered; regular files
   collected; symbolic links and special files neither followed nor read (an error each); a
   directory already entered by device and inode (a bind mount, a looping file system) reported
   and not entered (`local_walk_rules`). The file list is sorted by relative path, so the order is
   the same in every run.
2. **Small files**, 16 in flight. A downloaded small file is written, synced and renamed in one
   blocking task.
3. **Large files**, one after another, on the stream path.

Local directories of a `get -r` are made one component at a time, never through a symbolic link:
a link where a directory is to be is one error, and its subtree is left out and counted
(`local_dirs_never_through_links`, `get_r_keeps_inside_its_destination`). Remote directories of a
`put -r` are opened or created level by level (open-or-create: an existing directory is merged, a
file of that name is `not_a_directory`, and its subtree is left out; `mkdir_rules`). Empty
directories are created on both sides (`structure_round_trip`).

**Not transferred, one `error` with `path` each, the transfer goes on:** a directory that denies
access, a link back to a directory already visited, a directory cut by the caps, a name the local
file system cannot have (empty, `.`, `..`, containing `/`, `\` or NUL), a name of the temporary
form, an existing target without `-f`, an unreadable file, a file gone since the walk
(`file_vanishes_during_get_r`), on upload: a symbolic link, a special file, a name SMB does not
allow (`"*:<>?\|`, a control character, a trailing space or period; rsmb does not use the crate's
private-use mapping), names in one directory that are equal after `to_lowercase()` (all of them,
directories with their subtrees; `case_collisions`, `names_round_trip`), a local directory that
cannot be read (not created on the server either; `recursive_put_refuses_and_goes_on`), and the
root itself: that is the command's error (`local_io`) before the server is touched.

**Exit status:** 0 only if every directory and file the walk found was transferred; otherwise,
after the rest, one final error of kind `other` (`get -r <dir>: <n> of <m> not transferred`), exit
1. **Fatal**, ending the transfer: a lost connection, a full local disk, `disk_full` from the
server. What is in flight is awaited, the summary of what arrived is printed, and the fatal error
is the command's error. Files already complete stay. **An interrupt** drops the command: no summary;
the `transfer` lines already written are the record (`signal_in_the_small_file_phase`).

Output order with `--json`: `transfer` and item `error` objects as they happen, `summary`, the
final `error`, `done`.

Files that change during the walk: a file is transferred as it is when its turn comes; gone →
`not_found` for that file; grown past the threshold → the stream path; now a directory →
`is_a_directory`. A directory created after its parent was listed is not seen.

## Failure, interruption, cleanup

**The registry and the writer slot.** The session keeps the paths of remote temporary files that
exist or may exist (added before the CREATE that makes one; taken out when the rename was
confirmed, a delete succeeded or found nothing, or the CREATE itself failed), and the `FileWriter`
of the one stream upload that can be running. The writer lives in a slot the command's future
does not own: when an interrupt drops the future in the middle of a write, the writer stays there
with everything on the wire tracked, and the cleanup can end it properly.

**The cleanup** runs when a command that used a `Remote` fails with something registered, and
after every interrupt:

1. It waits until every `Remote` clone and every `Alive` of the command is gone (the blocking task
   that writes a small downloaded file holds one, so nothing of the command still runs, on the wire
   or on the local disk).
2. On the connection that carried the upload, within 2 s for all of it: the writer is ended with
   `abort()` (it waits out the writes on the wire and closes the handle), then every registered
   name is deleted, all in flight together. A connection already known to be gone skips this step.
3. If that did not finish in time, the connection is gone, or a delete was answered with a sharing
   violation (a handle of our own session still open) or could not get credits
   (`cleanup_goes_to_a_new_connection`): **one** new connection, deletes only. Not after a silent
   link: there the files are named instead.
4. What is still registered is named: in the error message (`left on the server: …; remove with
   rm once the server has let go of them`) and in `left_behind`, **and then dropped from the
   registry**: later commands neither try to delete it again (each try would be a sharing
   violation and cost a new connection) nor carry it in their errors, and a retry of the same name
   is not refused as running; the retry's create finds the name taken and removes it as above once
   the server lets go (`leftover_temp_is_reported_once`, `silent_link_mid_put`). The registry is
   empty between commands, so the cleanup only ever acts on the share of its own command.

The connection is replaced before the next command after an interrupted transfer, or one that
failed locally once the first chunk was read (requests may be on the wire). A refusal before that
(the size at open is too big for `cat --json`, the local file cannot be created) has sent no
READ; dropping the download closes its handle and the connection is kept
(`refusal_before_data_keeps_the_connection`). A failed `put` is not run again; the new connection of
step 3 carries no data request.

| Event | `get`: what is left | `put`: what is left | The user is told | Test |
|---|---|---|---|---|
| the target exists, no `-f` | nothing touched, nothing sent | large: nothing created; small: temp written, then deleted | `already_exists`, exit 1 (or the prompt) | `overwrite_rules`, `overwrite_prompt`, `transfer_costs` |
| the target appears during the transfer (`get` with or without `-f`, `put` without) | temp removed; the file that appeared untouched | temp deleted; the file that appeared untouched | `already_exists`, exit 1 | `target_appears_during_get`, `target_appears_during_put` |
| the remote target is a directory | — | large: nothing created; small: temp written, then deleted | `is_a_directory` with the hint, exit 1, also with `-f` | `overwrite_rules` |
| the source or the target name has the temporary form | nothing sent | nothing sent | `invalid_name`, exit 1 | `structure_round_trip` |
| the source cannot be read (missing, denied, a directory) | nothing created | nothing created | the kind, exit 1 | `single_transfer_arguments`, `read_only_share` |
| the remote disk is full | — | the error comes from `finish`; temp deleted; old file untouched | `disk_full`, exit 1 | `disk_full_tiny` |
| the local disk is full, a local write fails | temp removed; the connection replaced before the next command | — | `local_io`, exit 1 | `local_failures` |
| the local file changes while it is read | — | writer aborted, temp deleted | `other`, exit 1 | `source_changes_during_put` |
| the remote file shrinks during the download | temp removed | — | `other` ("changed"), exit 1 | `file_shrinks_during_get` |
| the connection is **closed** in the middle | temp removed | one new connection deletes the temp; if that fails, temp left and named | `connection_lost`, exit 1; not run again; the next command works | `cut_mid_get`, `cut_mid_put` |
| the connection is closed between the rename request and its answer | — | the new connection finds no temp (the rename happened: the message says the file is probably complete, check with `ls`) or deletes it | `connection_lost`, exit 1 | (a rule; cannot be hit on purpose) |
| the link goes **silent** | temp removed, after the deadline | temp **left**, named; the server keeps it open for the dead session until it notices, and until then `rm` and a new `put` of that name fail with "in use" | `connection_lost`, exit 1, `left_behind` | `silent_link_mid_get`, `silent_link_mid_put` |
| Ctrl-C, SIGINT, SIGTERM in the middle | temp removed | writer aborted, temp deleted on the same connection within 2 s, else on one new connection; if both fail, left and named | `interrupted`, exit 130 (the shell: back to the prompt) | `signals_mid_get`, `signals_mid_put`, `signal_mid_put_edge`, `ctrl_c_in_the_shell` |
| a signal in the small-file phase of a recursive transfer | files being written locally are finished or never started; no temp | up to 16 temps deleted together | `interrupted`, 130, no summary | `signal_in_the_small_file_phase` |
| a signal during the cleanup | a small file being written at that moment may leave its temp, unnamed | temp left, named | `interrupted`, `left_behind`, 130 | `signals_mid_put` (two signals 10 ms apart) |
| a signal after the rename | final complete | final complete; the delete finds nothing | `interrupted`, 130, although the file is there | — |
| `kill -9`, a crash, power loss on the client | temp left locally, partial | temp left on the server (the server closes it when the connection drops) | nothing; the next transfer of the same name removes it | `kill_9_leaves_only_temporaries` |
| power loss just after a `get` reported success | final complete (synced before the rename), or absent if the rename was not yet on disk | — | — | — |
| a second rsmb transfers to the same target at the same time | the second fails at once (the lock) and touches nothing | the second's open of the temp is refused while the first writes; see the window below | an error for one of the two | `two_processes_one_target` |
| a crash between the hard link and the unlink of a `get` | final complete, and the temp name a second name of it; the next `get` removes the temp name and starts a new file | — | nothing | `local_temp_lifecycle` |
| a symbolic link inside the local destination of a `get -r` | nothing written through it; its subtree not fetched | — | one `error` with `path`, exit 1 | `get_r_keeps_inside_its_destination` |
| recursive: one file fails | that file's temp removed; the others go on | the same | one `error` with `path`; exit 1 at the end | `structure_round_trip`, `recursive_put_refuses_and_goes_on` |

`BUSY` is held from before a command until after its cleanup, so a signal in between cannot exit
past the cleanup (a second signal there is dropped; one during the cleanup ends it, and what it
had not removed is named).

**Leftovers and how to remove them.** A local `.rsmb-….part` after a kill: the next `get` of the
same file to the same place, or `rm`/`lls` by hand. A remote one: the next `put` of the same name
to the same directory, or `rm` in rsmb. After a silent link the server keeps it open for the dead
session until it notices the connection is gone (14.6 s in the adversary's probe of the planning
phase; longer on a link that stays silent): until then both fail with "in use" and change nothing.

## Timeouts, credits and reconnect

- The deadline is per request, not per transfer. Each READ or WRITE of at most 512 KiB, each small
  file of at most 256 KiB, each walk listing of at most 1 MiB + 64 KiB is one reply that must be
  complete within 6 × `--timeout`, with at most 4 MiB queued in front of it: a link must carry
  4 MiB in 180 s (23 KiB/s) with the default.
- A silent link fails the request in flight after between `--timeout` and max(`--timeout`, 15 s),
  and the transfer with it (`silent_link_mid_get`).
- Credits: the crate waits for credits while others are outstanding and sizes its chunks to the
  window. On a 16-credit server a stream runs with 64 KiB requests, small files about five at a
  time, walk listings four at a time; the walk's buffer is sized to the credits at hand so a
  listing compound can always be funded (`edge_and_encrypted`).
- No data moves on a new connection inside the command that lost the old one. A connection that
  died while idle is replaced before the transfer starts, as for every command that sends.
- After an interrupt with requests in flight the connection is replaced (Stage 1's `abandon`), so
  the next command starts with a reconnect (4 round trips).

## Limits

- Nothing is resumed and nothing is retried: a failed transfer starts again from byte 0; a failed
  recursive transfer is run again with `-f` and transfers everything again.
- Times, attributes and permissions are not kept. Symbolic links are not created.
- Changes on the server during a download that are not detected: growth, a rewrite of the same
  size.
- A share where one may create but not rename or delete leaves temporary files, named.
- **Two uploaders of one remote name** (different processes, different content, the same moment):
  between the first's CLOSE and its rename a second rsmb can write the same temporary name to its
  end (in practice a small file). The final name then holds one complete file, the second's, while
  the first reports success and the second `not_found`: never a partial file, nothing lost, the
  two reports the wrong way round. Not mitigated; the fix (a rename on the handle that wrote the
  data, a compound of our own for the small path) is recorded in `docs/performance.md`, "Not done".
- A server that folds names differently from Rust's `to_lowercase()` (or normalises Unicode), under
  `put -r -f`, can let one uploaded file replace another; without `-f` the second fails.
- A walk on a server without file ids stops after 100,000 directories and says so.
- `cat --json` prints at most 1 MiB of UTF-8.
