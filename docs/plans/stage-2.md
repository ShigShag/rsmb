# Stage 2 plan: transfers and file operations

> Written on 2026-11-18 by a planning agent that read the repository and nothing else, before any
> Stage 2 code exists. It follows [`master-plan.md`](master-plan.md) (sections 3–6, 8, 11–14);
> where it departs from it, section 4 says where and why. It is written for one implementing agent
> who was not present: everything needed to build Stage 2 in one pass is here or in a document this
> one names. **Revised on 2026-11-18 after the adversary's review:** section 11 lists the
> seventeen findings, the evidence for each and what changed; the text above it is the revised
> plan. Section 10 lists what could not be verified.
>
> Words are used as in `docs/measuring.md` (request, flight, outstanding) and in the master plan
> (fast, slow, edge, wan-bad). "The crate" is `smb2` 0.43.0. **Temporary file**, short **temp**:
> the name data is written under until the transfer is complete (5.4). **Stream path** and
> **small path** are the two ways a file's data moves (5.3).

## 1. Scope, what is not in it, exit criteria

**In scope** (brief, Stage 2; master plan section 8): `get` and `put`, single file and recursive
(`-r`), keeping the directory structure; streaming I/O; progress display, correct byte counts,
non-zero exit on failure; no file that looks complete after a failed or interrupted transfer; no
silent overwrite (`-f`, or a prompt in the interactive shell); `mkdir` (`-p`), `rm`, `rmdir`,
`cat`, `lcd`, `lpwd`, `lls`; cache invalidation by every command that changes a directory;
completion of local paths; the walk core (`src/walk.rs`) that Stage 3 reuses.

**Not in Stage 2:** `tree` and `find` as commands; resuming a transfer (a failed transfer starts
again from byte 0); a "skip what exists" mode; `mv`; recursive `rm`; `rm` of several files or by
pattern; preserving timestamps, attributes or permissions; several large files at once; the
private-use name mapping of the crate (5.2); a transfer continued on a new connection; any
compound of our own for file data (5.3); a flag for the concurrency limit or the chunk size;
symbolic links (not created, not followed locally in a recursive `put`).

**Exit criteria** (master plan section 8): every row of section 6.4 passes in `./test.sh`; the
full run is within 7 minutes of counted time and `./test.sh fast` within 2; the transfer
measurements of 6.5 are recorded in `docs/performance.md`; `docs/transfers.md` states every rule
of sections 5.3 to 5.12 so that a reviewer can check each against the code; the documents of
section 8 match the code; the review's confirmed findings are fixed and their tests are in the
suite.

## 2. What was measured while planning

All on 2026-11-18, Samba 4.23.8, the crate's own calls unless a row says "compound of our own",
with a throwaway program under `spikes/smb2/target/` (not committed), under the shared lock.
Counts through `smbtap`, times direct to the endpoint. Everything written went below a directory
of its own in `upload/` (on `samba` and on `samba-edge`) and to `tiny`, and was removed; both were
checked empty afterwards.

### 2.1 What one file costs

| What | Requests / flights | Per command |
|---|---|---|
| `Tree::read_file_compound_sized_with_info`, files of 0, 1,024, 65,537 and 1,048,576 bytes | 3 / 1 | CREATE, READ, CLOSE in one compound |
| the same with an expected size below the real one | 3 / 1, error `FileTooLargeForSingleRead` (kind `TooLarge`) carrying the real size; nothing returned | |
| `Tree::download`, 1-byte file | 7 / 3 | CREATE + 4 QUERY_INFO in one flight, READ, CLOSE |
| `Tree::download`, 0-byte file | 6 / 2 | no READ |
| `Tree::download`, 1,048,577 bytes | 9 / 4 | 3 READs (512 KiB, 512 KiB, 1 byte) |
| `Tree::write_file_compound` (3,000 bytes; also 0 bytes) | 4 / 2 | CREATE, WRITE, FLUSH in one compound; CLOSE on its own |
| `Tree::rename_with` | 3 / 1 | CREATE, SET_INFO, CLOSE |
| `Tree::delete_file`, `Tree::delete_directory` | 3 / 1 | CREATE, SET_INFO, CLOSE |
| compound of our own: CREATE (`FILE_READ_ATTRIBUTES \| SYNCHRONIZE`, `FileOpen`, no directory flag) + CLOSE | 2 / 1 | the CREATE answer carries attributes (0x10 directory, 0x20 file) and size; `OBJECT_NAME_NOT_FOUND` for a missing name, `OBJECT_PATH_NOT_FOUND` for a missing parent |
| compound of our own: CREATE (`FileCreate`, `FILE_DIRECTORY_FILE`, attribute 0x10) + CLOSE | 2 / 1 | creates the directory; `OBJECT_NAME_COLLISION` when a directory or a file has the name; `OBJECT_PATH_NOT_FOUND` for a missing parent. The crate's `create_directory` sends the same two requests in two flights |

So a small file costs **1 round trip down (3 requests) and 3 up (7 requests)** through the crate,
and a single-file download through the stream costs 3 round trips whatever its size.

### 2.2 Rename, delete and names on Samba

| Case | Answer |
|---|---|
| rename onto an existing file, no replace | `OBJECT_NAME_COLLISION` on the SET_INFO (kind `AlreadyExists`); both files unchanged (checked in the container) |
| rename onto an existing file, replace | success; the target has the new content |
| rename onto a directory, with or without replace | `OBJECT_NAME_COLLISION` |
| rename onto a name that differs from an existing one only in letter case, no replace | `OBJECT_NAME_COLLISION` |
| rename to a 250-byte name | success. A 256-byte name: `OBJECT_NAME_INVALID` |
| a name starting with a dot (`.rsmb-0123456789abcdef.part`) | created, renamed and deleted like any other |
| `delete_directory` on a directory that is not empty | `STATUS_DIRECTORY_NOT_EMPTY` (the crate's kind is `Other`) |
| `delete_file` on a directory / `delete_directory` on a file / `delete_file` on a missing name | `FILE_IS_A_DIRECTORY` / `NOT_A_DIRECTORY` / `OBJECT_NAME_NOT_FOUND` |
| a file with mode 0444 on the server | attributes 0x20: this Samba does not present it as read-only; a replacing rename replaced it and `delete_file` deleted it. **The read-only attribute cannot be tested here** (section 10) |
| the crate asked to write `a:b.txt` | it creates `a<U+F022>b.txt` on the server (its private-use mapping, `smb2::name`), where rsmb's `ls` would then show the private-use character |

### 2.3 Sixteen at a time on a fresh connection

A new connection (513 credits on `samba`, 16 on edge), 100 files of `smallfiles/d01` (1,024 to
4,096 bytes), 16 in flight:

| | fast, through the tap | edge, through the tap | slow, direct |
|---|---|---|---|
| download (compound read) | 300 requests, peak outstanding 48 | 300 requests, peak outstanding 15, no stall | **1606.5 ms** (7 waves × 1 round trip = 1.4 s) |
| upload (compound write to a temporary name, then rename) | 700 requests, peak outstanding 48 | 700 requests, no stall | **4517.3 ms** (7 waves × 3 round trips = 4.2 s) |

Every uploaded file had the checksum of its source (compared in the container). One after
another the same work needs 100 and 300 round trips: 20 s and 60 s.

The walk listing `[CREATE, QUERY_DIRECTORY(b), QUERY_DIRECTORY(64 KiB), CLOSE]` (shape P of
Stage 1 with one full query), 16 listings in flight, a new connection:

| Walk | b | Result |
|---|---|---|
| `tree/`, fast | 1 MiB | 342 listings (1 denied), 1,368 requests, peak outstanding 64, 85 ms |
| `tree/`, slow, direct | 1 MiB | **4827.7 ms** against 23 waves × 1 round trip = 4.6 s (Stage 0, through the crate: 18.5 s). Credits 513 at the start, 8,192 after |
| `fanout/`, fast | 1 MiB | 2,001 listings, 8,004 requests, 481 ms |
| `tree/`, edge | 64 KiB | 342 listings, peak outstanding 16 (four listings of 4 credits each), 85 ms, no stall |
| `fanout/`, edge | 64 KiB | the 2,000-entry root does not fit one 64 KiB query (the second phase of `list_dir` is needed, which the throwaway did not have); the 1,091 listings it reached completed without a stall |

### 2.4 Stopping in the middle

| Case | Measured |
|---|---|
| slow endpoint, 12 MiB pushed into a `FileWriter` (8 MiB confirmed), the writer **dropped**, then `delete_file` at once on the same connection | the delete answered after **402.9 ms**; the file was gone; the next request took 201.1 ms (normal) |
| the same with `FileWriter::abort()` before the delete | 401.2 ms for the abort, 602.5 ms until the delete had answered |
| slow endpoint, a `download` dropped after 8 MiB | the next request answered after 200.4 ms |
| fast endpoint, the tap cuts the connection in the middle of an upload | `write_chunk` fails with `Disconnected`; `abort()` returns after 0.1 ms; a new connection deleted the temporary file at the first try, 6.7 ms after the reconnect began |
| 12 MiB into `tiny` (10 MiB) | all twelve `write_chunk` calls returned success; **`finish()`** failed with `STATUS_DISK_FULL` during Write; `delete_file` then succeeded and `tiny` was empty |
| a file truncated on the server from 50 MiB to 20 MiB while it is downloaded | the download **ends without an error** after 20,971,520 bytes (size at open 52,432,897) |
| the same file grown by 1 MB during the download | the download ends after 52,432,897 bytes, the size at open |

### 2.5 The local disk

ext4 on NVMe, the disk the tests write to: 2,000 files of 2,560 bytes, written to a temporary
name and renamed: 0.07 s; the same with `fdatasync` before each rename, one after another:
12.3 s (6.2 ms a file). One file of 1.1 GiB in 512 KiB writes: `fdatasync` at the end 0.24 s.

## 3. Decisions on the open questions (master plan section 14)

| # | Question | Decision | Reason |
|---|---|---|---|
| S2-1 | Temporary names | Local and remote the same: **`.rsmb-<16 hex digits>.part`**, the digits being the 64-bit FNV-1a hash of the final file name's UTF-8 bytes (the name, not the path), in the directory of the final name. 27 bytes, so it fits wherever a name of 27 bytes or more fits, 250-byte names included. **A name of this form is never itself transferred** (finding 1). Section 5.4 | Derived from the final name, so a second attempt writes over what a killed first attempt left and removes it by its rename. On the server that works once the old session's handle is closed: at once after a kill or a cut the server sees, but after a silent link only when the server notices the dead connection (the adversary measured a sharing violation at 1.2 s and success at 14.6 s); until then the second attempt fails with "in use" and changes nothing (finding 3). FNV-1a is ten lines and gives the same name in every build; `std`'s hasher does not promise that. 64 bits, because two files in one directory with the same temporary name must not be in flight together (5.4 also refuses that case outright) |
| S2-2 | Round trips per small file | **p = 1 for `get`** (`read_file_compound_sized_with_info`, 3 requests) and **p = 3 for `put`** (`write_file_compound` to the temporary name: 2 flights, 4 requests; `rename_with`: 1 flight, 3 requests), all the crate's calls. A `put` in one compound is not built | Measured (2.1, 2.3). The FLUSH has to end its compound (the crate's own note: a server may refuse an operation that goes async anywhere but last, and Samba up to 4.15 crashes on it), and the rename has to come after the FLUSH, or a file would get its final name before its data is safe. The crate's three flights are the data-safe order; one flight fewer would need a write compound of our own for 0.2 s a wave |
| S2-3 | Waiting after an interruption | Only `put` has something to wait for. **On the connection that carried the upload, with 2 s for all of it:** the stream writer, which outlives the command's future in a slot of the session, is ended with `abort()` (it waits out the writes on the wire and closes the handle), then the temporary files are deleted, all deletes in flight together. If that does not finish in time, the connection is gone, or a delete is answered with a sharing violation or cannot get credits, **one** new connection is made and the deletes sent there. `get` waits for nothing on the wire. In every case the connection is replaced before the next command, as in Stage 1. Sections 5.2 and 5.11 | Measured: `abort()` and the delete took 603 ms at the slow endpoint. Dropping the writer instead (403 ms) is not safe on a server with few credits: the adversary saw the delete fail for want of credits and the writer's CLOSE never sent in 4 of 11 rounds on edge, the file still locked 2 s later, and a new connection delete it in 7 ms; `abort()` then delete worked 10 of 10 (finding 4) |
| S2-4 | `cat` with `--json` | One `content` object with the file as `text`, if the file is at most 1 MiB and valid UTF-8; otherwise an error of kind `other` whose message says to use `get` | The default. An object per line cannot carry a binary or a very large file |
| S2-5 | Walk buffer and small-file threshold | **Walk listings: b = min(negotiated transact size, 1 MiB), one full query plus the 64 KiB probe**: 4 requests, 1 flight, 19 credits on Samba, 4 on edge. **Small-file threshold: 256 KiB**, lowered to the negotiated read and write sizes (64 KiB on edge) | Measured (2.3): 16 listings of 19 credits fit the 513 of a new connection and ran at the wave bound; edge ran four at a time without a stall. The threshold departs from the default of 1 MiB (section 4, D2): 16 files of 256 KiB are 4 MiB in flight, the same as one stream window, so the bound on what an interrupt abandons and on what must arrive within one reply deadline is the same in every phase |
| S2-6 | File ids of links | Settled in Stage 1: `loops/self` carries the id of `loops/`, `loops/up` the id of the share root, the root's `..` has id 0 (`docs/plans/stage-1.md` 2.2; the gate test `file_ids_follow_links`). The walk uses ids as the master plan designs it | — |
| S2-7 | The 4.3 GiB test and the budget | The integration tests already run a release build (Stage 1, P6). `over4g.bin` goes down and up once, direct to the fast endpoint, in `tests/slow.rs` (full run only), about 45 s with its two checksums; `docs/testing.md` gets the line the 30 s rule asks for | It is a stress fixture, and `./test.sh fast` has to stay quick enough to run after every task |
| S2-8 | `mkdir` of a nested path | One level; a missing parent is `not_found`. `-p` opens-or-creates every directory of the path, one compound each (`FileOpenIf` with the directory flag), from the top: it succeeds on a directory that exists and fails with `not_a_directory` on a file in the way | The default. The open-or-create compound is the adversary's (finding 8): created, success for an existing directory, `NOT_A_DIRECTORY` for a file, `OBJECT_PATH_NOT_FOUND` for a missing parent |

Decisions on points the master plan left to this plan:

| # | Point | Decision |
|---|---|---|
| P1 | A single file's path | `get <file>` and `cat` always use the stream path (7 requests, 3 flights for a file of one chunk). The size is not known without asking, and asking with a blind 256 KiB compound read would move 256 KiB of a large file for nothing. In a recursive transfer the listing has the sizes |
| P2 | When the target of a `put` is checked | The stream path checks before it sends data, with and without `-f` (2 requests, 1 flight): a directory in the way is found before the upload, not after it. The small path does not: it writes the temporary file and lets the rename refuse; a refusal then costs at most 256 KiB, one delete and one check that tells a directory from a file for the message, and every accepted small file saves a round trip |
| P3 | The overwrite prompt | Asked by the interactive shell after the command has failed with `already_exists`, then the command runs again with `-f` (5.17). Nothing is ever read from the terminal inside a running command. Only single-file transfers ask; a recursive transfer reports every existing target as an error and names `-f` |
| P4 | What the second argument means | 5.5 (`get`) and 5.6 (`put`). For `-r` it names the directory **into which** the tree is copied, always; a tree cannot be renamed while it is copied |
| P5 | Exit status of a recursive transfer | 0 only if every directory and file the walk found was transferred. Anything not transferred (a directory that denies access, a link back to a directory already visited, an unreadable file, a name that cannot be used, an existing target without `-f`, a local symbolic link or special file) is one `error` object and makes the status 1. The transfer goes on past all of these |
| P6 | Names SMB forbids | The crate's private-use mapping is not used. A remote name with a character SMB forbids (`"*:<>?\|`, a control character, a trailing space or period) is refused before anything is sent, kind `invalid_name`. `cd` and `ls` keep sending names as typed (the server answers `invalid_name` itself). This settles the question Stage 1 left (its section 9) |
| P7 | Durability of a download | `sync_data` on the temporary file before the rename, for every file. Cost measured in 2.5 |
| P8 | Progress | A line of our own on stderr (5.18); `indicatif` is not added (D1) |
| P9 | Rerun after a reconnect | No Stage 2 command is run again. `cat` too (D4) |
| P10 | Error kinds | One new kind, `not_empty` (`rmdir` of a directory that has entries). Fifteen kinds in all |
| P11 | Flags inside commands | Exactly `-r`, `-f` (`get`, `put`) and `-p` (`mkdir`), each its own token before `--`. `-rf` is not a flag (Stage 1 rule P3), so a file called `-rf` stays reachable |
| P12 | Where local I/O runs | The stream path reads and writes with `std::fs` in the command's own future, which is polled on the main thread only (`Runtime::block_on`), so no runtime worker is blocked. Small files of a recursive transfer run in spawned tasks; their local work is one `spawn_blocking` closure a file. tokio's `fs` feature is not added |
| P13 | Order of a recursive transfer | Three phases, never overlapping: walk (listings, 16 in flight), small files (16 in flight), large files (one after another). The file list is held in memory between the phases (5.9) |
| P14 | `rm`, `rmdir` | One name each, no pattern, no `-r`, no `-f`. `rmdir` removes an empty directory only |
| P15 | Two rsmb processes writing the same local file | The local temporary file is locked (`File::try_lock`) while it is written; the second process fails at once and touches nothing (5.4) |
| P16 | Names of the temporary form | A name that matches `.rsmb-` + 16 lower-case hex digits + `.part` is never a source and never a target: `get`, `put` and their recursive forms refuse it with `invalid_name` (an item error in a recursive transfer). `ls`, `lls`, `cat` and `rm` treat it like any name |
| P17 | What `-f` replaces | Only what was there when the transfer started. A target that appears during the transfer is not replaced, with or without `-f`; in a recursive `get` that also keeps two server names that the local file system folds into one from overwriting each other (5.4, 5.9) |
| P18 | Symbolic links in the local destination of a recursive `get` | Not followed: every directory below the named local directory is checked with `symlink_metadata`; a link where a directory is to be is an item error and its subtree is left out |
| P19 | Two rsmb processes uploading different files to one remote name at the same moment | A documented limitation, not mitigated (section 10, assumption 10): the final name always holds one complete file, but the reports of the two processes can be the wrong way round |
| P20 | Order of the files in a recursive transfer | The file list is sorted by relative path (bytes) before the small and the large phase, so the order is the same in every run |

## 4. Departures from the master plan

| # | Master plan | Here | Why |
|---|---|---|---|
| D1 | 4.2: `indicatif` for transfer progress | no new dependency; one progress line written by about 40 lines of our own | One line with a percentage, a rate and a remaining time does not need a crate with its own dependency tree. The brief offers `indicatif` as a suggestion "to confirm or replace" and asks for a short dependency list |
| D2 | S2-5 default: small-file threshold 1 MiB | 256 KiB | S2-5 above. At 1 MiB, 16 files are 16 MiB in flight: on wan-bad (0.1 MB/s measured in Stage 0) that is 160 s, at the edge of the 180 s one reply may take |
| D3 | 8: "`put` … The target is checked before the upload starts" | on the stream path, always; not on the small path | P2 |
| D4 | 4.9: `cat` is among the commands run again after a reconnect | `cat` is not run again | It writes to stdout while it reads. Stage 1's rule for a rerun is that a command prints only after its last request; a second run would print the first part twice |
| D5 | 4.9: "An interrupted upload uses `FileWriter::abort`" | **withdrawn after finding 4: no departure.** The first version dropped the writer on an interrupt. Now the writer lives in a slot of the session that the command's future does not own, and the cleanup calls `abort()` on it (5.2) | Dropping was 200 ms faster at the slow endpoint and loses the handle on a server with few credits |
| D6 | 8, row 2.12: tap cut and signals "in the middle of `get` and of `put` of `big.bin` … fast" | `large/medium.bin` at the **slow** endpoint, through the tap | At the fast endpoint 1.1 GiB moves in about a second and "the middle" is a race. At the slow endpoint 50 MiB takes about 4 s, requests are certain to be in flight when the fault comes, and the wait of S2-3 is exercised where it costs something. A second reason: an upload of `big.bin` needs a 1.1 GiB local copy in every such test |
| D7 | 8, row 2.18: "`samba` paused during `get`" | the blackhole port of `wan` (10.213.45.30:4450) turned on in the middle of a `get` and of a `put` of `medium.bin` | The same reason: a pause takes a few hundred milliseconds to apply and the fast endpoint has finished by then. The blackhole is the same silent link (Stage 1, X3). The `put` half is added because it is the case in which a temporary file is left and has to be named |
| D8 | 8, row 2.6: "N files, e.g. 2 directories = 200" | N = 100, one directory (`smallfiles/d01`) per command | One `get -r` or `put -r` names one directory. The formula is the master plan's |
| D9 | 4.11: fourteen error kinds; 5.13 of Stage 1: the `error` object | kind `not_empty` added; the `error` object gets two optional fields, `path` and `left_behind`; new objects `transfer`, `summary`, `content`, `local_entry`, `lpwd` (5.19) | P10; a recursive transfer reports one error per item, and a temporary file that could not be removed has to be named where a program finds it |
| D10 | decision 5: "prompt in interactive mode" | the prompt exists for single-file transfers only | P3. With 16 files in flight there is no moment at which to ask about one of them |
| D11 | 4.9: a changing command is "not run again" after a lost connection | unchanged, with one addition: a failed `put` may make **one** new connection inside the command, only to delete its temporary files. No data request is sent on it | "Nothing is ever left" (master plan 8) cannot be kept on a connection that is gone. Not after a silent link: there the command fails without a second connection (Stage 1, D2) and names what it left |
| D12 | Stage 1, 5.14: "no example changes the server" | the examples of `get`, `put`, `mkdir`, `rm`, `rmdir` are written for a scratch directory, and the test runs them in one (5.17, H3) | master plan section 3 says so; the Stage 1 rule was written when no command changed anything |

Additions that change nothing decided before: the sync before the local rename (P7); the lock on
the local temporary file (P15); `~` at the start of a local path; `lls` showing times in UTC like
`ls`.

Nothing else departs. In particular: one connection, no second timer around requests (the 2 s of
S2-3 bounds a cleanup, after which that connection is never used again), the crate's stream types
for large files, CLOSE always awaited inside the command, no transfer retried or resumed, all use
of `smb2` in `src/session.rs`.

## 5. Design

### 5.1 Files, dependencies, constants

No new dependency. `Cargo.toml` is unchanged (tokio's `rt` already has `spawn_blocking` and
`JoinSet`, `sync` has `mpsc`; `libc` is there for `O_NOFOLLOW`).

| File | Stage 2 adds |
|---|---|
| `src/session.rs` | `Kind::NotEmpty`; `Error::path`, `Error::left`; `open_close` (one compound CREATE + CLOSE for `cd`, the existence check and `mkdir`); `Cache::invalidate`; `Remote` and its methods (every `smb2` call of Stage 2); the registry of remote temporary files, `Session::remote`, `finish`, `cleanup`; `valid_name`. `list_dir` stops dropping `.` and `..` (its callers do) |
| `src/transfer.rs` (new) | `temp_name`, `local_component`, `LocalTemp`, the target rules, `get`, `put`, `cat`, the recursive transfers, the local walk, `Progress` |
| `src/walk.rs` (new) | `walk`, `Listing`, `Dir`, `Report`: the one traversal routine, generic over the function that lists a directory |
| `src/commands.rs` | nine table entries and their functions (`mkdir`, `rm`, `rmdir`, `lcd`, `lpwd`, `lls` here; `get`, `put`, `cat` call `transfer`); flags passed to `dispatch`; `Arg::LocalPath`, `Arg::LocalDir`; `Out::raw`, `Out::item_error`, the current command line in `Out`; `Flow::Confirm`; the new JSON types |
| `src/shell.rs` | completion of local paths; the overwrite prompt |
| `src/main.rs` | `mod transfer; mod walk;`; `run_blocking` runs the cleanup after an interrupt; `Flow::Confirm` |
| `tests/common/mod.rs`, the three test files, `test.sh`, `testenv/bench-rsmb.sh` | section 6 |

Module dependencies: `transfer` → `session`, `walk`, `commands::Out`; `walk` → `session` (types
only); `commands` → `transfer`. Still no trait of our own.

Constants, each in one place:

| Name | Value | Where | Meaning |
|---|---|---|---|
| `L` | 16 | `walk.rs` | listings in flight in a walk; small files in flight in a recursive transfer |
| `SMALL` | 262,144 | `session.rs` | largest file for the small path; `Remote::small` = min(`SMALL`, negotiated read size, negotiated write size) |
| `WALK_BUFFER` | 1,048,576 | `session.rs` | cap of the full query of a walk listing |
| `CHUNK` | 524,288 | `transfer.rs` | the buffer a local file is read into for an upload |
| `CLEANUP_WAIT` | 2 s | `session.rs` | S2-3 |
| `CAT_JSON_MAX` | 1,048,576 | `transfer.rs` | S2-4 |
| `DEPTH_CAP` | 100 | `walk.rs` | depth limit for directories whose file id is 0 |
| `NO_ID_CAP` | 100,000 | `walk.rs` | how many directories without a file id one walk enters |

### 5.2 Session: errors, the compound open, `Remote`, cleanup

```rust
pub enum Kind { …, NotEmpty, … }                       // name "not_empty"; KIND_NAMES has 15 entries
pub struct Error { pub kind: Kind, pub message: String,
                   pub path: Option<String>,           // the item, for an error inside a recursive transfer
                   pub left: Vec<String> }             // temporary files that could not be removed
```

`Error::new` fills the two new fields empty. `classify` gains, before its `match` on the kind:
status `DIRECTORY_NOT_EMPTY` → `NotEmpty` (`<what>: directory not empty`); the crate's kind
`SharingViolation` → `Other` (`<what>: in use by another client`); status `CANNOT_DELETE` →
`Other` (`<what>: cannot be deleted (read-only?)`). The crate's `TooLarge` never reaches
`classify` (5.3).

**One compound open.** `check_dir` becomes a call of

```rust
/// CREATE + CLOSE in one compound: 2 requests, 1 flight. The CREATE's answer decides.
async fn open_close(conn: &Connection, tree: &Tree, path: &[String], disposition: CreateDisposition,
                    options: u32, attributes: u32, what: &str) -> Result<CreateResponse>
```

with access `FILE_READ_ATTRIBUTES | SYNCHRONIZE` always. `cd`: (`FileOpen`, 1, 0), as today.
Existence check: (`FileOpen`, 0, 0); the answer's `file_attributes & 0x10` and `end_of_file`.
`mkdir`: (`FileCreate`, 1, 0x10). `mkdir -p` and every directory of a `put -r`: (`FileOpenIf`,
1, 0x10), which creates the directory, succeeds on one that exists, and answers
`NOT_A_DIRECTORY` for a file of that name. The first three measured here (2.1; `cd` in Stage 0),
the fourth by the adversary (finding 8).

**Names.** `pub fn valid_name(name: &str) -> bool` is `!name.is_empty() &&
smb2::encode_name(name) == name`. Every path handed to a crate call goes through one function,
`crate_path(tree, components) -> Result<String>`: each component must be `valid_name`, else
`invalid_name` (`<path>: '<name>' has a character SMB does not allow`); the components are joined
with **`/`** (the crate's `Tree` methods split on `/` and take `\` as a name character) and the
crate adds the DFS prefix itself. Our own compounds keep using `tree_path` (joined with `\`).
For every name a server lists the two agree.

**Cache.**

```rust
impl Cache { pub fn invalidate(&mut self, share: &str, dir: &[String], subtree: bool) }
impl Session { pub fn invalidate(&mut self, dir: &[String], subtree: bool) }   // on the selected share
```

Removes the entry of `dir`; with `subtree` also every entry whose key starts with the key of
`dir` followed by `/` (every entry of the share when `dir` is the root). Keys are folded as in
Stage 1.

**`Remote`**: what a command needs to overlap requests, and the only way `transfer.rs` and
`walk.rs` reach the server. No `smb2` type appears in its public signatures.

```rust
#[derive(Clone)]
pub struct Remote { /* conn: Connection, tree: Arc<Tree>, share: String, walk_buf: u32,
                       temps: Arc<Mutex<Vec<Vec<String>>>>,
                       writer: Arc<tokio::sync::Mutex<Option<smb2::FileWriter>>>,   // the one stream upload
                       dirty: Arc<AtomicBool>, alive: mpsc::Sender<()> */
                    pub small: u64 }
#[derive(Clone)] pub struct Alive(/* mpsc::Sender<()> */);   // held by whatever must end before the cleanup
pub struct Stat { pub is_dir: bool, pub size: u64 }
pub enum Part<'a> { Start(u64), Data(&'a [u8]) }       // the size at open, then the data in order

impl Remote {
    pub async fn list(&self, dir: &[String]) -> Result<walk::Listing>            // 4 requests, 1 flight
    pub async fn stat(&self, path: &[String]) -> Result<Option<Stat>>            // 2 / 1; None: not found
    pub async fn mkdir(&self, path: &[String], existing_ok: bool) -> Result<()>  // 2 / 1; FileCreate or FileOpenIf
    pub async fn remove(&self, path: &[String], dir: bool) -> Result<()>         // 3 / 1
    pub fn alive(&self) -> Alive
    pub async fn read_small(&self, path: &[String], expected: u64) -> Result<Option<Vec<u8>>>   // 3 / 1; None: larger than expected now
    pub async fn download(&self, path: &[String], sink: &mut dyn FnMut(Part) -> Result<()>) -> Result<u64>
    pub async fn put_small(&self, dir: &[String], temp: &str, name: &str, data: &[u8], replace: bool) -> Result<()>
    pub async fn upload(&self, dir: &[String], temp: &str, name: &str, replace: bool,
                        source: &mut dyn FnMut(&mut [u8]) -> Result<usize>) -> Result<u64>
}

impl Session {
    pub async fn remote(&mut self) -> Result<Remote>          // starts with tree(); sets in_flight
    pub async fn finish<T>(&mut self, remote: Remote, r: Result<T>) -> Result<T>
    pub async fn cleanup(&mut self, reconnect: bool) -> Vec<String>
    pub fn temps_left(&self) -> Vec<String>
}
```

- `Session::remote()`: `tree(selected share)` (which replaces a dead connection, Stage 1), then a
  `Remote` holding a clone of the connection and of the tree, `small` as in 5.1, `walk_buf` =
  the `b` of `plan(min(max_transact, WALK_BUFFER), conn.credits())` (1 MiB on Samba, 64 KiB on
  edge; never more than the credits at hand can fund), a clone of the session's registry of
  temporary files and of its writer slot, and the sender half of a fresh channel whose receiver
  the session keeps. It sets `in_flight`. A `Remote` lives for one command and is dead once the connection is replaced.
- Every command that uses a `Remote` has this shape, and only this shape:

  ```rust
  let remote = sess.remote().await?;
  let r = work(&remote, …).await;
  sess.finish(remote, r).await
  ```

- `finish`: drops the `Remote`; if the registry or the writer slot is not empty,
  `cleanup(reconnect)` with `reconnect` false when `r` is a `ConnectionLost { silent: true }`
  and true otherwise, and what is left goes into the error's `left` and the end of its message
  (`; left on the server: <paths>; remove with rm once the server has let go of them`); clears
  `in_flight`; if the `Remote` was marked dirty and the connection is still the one it was made
  on, `mark_dead()`.
- **The registry**: the wire-independent paths (components) of remote temporary files that exist
  or may exist. `put_small` and `upload` add the name before the CREATE that makes the file. A
  name is taken out only when the server has confirmed the rename, or a delete (success or
  `not_found`), or when the CREATE itself failed. Adding a name that is already there fails
  with `other` (`<temp>: a transfer with the same temporary name is running`), so two files in
  one directory whose names hash alike are never in flight together.
- **The writer slot**: the `FileWriter` of the one stream upload that can be running, in an
  `Arc<tokio::sync::Mutex<Option<..>>>` that the session owns and the `Remote` shares. `upload`
  puts the writer there when it has opened it, and holds the lock around each `write_chunk`
  and around `finish`, which takes the writer out. When the command's future is dropped, the
  lock guard goes with it and the writer **stays in the slot**: `write_chunk` may be dropped at
  any point without losing track of what is on the wire (the crate documents that), so the
  cleanup can still end the writer properly (finding 4).
- **`cleanup(reconnect)`**, used by `finish` and after an interrupt (5.11):
  1. Wait until every `Remote` clone and every `Alive` of the command is gone: `recv()` on the
     channel returns `None` when the last sender is dropped. Spawned tasks hold `Remote`
     clones; after an interrupt they have been aborted and end at their next await. The
     `spawn_blocking` closure that writes one small downloaded file holds an `Alive`, on
     purpose: the wait then lasts until that file is written, synced and renamed (milliseconds
     for at most 256 KiB), and after it nothing of the command is running, on the wire or on the
     local disk (finding 13).
  2. If the connection is not disconnected, under one `tokio::time::timeout(CLEANUP_WAIT)` for
     all of this step: a writer in the slot is taken out and ended with `abort()` (it waits for
     the answers of the writes on the wire, then closes the handle; no flush); then
     `delete_file` for every name in the registry, **all in flight together** (a `JoinSet`;
     sixteen deletes one after another would be sixteen round trips, finding 9). Success and
     `not_found` take a name out. **A lost connection, the timeout, a sharing violation, or a
     delete that cannot get credits** (the crate's `CreditStarvation`): `mark_dead()` and go to
     step 3. A sharing violation here means a handle of **our own** session is still open on
     the file (a small `put` cut between its write compound and its CLOSE, a CLOSE that could
     not be sent for want of credits), and that handle dies with the session. Any other answer
     (access denied) leaves the name in; a second connection would get the same answer.
     If the connection is disconnected, a writer in the slot is dropped (it sends nothing on a
     dead session).
  3. If names remain, the connection is disconnected and `reconnect` is true: `tree(share)` once
     (the Stage 1 reconnect: one dial, one login, one TREE_CONNECT) and `delete_file` for each
     remaining name, in flight together, with no timer but the crate's. A sharing violation
     here is another session's handle (or the old one's, if the server has not yet seen it
     end): the name stays in and is reported.
  4. Return the display paths (`/dir/.rsmb-….part`) still in the registry. `temps_left` returns
     the same without doing anything; it is what is reported when the cleanup itself is
     interrupted.
- **The generation.** `Session` counts its reconnects (`generation: u64`, incremented in
  `ensure_connected` when it replaces the connection, which also clears `in_flight`).
  `run_parsed` notes the generation before `dispatch` and calls `mark_dead()` for a lost error
  only if it is unchanged: otherwise it would kill the connection `cleanup` has just made.

`Remote`'s methods, all through `crate_path` and `classify`:

- `list`: `list_dir(conn, tree id, path, walk_buf, 1, what)`, then the ids of `.` and `..` are
  taken out of the entries and the two entries dropped: `walk::Listing { entries, id, parent }`,
  an id of 0 meaning "none".
- `stat`: `open_close(FileOpen, 0, 0)`; `not_found` → `None`.
- `mkdir`: `open_close(FileCreate, 1, 0x10)`, or `FileOpenIf` with `existing_ok`.
- `remove`: `Tree::delete_file` or `Tree::delete_directory`.
- `read_small`: `Tree::read_file_compound_sized_with_info(conn, path, max(expected, 1))`.
  `FileTooLargeForSingleRead` → `Ok(None)` (the caller takes the stream path). The data must be
  as long as the size in the same answer, else `other` (`<path>: changed while it was read`).
  A name that has become a directory answers the READ with `STATUS_INVALID_DEVICE_REQUEST`
  (the crate's kind `Unsupported`; measured by the adversary, finding 14): that status on the
  READ is mapped to `is_a_directory` here.
- `download`, `put_small`, `upload`: 5.3.

### 5.3 The transfer engine

Two paths, both the crate's own calls. rsmb has no chunk scheduler.

**Stream path** (any size; every single-file transfer; in a recursive transfer every file above
`Remote::small`).

- Down, `Remote::download`: `Tree::download(&mut conn_clone, path)`; if the open says it is a
  directory, `is_a_directory`; `sink(Part::Start(size))`; then `next_chunk()` until `None`, each
  chunk to `sink(Part::Data(..))`; then the bytes received must equal the size at open, else
  `other` (`<path>: changed during the transfer (<size> bytes when opened, <n> received)`): a
  file that shrinks ends the crate's download without an error (2.4). The crate reads 512 KiB a
  request (the server's read size if smaller), keeps at most 4 MiB requested and not delivered,
  and sizes that window to the link. If the sink fails, or a chunk fails with an error that is
  not a lost connection, the `Remote` is marked dirty: reads may be on the wire, and `finish`
  has the connection replaced rather than leave their answers in front of the next command.
- Up, `Remote::upload`: **`stat(final)` first, always**: a directory → `is_a_directory`
  (the message of the rename rule below), with or without `replace`, before a byte is sent
  (finding 12); a file without `replace` → `already_exists`; nothing created in either case.
  Then the temporary name goes into the registry;
  `Tree::create_file_writer(conn.clone(), temp)` (`FileOverwriteIf`, so a stale temporary file is
  written over; share access none, so a second open of the same temporary file is refused
  while this one is open, and this open is refused with a sharing violation while an older
  handle is still open on it: `other`, `<temp>: in use: another transfer is writing it, or the
  server has not yet closed the handle of an earlier one; try again later`); the writer goes
  into the slot of 5.2; a loop of `source(&mut buf)` (a `CHUNK` buffer) and
  `write_chunk(&buf[..n])` until the source returns 0; `finish()` (waits for every WRITE, FLUSH,
  CLOSE), whose count of confirmed bytes must equal the bytes handed over, else `other`; then
  the rename (below). The crate writes 512 KiB a request with at most 4 MiB unconfirmed.
  A failing `source` or a failing write that is not a lost connection: the writer is taken out
  of the slot and ended with `abort()` (it waits out the writes on the wire and closes), then
  the temporary file is deleted. A failing `finish` (a full disk shows up **here**, 2.4): the
  writer is gone and its CLOSE was queued by the crate; delete the temporary file. If any of
  these deletes is refused with a sharing violation or for want of credits, the name stays in
  the registry and `finish`'s cleanup deletes it on a new connection (5.2, step 2).
- The rename, shared by both upload paths: `Tree::rename_with(temp, final, replace)`. Success
  takes the name out of the registry. Any failure: delete the temporary file, then word the
  error by one rule (finding 11):
  - `AlreadyExists`: `stat(final)` (2 requests, 1 flight; only on this path). A directory →
    `is_a_directory`, `<final>: is a directory; write '<final>/' to put the file into it`,
    whatever `replace` is (Samba answers a rename onto a directory with a collision even with
    replace, 2.2). A file, without `replace` → `already_exists`, `<final>: exists on the
    server; -f replaces it`. A file, with `replace` → `other`, `<final>: the server did not
    replace it`. (On the stream path the directory case was already caught by the first
    `stat`; this covers a directory that appeared since, and the small path.)
  - `AccessDenied` with `replace`: `access_denied`, `<final>: the server refused to replace it
    (open in another program, read-only, or not permitted)` (finding 12: a target another
    client has open answers this way).
  - anything else: the classified error.

  Each delete that succeeds (or finds nothing) takes the name out; one that fails leaves it for
  `finish`.

**Small path** (files of at most `Remote::small` bytes inside a recursive transfer, and a single
`put` of such a file, whose size is known locally).

- Down: `Remote::read_small` (3 requests, 1 flight), the whole file in memory.
- Up, `Remote::put_small`: registry; `Tree::write_file_compound(temp, data)` (`FileOverwriteIf`,
  CREATE + WRITE + FLUSH, then CLOSE); the count it returns must equal the length; the rename as
  above. 7 requests, 3 flights. No check of the target first (P2); a refused one costs the
  delete (3) and the `stat` (2) on top: 12 requests.

**What is in memory.** Stream path: the crate's window (at most 4 MiB), one chunk, and for an
upload the `CHUNK` buffer. Small path: at most `L` files of at most 256 KiB each (for an upload
twice, the crate copies the data into its request). A recursive transfer also holds its file
list, about 150 bytes a file. The gate asserts a peak RSS of at most 64 MB for `big.bin` each
way and for 2,000 small files.

**What is in flight.** The phases of a recursive transfer never overlap (P13), so the bound is
per phase: walk at most `L` listings (64 requests on Samba), small files at most `L` (48
requests), stream at most the crate's window (8 reads or 4 writes measured in Stage 0, 16 on
edge). The tests assert 64, 48 and 16.

**Local I/O.** P12. The stream path's sink and source are plain closures in `transfer.rs` that
call `std::fs` and the progress line. A small file's local half is one closure in
`spawn_blocking`: for a download create the temporary file, write, sync, rename; for an upload
read the whole file (if it has grown past `small` since the walk, it goes to the large list
instead). The download closure holds an `Alive` (5.2), so the cleanup after an interrupt waits
for it. Every local path is made absolute (`std::path::absolute`) when the command starts, so
nothing depends on a working directory that a later `lcd` changes (finding 13).

**Not retried, not resumed.** A transfer that fails is over. The next attempt starts at byte 0
and writes over the temporary file of the last one (on the server: once the old handle is
closed, S2-1).

### 5.4 Temporary names and the rename

```rust
pub fn temp_name(final_name: &str) -> String     // ".rsmb-" + 16 lower-case hex digits + ".part"
```

FNV-1a, 64 bits: `h = 0xcbf29ce484222325`; for each byte `h ^= byte; h = h.wrapping_mul(0x100000001b3)`.
The temporary file is always in the directory of the final name, so the rename never crosses a
file system or a share. `ls` and `lls` show such files like any other (`lls` lists names that
start with a dot); they are safe to delete when no transfer is running.

```rust
pub fn is_temp_name(name: &str) -> bool          // exactly ".rsmb-", 16 of [0-9a-f], ".part"
```

**A name of this form is never transferred** (P16, finding 1). A leftover temporary file is a
partial file by definition, and worse: it may be the temporary name of a file in the same
transfer. Uploaded under its own name it would be written over and renamed away by the transfer
of the file whose hash it carries, with exit 0; with `-f` and both in flight, partial bytes
could end under a final name. So: a single `get` or `put` whose source name or target name has
this form fails with `invalid_name` (`<name>: a temporary file of rsmb; not transferred. If it
is a leftover, delete it`), before anything is sent or created; in a recursive transfer every
such entry is an item error of the same kind, and a directory of that name is left out with its
subtree. `cat`, `rm`, `ls` and `lls` are not affected, so a leftover can be looked at and
removed.

**Local** (`get`):

```rust
struct LocalTemp { path: PathBuf, file: Option<std::fs::File> }      // Drop removes the file unless committed
impl LocalTemp {
    fn create(dir: &Path, final_name: &str) -> Result<LocalTemp>
    fn write(&mut self, data: &[u8]) -> Result<()>
    fn commit(self, final_path: &Path, replace: bool) -> Result<()>
}
```

- `create`, at most three times round: open read and write, create, **no truncate**,
  `O_NOFOLLOW` (a symbolic link planted under the temporary name is an error, not a file to
  write through); `try_lock()`: `WouldBlock` → `other` (`<final>: another transfer is writing
  this file`), any other lock error → go on without a lock (a file system without locks); then
  the path must still name the file that was opened (same device and inode in
  `std::fs::metadata(path)` and `file.metadata()`), else start again (a transfer that finished
  in between renamed that file away); then, **if the file has more than one link (`nlink() >
  1`), remove the temporary name and start again**: a crash between the hard link and the
  unlink of an earlier `commit` leaves the temporary name as a second name of a complete final
  file, and truncating it would truncate that file in place (finding 5); then `set_len(0)`. The
  lock is held until the file is closed, after the rename.
- `commit(final, replace)`: the caller passes `replace = -f was given **and** the final name
  existed when the transfer started` (P17). A target that was not there at the start is
  committed without replace, so one that appears meanwhile is never overwritten: another
  program's file, or, on a local file system that folds case or normalises Unicode, the file a
  sibling of the same recursive `get` has just committed (finding 6: `Case.txt` then
  `case.txt`; the second becomes `already_exists` instead of replacing the first).
  `sync_data()`. Then, with `replace`: if the final name exists and is neither a
  regular file nor a symbolic link, `is_a_directory` for a directory and `local_io` otherwise
  (`-f` never replaces a directory, a device or a pipe); `std::fs::rename` (atomic; a symbolic
  link at the final name is replaced, its target is not touched). Without `replace`:
  `std::fs::hard_link(temp, final)`, which fails if the final name exists however it got there,
  then remove the temporary name; `AlreadyExists` → `already_exists`; any other error (a file
  system without hard links) → if the final name exists, `already_exists`, else `rename`.
- A failed `commit` and a `LocalTemp` dropped without one remove the temporary file. If the
  removal fails, the path goes into the error's `left`.
- The parent directory is not synced.

**Remote** (`put`): 5.3. The temporary file is made by the crate with `FileOverwriteIf`; while a
stream upload writes it, no other open of it succeeds. The rename with replace cannot be
limited to "what was there at the start" as on the local side: the server decides, and a small
`put` never looked. Section 10, assumptions 10 and 21, say what that leaves open.

What the user can find, and when:

| Left behind | When | What removes it |
|---|---|---|
| a local `.rsmb-….part` | rsmb was killed (`kill -9`, power loss) during a `get`; or a second signal ended the cleanup of a recursive `get` while a small file was being written | the next `get` of the same file to the same place; or `rm` by hand |
| a remote `.rsmb-….part` | rsmb was killed during a `put`; the link went silent during a `put`; the cleanup was interrupted, or the server refused the delete | the next `put` of the same name to the same directory; or `rm` in rsmb. In the last three cases the error names the file. **After a silent link the server keeps the file open for the dead session until it notices the connection is gone** (14.6 s in the adversary's probe; longer on a link that stays silent): until then both the next `put` and `rm` fail with "in use" and change nothing |
| anything under the final name | never, unless it is complete (or was there before and was not to be replaced) | — |

### 5.5 `get`

```
get [-r] [-f] <remote> [<local>]
```

1. No share selected → `usage`. `remote` is resolved like every remote path. The **base name**
   is its last component; for the share root (only meaningful with `-r`) the share's name.
2. The local target, by one pure function `local_target(local_arg, base, recursive)`: no
   `local` → the local working directory; a `local` that is an existing directory, or ends in
   `/` → that directory; in both cases the file is `<directory>/<base>`. Otherwise (without
   `-r` only) `local` is the file's path, and its parent must exist. With `-r`, `local` must be
   an existing directory, and the tree goes to `<local>/<base>/…`. A leading `~` or `~/` is
   `$HOME`. The base name must pass `local_component` (5.9). The target path is made absolute
   here. Neither the remote name nor the local file name may have the temporary form
   (`is_temp_name`, 5.4): `invalid_name`, nothing sent.
3. Without `-r`: `existed` = the target exists (`symlink_metadata`). If it does and `-f` is
   absent → `already_exists` (`<path>: exists; -f replaces it`), **before any request**. Then
   `Remote::download`: on `Part::Start` the `LocalTemp` is created (so a remote name that does
   not exist creates nothing locally) and the progress line gets its total; each `Part::Data`
   is written; after the last, `commit(target, force && existed)`. One `transfer` line.
   A directory → `is_a_directory` with the hint `use get -r`.
4. With `-r`: 5.9. If the remote path turns out to be a file, it is fetched as in 3.

Requests on Samba, single file: 6 + the number of READs (7 for anything up to 512 KiB), 3
flights for one chunk. Nothing is invalidated: a `get` changes nothing on the server.

### 5.6 `put`

```
put [-r] [-f] <local> [<remote>]
```

1. No share selected → `usage`. `local` (with `~`) must exist: a regular file, or a symbolic
   link to one (the named argument is followed); a directory needs `-r` (`is_a_directory`, hint
   `use put -r`); anything else → `local_io` (`not a regular file`). The **base name** is the
   file name of the canonical path; it must be valid UTF-8 and `valid_name`. Neither the local
   name nor the remote file name may have the temporary form (`invalid_name`, nothing sent).
2. The remote target, by one pure function `remote_target(cwd, remote_arg, base, recursive)`:
   no `remote` → `<working directory>/<base>`; a `remote` that ends in `/` or `\` →
   `<remote>/<base>`; otherwise (without `-r`) `remote` is the file's path. With `-r`, `remote`
   always names the directory into which `<base>/…` goes. **A trailing separator is the only
   way to say "into this directory" for a single file**: finding out whether a name is a
   directory would cost a request. `put a.txt docs` with a directory `docs` therefore fails,
   on both paths and with or without `-f`, with `is_a_directory` and the one message of 5.3
   (`/docs: is a directory; write '/docs/' to put the file into it`); completion writes the `/`
   itself.
3. `sess.invalidate(parent of the target, false)`, before the first request, whatever follows.
4. Without `-r`: the file is opened and its size and modification time noted. At most
   `Remote::small` bytes → read it whole, `Remote::put_small(.., force)`. Larger →
   `Remote::upload(.., force, source)`; the source reads the file and at its end compares size
   and modification time with what was noted: a difference → `other` (`<local>: changed while
   it was read`), which makes `upload` abort and delete the temporary file. The same check is
   made on the small path before the write. One `transfer` line.
5. With `-r`: 5.10.

Requests on Samba, single file: at most 256 KiB: 7 in 3 flights. Larger: 2 (the check) + 4
(CREATE and 3 QUERY_INFO) + the WRITEs + FLUSH + CLOSE + 3 (rename). A small `put` refused
because the target exists: 7 + 3 (the delete of the temporary file) + 2 (the `stat` that words
the error) = 12.

### 5.7 Overwrite rules

| Case | Without `-f` | With `-f` |
|---|---|---|
| `get`, local target exists | `already_exists`, 0 requests, exit 1; in the interactive shell the prompt of 5.17 | replaced by the rename, when the download is complete; a directory or a special file is never replaced |
| `get`, the target appears during the download | `already_exists` from the hard link; the file that appeared is untouched | the same: `-f` replaces what was there at the start, nothing else (P17) |
| `put`, large, remote target exists | `already_exists` from the check, 2 requests, nothing created | replaced by the server in the rename |
| `put`, the remote target is a directory (no trailing `/` was written) | `is_a_directory` with the hint to write `<name>/`: large from the check, small from the rename and the `stat` after it | the same |
| `put`, small, remote target exists | `already_exists` from the rename; the temporary file is deleted | replaced |
| `put`, the target appears during the upload | `already_exists` from the rename; the file that appeared is untouched | replaced (the server cannot be told "only what was there before") |
| recursive, a file target exists | one `error` (`already_exists`, with `path`) per file, the transfer goes on, exit 1, no prompt | replaced, file by file |
| recursive, a directory target exists | used as it is (directories are merged, never replaced) | the same |
| a target that is a directory where a file is to go | `is_a_directory` | `is_a_directory` |

What `-f` does not do: it does not clear a read-only attribute and does not wait for another
program to close the target (a server that refuses the replacing rename for either reason
answers `ACCESS_DENIED`; the message of 5.3 names both), and the replaced file's permissions
and times are not carried over. Neither `-f` on a recursive transfer nor `rm` asks for
confirmation (the user's decision of 2026-11-18).

### 5.8 What "complete" means: the checks at the end

| Transfer | Checked before the rename |
|---|---|
| `get`, stream | bytes written = the size the server gave at open; then `sync_data` |
| `get`, small | data length = the size in the same answer; then `sync_data` |
| `put`, stream | bytes confirmed by the server (`finish()`; the crate treats a WRITE answered short as an error) = bytes read from the file = its size when opened; size and modification time unchanged at the end; the FLUSH succeeded |
| `put`, small | the count the write returns = the length; size and modification time unchanged; the FLUSH succeeded |

No checksum is computed and nothing is read back; the tests compare sha256 on both sides. Two
changes on the server during a download are **not** detected: a file that grows arrives with
the size it had at open (2.4), and a file rewritten in place with the same size arrives as a
mix of old and new bytes (the crate opens it sharing read, write and delete, so nothing stops
another writer; finding 12). Comparing the file's change time at the end would cost a round
trip per file (three times the cost of a small file) and was not measured for reliability; it
is not done. `docs/transfers.md` and `docs/known-limitations.md` say so.

### 5.9 The walk core and recursive `get`

`src/walk.rs`:

```rust
pub const L: usize = 16;
pub struct Listing { pub entries: Vec<Entry>, pub id: u64, pub parent: u64 }   // without . and ..; their ids, 0 = none
pub struct Dir { pub path: Vec<String>, pub depth: usize, pub entries: Vec<Entry> }   // sorted by name
pub struct Report {
    pub denied: Vec<Vec<String>>,                          // listing refused with access_denied
    pub links:  Vec<(Vec<String>, Option<Vec<String>>)>,   // not entered: its id was seen before (where, if known)
    pub capped: Vec<Vec<String>>,                          // not entered: no file id and deeper than DEPTH_CAP
    pub failed: Vec<(Vec<String>, Error)>,                 // any other error for one directory
    pub listed: usize,
}
pub async fn walk<F, Fut>(list: F, start: &[String], max_depth: Option<usize>,
                          enter: &dyn Fn(&[String], &Entry) -> bool,
                          on_dir: &mut dyn FnMut(Dir)) -> Result<Report>
where F: Fn(Vec<String>) -> Fut, Fut: Future<Output = Result<Listing>> + Send + 'static
```

- `list` is `|d| { let r = remote.clone(); async move { r.list(&d).await } }` in the product
  and a fake tree in the unit tests. It is a parameter so that the loop check, the depth cap for
  servers without ids and the width can be tested without a server; there is no trait.
- One coordinating loop, a queue of directories and a `JoinSet`: while the queue is not empty
  and fewer than `L` listings are in flight, spawn one; then `join_next()`. No semaphore.
- The start directory is listed first, alone. Its error is the walk's error (`not_found`,
  `not_a_directory`, `access_denied`). Its `id` and `parent`, when not 0, seed the set of
  visited ids (`HashMap<u64, Vec<String>>`, id → first path; the parent's path is unknown).
- For each directory entry of a listing, in order: `enter(path, entry)` false → skipped
  silently (the caller reports it); depth beyond `max_depth` → not entered; id 0 → entered
  unless it is deeper than `DEPTH_CAP` **or the walk has already entered `NO_ID_CAP`
  directories without an id**, then `capped`; id already visited → `links`; else the id is
  recorded and the directory queued. The depth cap alone bounds depth, not breadth: two links
  without ids at every level would be 2^100 listings (finding 16). The second cap ends any
  walk on a server without ids after 100,000 such directories, and says so; a real tree of
  that size on such a server is cut short and reported, which for a transfer is an error.
- An error for a directory below the start: `access_denied` → `denied`; a lost connection or
  `interrupted` → fatal; anything else → `failed`. On a fatal error nothing more is spawned,
  the listings in flight are awaited (so no request is abandoned on a connection that may
  live), and the error is returned.
- `on_dir` is called once per listed directory, in the order the listings complete, with the
  entries sorted by name. The cache is not touched.
- Depth: the start directory is depth 0, its entries depth 1 (decision 3, for Stage 3).

**Recursive `get`** (`get -r <remote dir> [<local dir>]`):

1. The walk, with `enter` = "the name passes `local_component` and is not of the temporary
   form". `on_dir` collects every directory path and every file (`rel` path components, size).
   The progress line shows the number of directories listed. When the walk is done, the file
   list is sorted by relative path (P20).
2. Reports, each an `error` with `path`: every `denied` directory (`access_denied`), every
   `links` entry (`other`: `<path>: not copied: the same directory as <first>` or `… as a
   directory above <start>`; a link, or a server that reuses file ids), every `capped` (`other`),
   every `failed`; every entry whose name fails `local_component` (`invalid_name`); every file
   or directory whose name has the temporary form (`invalid_name`, 5.4).
3. Local directories: `<local>/<base>` and one for each collected directory, parents first,
   empty ones included, **one component at a time and never through a symbolic link** (P18,
   finding 7): `symlink_metadata(path)`; nothing there → `create_dir`; a real directory → used;
   a symbolic link → one `error` (`other`: `<path>: a symbolic link in the destination; not
   followed`), and that directory's whole subtree is left out and counted; any other kind of
   file → `not_a_directory`, likewise. `create_dir_all` is not used. Any other failure is one
   `error` (`local_io`) and leaves the subtree out the same way. So every file is committed
   into a directory that this transfer has seen to be a real directory below `<local>`.
   (`<local>` itself is what the user named and may be a link.)
4. For every file, `existed` = its target exists (`symlink_metadata`), noted **before any
   transfer starts**. Without `-f`, an existing target is one `error` (`already_exists`), no
   request. Two files of this transfer whose existing targets are one and the same local file
   (equal device and inode: a local file system that folds case or normalises names, on a
   second run) are both an `error` (`invalid_name`: `<path>: the local file system takes it
   for the same name as <other>`), with or without `-f`. Every commit below is
   `commit(target, force && existed)` (P17), so on a first run into such a file system the
   second of two folded names fails with `already_exists` instead of replacing the first
   (finding 6).
5. Small files (size at most `Remote::small`): up to `L` in flight; each task is
   `read_small`, then the `spawn_blocking` closure of 5.3. `None` from `read_small` (the file
   grew) moves the file to the large list.
6. Large files, one after another, as in 5.5 step 3.
7. One `transfer` line per file as it completes; at the end one `summary`. If anything was
   reported in steps 2 to 6, the command fails with `other`
   (`get -r <remote>: <n> of <m> not transferred`), exit 1.

A **fatal** error stops the transfer: a lost connection, and a local write that fails with
`StorageFull` or `QuotaExceeded`. What is in flight is awaited, the summary of what was done is
printed, and the fatal error is the command's error. Files already complete stay.

An **interrupt** is different: it drops the command's future (5.11), so nothing more of the
command runs and **no summary is printed**. The `transfer` lines already written are the
record of what arrived; then come the `interrupted` error and `done` (finding 11).

```rust
pub fn local_component(name: &str) -> Result<&str>
```

refuses the empty name, `.`, `..`, and any name containing `/`, `\` or NUL, with
`invalid_name`. Local paths are built only by pushing checked components onto the target
directory, so nothing a server sends can name a path outside it.

Files that change during the walk: a file is transferred as it is when its turn comes. Gone →
`not_found` for that file. Grown past the threshold → the stream path. Now a directory →
`is_a_directory` for that file (the stream path sees it in the open's answer, the small path
in the READ's status, 5.2). A directory created after its parent was listed is not seen.

Round trips at the slow endpoint for N small files in D directories: Σ ⌈directories per level
÷ 16⌉ for the listings, plus ⌈N ÷ 16⌉ × 1. Requests: 4 per listing, 3 per small file.

### 5.10 Recursive `put`

`put -r <local dir> [<remote dir>]`: the tree goes to `<remote dir>/<base>/…`, `<remote dir>`
defaulting to the working directory.

1. `sess.invalidate(<remote dir>, false)` and `sess.invalidate(<remote dir>/<base>, true)`.
2. The local walk, one `spawn_blocking` closure (so an interrupt is not held up by a large
   tree): `read_dir`, entries sorted by name, `symlink_metadata` for each. A directory is
   entered; a regular file is collected with its size; a symbolic link (to anything) and any
   other kind of file are **not followed and not read**: one `error` each (`other`:
   `<path>: not uploaded: a symbolic link` / `a special file`). A name that is not UTF-8 or
   not `valid_name` → `invalid_name`; a name of the temporary form → `invalid_name` (5.4); for
   a directory the whole subtree is left out and the error says how many files it held. Within
   one directory, names that are equal after
   `to_lowercase()` are **all** reported (`invalid_name`: `<path>: not uploaded: differs only
   in letter case from <other>`) and none is uploaded, directories with their subtrees.
   **A directory that cannot be read** (`read_dir` or an entry fails) is one `error`
   (`local_io`) and is left out with everything below it: it is **not** created on the server,
   so an unreadable local directory never becomes an empty remote one with exit 0 (finding
   11). A file that cannot be opened or read later is that file's `error`. A directory whose
   device and inode the walk has already entered (a bind mount or a file system that loops) is
   one `error` (`other`) and not entered again (finding 16). The file list is sorted by
   relative path (P20).
3. Remote directories, level by level, up to `L` in flight within a level, each with
   `Remote::mkdir(path, true)`: the open-or-create compound of 5.2, which succeeds on a
   directory that is there (merge) and answers `not_a_directory` for a file of that name
   (finding 8). `<remote dir>/<base>` first, alone: its error is the command's error and
   nothing else is sent. Below it, an error is one `error` for that directory, and its subtree
   is left out and counted.
4. Small files (at most `Remote::small`), up to `L` in flight: the `spawn_blocking` read, then
   `put_small(.., force)`. `already_exists` without `-f` is that file's `error`.
5. Large files, one after another: `upload(.., force, source)` as in 5.6.
6. `transfer` lines, `summary` and the final error as in 5.9. Fatal: a lost connection,
   `disk_full` from the server. An interrupt as in 5.9: no summary.

Round trips for N small files in D new directories: Σ ⌈directories per level ÷ 16⌉ × 1 for the
`mkdir`s, plus ⌈N ÷ 16⌉ × 3. Requests: 2 per directory, 7 per small file.

### 5.11 Failure, interruption, cleanup

**After an interrupt** (SIGINT, Ctrl-C, SIGTERM), `run_blocking`'s existing branch becomes:

```
BUSY is set once, before the command, and cleared once, after the cleanup      (finding 13)
the command's future has been dropped
    → every LocalTemp in it has removed its file; the stream writer is still in its slot;
      the JoinSet has aborted its tasks
left = the cleanup, itself under the interrupt select: sess.cleanup(true)
       (waits for local work in flight, abort() on the writer, the deletes together; 5.2)
       or, if a further signal ended it, sess.temps_left()
sess.abandon()                                 // Stage 1: in_flight → the connection is replaced
error kind interrupted, message "interrupted" (+ "; left on the server: …" if left is not empty),
left_behind = left; the done line; exit code 130 as before
```

`run_blocking` sets `BUSY` itself around both selects instead of leaving it to `interruptible`.
In the first version `BUSY` was false between the dropped command and the cleanup, and a SIGINT
in that gap made the signal task of a non-interactive run call `exit(130)` with the temporary
files in place. Now a signal in the gap finds `BUSY` set and no waiter: it is dropped, and the
cleanup runs. A signal **during** the cleanup ends it; what it had not removed is named, and a
small file that was being written locally at that moment may leave its temporary file.
SIGTERM at the prompt still exits at once: no command is running, so no temporary file exists.

The summary of a recursive transfer is not printed after an interrupt (5.9).

**The matrix.** "Final" is the final name, "temp" the temporary file. Under the final name there
is never anything but a complete file, in every row; "old file" means one that was there before.

| Event | `get`: what is left | `put`: what is left | The user is told |
|---|---|---|---|
| the target exists, no `-f` | nothing touched, nothing sent | large: nothing created. small: temp written, then deleted | `already_exists`, exit 1 (or the prompt) |
| the target appears during the transfer, with or without `-f` for `get`, without for `put` | temp removed; the file that appeared untouched | temp deleted; the file that appeared untouched | `already_exists`, exit 1 |
| the remote target is a directory | — | large: nothing created. small: temp written, then deleted | `is_a_directory` with the hint to write `<name>/`, exit 1, also with `-f` |
| the source or the target name has the temporary form | nothing sent | nothing sent | `invalid_name`, exit 1 |
| the source cannot be read (missing, denied, a directory) | nothing created | nothing created | the kind, exit 1 |
| a server error in the middle (read or write refused) | temp removed | writer aborted, temp deleted | the kind, exit 1 |
| the remote disk is full | — | the error comes from `finish`; temp deleted; old file untouched | `disk_full`, exit 1 |
| the local disk is full, a local write fails | temp removed; the connection is replaced before the next command | — | `local_io`, exit 1 |
| the local file cannot be read, or changes while it is read | — | writer aborted, temp deleted | `local_io` / `other`, exit 1 |
| the remote file shrinks during the download | temp removed | — | `other` (changed during the transfer), exit 1 |
| the connection is **closed** in the middle | temp removed | one new connection deletes the temp; if that fails, temp left and named | `connection_lost`, exit 1; not run again; the next command works |
| the connection is closed between the rename request and its answer | — | the new connection finds no temp: the rename happened and the final file is complete. Or it finds and deletes it: the rename did not happen | `connection_lost` with `the temporary file is gone, so <final> is probably complete; check with ls`, or the plain message; exit 1 either way |
| the link goes **silent** | temp removed, after the deadline | temp **left** on the server (no second connection is tried), named; the server keeps it open for the dead session until it notices, and until then `rm` and a new `put` of that name fail with "in use" | `connection_lost`, exit 1, `left_behind` |
| Ctrl-C, SIGINT, SIGTERM in the middle | temp removed | writer aborted and temp deleted on the same connection within 2 s; else (timeout, sharing violation, no credits, connection gone) deleted on one new connection; if both fail, left and named | `interrupted`, exit 130 (the shell: back to the prompt after Ctrl-C) |
| a signal in the small-file phase of a recursive transfer | files being written locally are finished or never started (the cleanup waits for them); no temp | up to 16 temps deleted together, as in the row above | `interrupted`, 130, no summary |
| a signal during the cleanup | a small file being written at that moment may leave its temp, unnamed | temp left, named | `interrupted`, `left_behind`, 130 |
| a signal between the last byte and the rename | temp removed | temp deleted | as above; the transfer did not happen |
| a signal after the rename | final complete | final complete; the delete finds nothing | `interrupted`, 130, although the file is there |
| `kill -9`, a crash of rsmb, power loss on the client | temp left locally, partial | temp left on the server, partial (the server closes it when the connection drops) | nothing; the next transfer of the same name removes it |
| power loss on the client just after a `get` reported success | final complete (it was synced before the rename), or absent if the rename itself was not yet on disk | — | — |
| the rename fails (target appeared, a directory in the way, no right to rename) | temp removed, old file untouched | temp deleted; if the delete is refused too, left and named | the kind, exit 1 |
| a second rsmb transfers to the same target at the same time | the second fails at once (`other`, the lock), touches nothing | the second's open of the temp is refused while the first writes. Between the first's CLOSE and its rename there is a window in which a second upload of a **small** file can run to its end: the final name then holds one complete file, the second's, while the first reports success and the second `not_found` (section 10, assumption 10: reproduced by the adversary, documented, not mitigated) | an error for one of the two; in the window case the reports are the wrong way round |
| a crash between the hard link and the unlink of a `get` without replace | final complete, and the temp name is a second name of it; the next `get` of that file sees two links, removes the temp name and starts with a new file | — | nothing |
| a symbolic link inside the local destination tree of a `get -r` | nothing is written through it; its subtree is not fetched | — | one `error` with `path`, exit 1 |
| recursive: one file fails | that file's temp removed; the others go on | the same | one `error` with `path`; exit 1 at the end |
| recursive: a fatal event | the temps of the files in flight are removed or deleted as in the single-file rows; complete files stay | the same | the event's error; the summary says how many arrived |

A lost connection during `mkdir`, `rm` or `rmdir` is reported with `connection lost: …; '<the
command>' was not run again and may or may not have taken effect`. `run_parsed` adds that tail
for every command with `rerun: false`.

### 5.12 Timeouts, credits and reconnect in a running transfer

- **The deadline is per request, not per transfer.** A transfer may take any time. Each READ or
  WRITE of at most 512 KiB, each small file of at most 256 KiB, each walk listing of at most
  1 MiB + 64 KiB is one reply that must be complete within 6 × `--timeout` (Stage 1, 5.10). What
  is queued in front of a request counts against it: at most 4 MiB in every phase, so a link
  has to carry 4 MiB in 180 s (23 KiB/s) with the default. wan-bad carried 100 KB/s in Stage 0.
- **A silent link** fails the request in flight after between `--timeout` and max(`--timeout`,
  15 s), as for every command, and the transfer with it.
- **Credits.** The crate waits for credits while other requests are outstanding and sizes its
  chunks to the window. On a 16-credit server a stream runs with 64 KiB requests (Stage 0: 50
  MiB each way, no stall), small files with 3 credits each, about five at a time, walk listings
  with 4 credits each, four at a time (2.3). `Session::remote()` sizes the walk buffer to the
  credits at hand, so a listing compound can always be funded. A server that never grants 4
  credits cannot be walked (Stage 1's limit for `ls`).
- **Reconnect.** A `Remote` is never used after its connection was replaced. No data moves on a
  new connection inside the command that lost the old one; the new connection of 5.2 carries
  deletes only. A connection that died while idle is replaced by `Session::remote()` before the
  transfer starts, as for every command that sends.
- **`abandon`.** Unchanged: an interrupt with `in_flight` set marks the connection dead. For a
  transfer that is always the case, so the command after an interrupted transfer starts with a
  reconnect (4 round trips). The alternative, waiting for every abandoned answer, would be
  cheap at the slow endpoint (2.4) but not on a server with 16 credits, all of which the
  abandoned requests can hold (Stage 1, finding 5).

### 5.13 `cat`

`cat <file>`: `Remote::download` with a sink that writes each chunk to stdout through
`Out::raw(&[u8]) -> Result<()>` (no newline added, bytes as they are; a failed write is
`local_io`, which ends the download, marks the `Remote` dirty and, as for every broken stdout,
ends the run with exit 1). Binary data goes out unchanged; a file of any size is never held in
memory. With `--json`: `Part::Start` above `CAT_JSON_MAX` fails before anything is read; else
the data is collected and printed as one `content` object, or fails if it is not UTF-8 (S2-4).
A directory → `is_a_directory`. Nothing is drawn on stderr. `rerun: false` (D4).

### 5.14 `mkdir`, `rm`, `rmdir`

| Command | Does | Requests / flights | Errors |
|---|---|---|---|
| `mkdir <dir>` | `Remote::mkdir(path, false)` | 2 / 1 | `already_exists` (a directory or a file has the name), `not_found` (a parent is missing; the message names `-p`), `access_denied`, `invalid_name` |
| `mkdir -p <dir>` | `Remote::mkdir(prefix, true)` (open-or-create, 5.2) for every prefix of the path below the share root, from the top | 2 / 1 per component | `not_a_directory` when a file has the name of any component, the last included (so `mkdir -p` never reports a directory that is not there, finding 8); `access_denied`, `invalid_name` |
| `rm <file>` | `Remote::remove(path, false)` | 3 / 1 | `not_found`, `is_a_directory` (the message names `rmdir`), `access_denied`, `other` for a file the server will not delete |
| `rmdir <dir>` | `Remote::remove(path, true)` | 3 / 1 | `not_found`, `not_a_directory`, `not_empty`, `access_denied`; removing the working directory or one above it is `usage` (`cd` out of it first) |

No pattern is expanded (`rm *` removes a file called `*`, which SMB does not allow: `invalid_name`),
nothing is recursive, there is no `-f`. Each needs a selected share and exactly one path.

### 5.15 `lcd`, `lpwd`, `lls`

No request, no share needed.

- `lcd [<dir>]`: `std::env::set_current_dir`; without an argument `$HOME` (unset → `usage`). A
  leading `~` is `$HOME`. An error is `local_io` with the system's text.
- `lpwd`: the local working directory; with `--json` one `lpwd` object.
- `lls [<dir>]`: the entries of a local directory (default: the working directory), sorted by
  name, names starting with a dot included, in the line format of `ls` (`d` or `-`, size,
  modification time in UTC, name, `/` after a directory; a symbolic link shows what it points
  to, a dangling one as a file of size 0); with `--json` one `local_entry` each.

Local paths are relative to the process's working directory, which `lcd` changes. All local
errors are kind `local_io`, except the rules of 5.5 to 5.7 that have their own kind.

### 5.16 Cache invalidation, per command

Invalidation happens before the command's first request and whatever its outcome: after a
failure the state of the directory is not known either.

| Command | Dropped from the cache |
|---|---|
| `put <file>` | the directory of the target (the temporary file and the rename are both in it) |
| `put -r` | the directory the tree goes into, and everything at and below `<that>/<base>` |
| `mkdir` | the parent of the new directory; with `-p` the parent of every component |
| `rm` | the directory of the file |
| `rmdir` | the parent of the directory, and everything at and below the directory |
| `get`, `cat`, `lcd`, `lpwd`, `lls` | nothing |

`ls` and `refresh` as in Stage 1. A walk does not fill the cache.

### 5.17 The command layer

- `parse` returns the flags as well; `dispatch(cmd, sess, out, flags, args)`.
- `enum Arg { Command, Share, RemoteDir, RemotePath, LocalPath, LocalDir }`.
- `Out` remembers the command line being run (`Out::begin(line)`, set by `run_line`), so that
  `Out::item_error(&Error)` can print an `error` inside a recursive transfer with its
  `command`. `Out::error` prints `path` and `left_behind` when they are set. `transfer` and
  `summary` lines are flushed as they are written, so a program reading a long transfer sees
  them as they happen.
- **The prompt.** `enum Flow { Ok, Failed(u8), Exit, Confirm(String) }`, `Out::interactive`
  (set by `shell::run`). `run_line(sess, out, line, force)`: when `force` is true the command
  runs as if `-f` had been given. When a `get` or `put` without `-r` and without `-f` fails
  with `already_exists` and `out.interactive` is set, `run_line` prints nothing and returns
  `Flow::Confirm(message)`. The shell's loop then asks with the line editor,
  `<message> Overwrite? [y/N] ` (history off for that one line), and on an answer starting with
  `y` or `Y` runs the same line again with `force`; anything else, Ctrl-C and Ctrl-D mean no:
  the error is printed as usual. The non-interactive runner, and `-S` and `-C` lines before the
  shell, never see `Confirm`.
- Completion, by the table: `get` [`RemotePath`, `LocalPath`], `put` [`LocalPath`,
  `RemotePath`], `cat` [`RemotePath`], `mkdir` and `rmdir` [`RemoteDir`], `rm` [`RemotePath`],
  `lcd` and `lls` [`LocalDir`]. A local completion reads the directory part with `read_dir`
  (with `~`), takes the names that start with the typed prefix, directories only for
  `LocalDir`, and writes them like remote ones (`quote`, `/` after a directory). No request.

Table entries (summary; `details` says what 5.5 to 5.15 say, in the table's own words, with the
cost in requests):

| Command | Syntax | Flags | Examples (line — what it does) | `rerun` |
|---|---|---|---|---|
| `get` | `get [-r] [-f] <remote> [<local>]` | `-r` copy a directory and everything below it; `-f` replace an existing local file | `get /sizes/65537.bin` — fetch one file into the local working directory · `get /sizes/1.bin copy.bin` — fetch it under another name · `get -f /sizes/1.bin notes.txt` — replace the local file notes.txt · `get -r /tree/d0/d0` — fetch the directory d0 and everything below it into ./d0 | false |
| `put` | `put [-r] [-f] <local> [<remote>]` | `-r` copy a directory and everything below it; `-f` replace an existing remote file | `put notes.txt` — upload into the remote working directory · `put notes.txt renamed.txt` — upload under another name · `put notes.txt olddir/` — upload into the directory olddir (the trailing / says directory) · `put -f notes.txt old.txt` — replace the remote file old.txt · `put -r photos` — upload the directory photos and everything below it | false |
| `cat` | `cat <file>` | | `cat /tree/d0/file.txt` — print a remote file | false |
| `mkdir` | `mkdir [-p] <dir>` | `-p` create missing parents; no error if it exists | `mkdir newdir` — create a directory · `mkdir -p a/b/c` — create a path of directories | false |
| `rm` | `rm <file>` | | `rm old.txt` — delete one file | false |
| `rmdir` | `rmdir <dir>` | | `rmdir olddir` — remove an empty directory | false |
| `lcd` | `lcd [<dir>]` | | `lcd photos` — change the local working directory · `lcd` — go to the home directory | false |
| `lpwd` | `lpwd` | | `lpwd` — print the local working directory | false |
| `lls` | `lls [<dir>]` | | `lls` — list the local working directory · `lls photos` — list a local directory | false |

The examples of `get`, `put`, `mkdir`, `rm` and `rmdir` are written for **a remote working
directory you may write to that holds a file `old.txt` and an empty directory `olddir`, and a
local working directory that holds `notes.txt` and a directory `photos`**; those of `lcd` and
`lls` for the same local directory. `help all` says this in one sentence in its "examples"
topic, and the test `examples_run` builds exactly that (6.4, H3).

`help all` gains: the nine commands; the new JSON types; a topic "transfers" (temporary names,
what `-f` does and that it replaces only what was there when the transfer started, that names
of the temporary form are never transferred, what is left after a failure and how it is named,
that a leftover on the server stays "in use" for a while after a silent link, that nothing is
resumed, that a recursive transfer exits 1 if anything was not transferred and prints no
summary after an interrupt, the trailing `/` of `put`); the
`not_empty` kind; in "timeouts and reconnect" the sentences of 5.12.

### 5.18 Progress

```rust
struct Progress { /* active: bool, total: u64, done: u64, files: (usize, usize), started: Instant, last_draw: Instant */ }
impl Progress {
    fn new(json: bool, label: &str) -> Progress        // active only if !json and stderr is a terminal
    fn total(&mut self, bytes: u64, files: usize)
    fn add(&mut self, bytes: u64)                      // redraws at most every 100 ms
    fn file_done(&mut self)
    fn clear(&mut self)                                // before a line goes to stdout, and at the end
}
```

- One line on stderr, redrawn in place (`\r`, the text, `\x1b[K`): `get medium.bin  37%
  18.5 MiB / 50.0 MiB  17.2 MiB/s  eta 2 s`; in a recursive transfer the label is followed by
  `120/341 files`; during the walk `listing: 37 directories`.
- Drawn when the total is known, at most every 100 ms, and once at 100 %; then cleared. So a
  transfer that takes 50 ms still shows `100%` on a terminal.
- Not active when stderr is not a terminal, and never with `--json`: then nothing at all is
  written to stderr by a successful transfer.
- The rate is bytes ÷ elapsed since the first byte; sizes in KiB, MiB, GiB with one decimal.

### 5.19 Output: text, JSON, exit codes

Text mode, stdout: one line per file that arrived, `get <remote> -> <local>  <bytes> bytes
<seconds> s` (for `put`: `put <local> -> <remote>  …`); after a recursive transfer one summary
line, `<n> files, <bytes> bytes, <seconds> s; <m> not transferred`. `cat`: the file's bytes.
`mkdir`, `rm`, `rmdir`, `lcd`: nothing. Errors on stderr as `rsmb: <message>`, each item error
of a recursive transfer as it happens.

New JSON objects (each added to `JSON_OBJECTS`):

| `type` | Fields | When |
|---|---|---|
| `transfer` | `direction` `"get"` or `"put"`, `remote` string (from the share root, starting with `/`), `local` string (absolute path), `bytes` integer, `seconds` number | one per file that arrived |
| `summary` | `direction`, `files` integer (arrived), `bytes` integer, `failed` integer (not transferred: files, directories and other entries), `directories` integer, `seconds` number | once, at the end of a recursive transfer, also after a fatal error |
| `content` | `path` string, `size` integer, `text` string | `cat` with `--json` |
| `local_entry` | `name`, `path` (absolute) strings, `is_dir` bool, `size` integer, `mtime` string (RFC 3339, UTC) or null | `lls` |
| `lpwd` | `path` string | `lpwd` |
| `error` (extended) | as in Stage 1, plus `path` string (the remote path for `get`, the local path for `put`; only for an error about one item of a recursive transfer) and `left_behind` array of strings (temporary files that could not be removed: remote ones as `/dir/.rsmb-….part` on the selected share, local ones as absolute paths); each present only when it has something to say | any failure |

A recursive transfer that leaves something out prints, in this order: `transfer` and item
`error` objects as they happen, `summary`, one final `error` (kind `other`, or the kind of the
fatal event), `done` with `ok: false`.

Exit codes are unchanged: 0, 1, 2, 3, 130. A failed transfer is 1; an interrupted one 130.

## 6. Tests

### 6.1 The harness, additions to `tests/common/mod.rs`

```rust
/// A directory of its own under upload/ on `ep`'s server, owned by tester; removed on drop, also when the test fails.
pub struct Scratch { pub container: &'static str, pub name: String /* "rsmb-test-<pid>-<n>" */ }
impl Scratch {
    pub fn new(ep: Ep) -> Scratch
    pub fn remote(&self) -> String                         // "/upload/rsmb-test-…"
    pub fn server(&self) -> String                         // "/srv/share/upload/rsmb-test-…"
    pub fn sh(&self, script: &str) -> String               // docker exec <container> sh -c, in server()
}
pub fn server_sha(container: &str, path: &str) -> String   // docker exec … sha256sum
pub fn local_sha(path: &Path) -> String                    // the host's sha256sum
/// Relative path (bytes) → sha256 for every regular file below `dir`; and the set of directories.
pub fn server_tree(container: &str, dir: &str) -> (BTreeMap<Vec<u8>, String>, BTreeSet<Vec<u8>>)
pub fn local_tree(dir: &Path) -> (BTreeMap<Vec<u8>, String>, BTreeSet<Vec<u8>>)
pub fn temps_on_server(container: &str, dir: &str) -> Vec<String>     // find … -name '.rsmb-*.part'
pub fn temps_local(dir: &Path) -> Vec<PathBuf>
pub fn run_in(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output   // rsmb with that working directory
pub fn run_sh(script: &str) -> Output                      // sh -c, for a child with a file size limit
impl Client { pub fn cmd_within(..) /* exists */; pub fn kill(&mut self, signal: &str) }
pub fn wait_file(path: &Path, at_least: u64, limit: Duration)         // until a local file has grown that far
pub fn wait_server_file(container: &str, path: &str, at_least: u64, limit: Duration)   // the same in the container (stat)
pub fn wait_no_open(container: &str, name: &str, limit: Duration)     // until `smbstatus -L` lists no open of that name
pub fn fnv_temp(name: &str) -> String                      // the temporary name, computed by the test's own FNV
```

Rules:

- Both tree functions run the same pipeline on their side, `find . -type f -print0 | sort -z |
  xargs -0 sha256sum` and `find . -type d -print0`, and compare paths as byte strings.
- Every test that writes to a server does it below a `Scratch`. Every test starts its session
  with `lcd <its own tmp directory>`, so nothing depends on the working directory the test
  binary was started in. `./test.sh` still removes `upload/rsmb-test-*` at its start.
- A test that acts in the middle of a transfer waits for an observed event, never a time:
  `wait_requests("READ", 8, …)` or `("WRITE", 8, …)` on the tap, `wait_file` on the local
  temporary file, or `wait_server_file` on the remote one.
- A test that reuses a remote temporary file after a kill or a silent link first calls
  `wait_no_open`: the server keeps the dead session's handle for a while (finding 3).
- **A test that asserts a time, or a number of new connections that depends on the 2 s of the
  cleanup, runs in `tests/serial.rs`.** `tests/slow.rs` runs its tests side by side on one
  `wan` link of 165 Mbit/s, and several of them stream 50 MiB over it (finding 10).
- The 1.1 GiB and 4.3 GiB files a test fetches are removed by a guard when the test ends.
- `tiny` is used by one test only; its guard empties `/srv/tiny`.

### 6.2 `./test.sh`

Steps and locks as in Stage 1. The budgets change to **120 s for `fast`, 420 s for the full
run** (master plan section 6). Expected counted time: check 27 s, fmt, clippy and unit tests
5 s, `tests/fast` 30 s (13 today), `tests/slow` 75 s (9 today; `over_4g` about 45 s of it,
alone on its core), `tests/serial` 225 s (105 today; the review moved three tests here and
added two, about 50 s), tshark 3 s: about 62 s for `fast`, about 365 s for the full run. That
leaves 55 s of the 420. If the first full run is over, the first candidate to move to the
bench is X22 (it prints rates and asserts no bound); the budget line of `test.sh` shows it. One test is longer than 30 s, `over_4g`; `docs/testing.md` says why it
cannot be shorter (4.3 GiB twice and two checksums over them).

### 6.3 Unit tests (no server)

| Test | File | Asserts |
|---|---|---|
| `fnv_vectors` | transfer | FNV-1a 64 of `""` is `cbf29ce484222325`, of `"a"` `af63dc4c8601ec8c` |
| `temp_names` | transfer | 27 bytes, `.rsmb-` + 16 lower-case hex + `.part`; the same for the same name; different for `a.txt` and `A.txt`; for a 250-byte and a 255-byte name still 27 bytes; `is_temp_name` is true for every `temp_name(..)` and false for `.rsmb-0123456789abcdef.par`, `.rsmb-0123456789ABCDEF.part`, `x.rsmb-0123456789abcdef.part`, 15 and 17 digits, a `g` among the digits |
| `local_components` | transfer | refused: `""`, `.`, `..`, `a/b`, `/abs`, `a\b`, a name with NUL; accepted: `...`, `.hidden`, `a..b`, two leading spaces, every name of `CJK_NAMES` and `SPECIAL_NAMES`, a 250-byte name (master row 2.19) |
| `local_temp_lifecycle` | transfer | in a temporary directory: `create` makes the file; a second `create` for the same final name fails with the "another transfer" message and leaves the first intact; dropping removes it; `commit` without replace onto an existing final → `already_exists`, the existing content unchanged, the temporary file gone; with replace → replaced; a stale temporary file with old content is reused and holds only the new content; a symbolic link under the temporary name is an error and its target is untouched; replace onto a directory → `is_a_directory`; **a temporary name that is a second hard link of a file with content** (what a crash between link and unlink leaves): `create` does not truncate that file (its content and length are unchanged afterwards), the new temporary file is a different inode with one link (finding 5); `commit(.., false)` when the final name was created after `create`: `already_exists`, that file unchanged (P17) |
| `target_rules` | transfer | `local_target` and `remote_target` for: no second argument, an existing directory, a trailing `/`, a file path, `-r` with and without a directory, the share root as source, `~` |
| `case_collisions` | transfer | `Case.txt` and `case.txt` both flagged; `café` in NFC and NFD not; the Korean NFC/NFD pair not |
| `progress_format` | transfer | sizes and rates at the unit boundaries; the line for a single file and for a recursive transfer |
| `walk_visits_each_directory_once` | walk | a fake tree with `self` (the id of its directory) and `up` (the id of the start's parent): every real directory listed once, `links` holds both with the first path where known, the walk ends |
| `walk_depth_cap_without_ids` | walk | a fake tree that never ends, every id 0: ends, deepest listing at depth 100, `capped` not empty |
| `walk_goes_on_past_denied` | walk | a denied directory and one that fails with `not_found`: in `denied` and `failed`; everything else listed |
| `walk_width` | walk | a fake lister that counts concurrent calls over a directory with 200 subdirectories: the most at once is 16; never more |
| `walk_fatal_error_waits` | walk | one listing fails with a lost connection: the walk returns that error only after the listings in flight have returned, and starts none after it |
| `walk_max_depth` | walk | `max_depth` 1 and 2: which directories are listed (for Stage 3) |
| `walk_breadth_cap_without_ids` | walk | a fake tree in which every directory has two subdirectories and every id is 0, with the cap lowered to 1,000 through a parameter of the inner function: the walk ends after 1,000 such directories, well before depth 100, and `capped` is not empty (finding 16) |
| `local_walk_rules` | transfer | in a temporary directory: a symbolic link to a file and one to a directory are reported and not entered; a directory with mode 0000 is one error and nothing below it is in the list; a name of the temporary form is reported; the list is sorted by path; a directory seen twice by device and inode (simulated by handing the function a pre-filled set) is reported and not entered |
| `local_dirs_never_through_links` | transfer | the function that prepares the local directories: `base/sub` is a symbolic link to another directory → an error for `sub`, nothing created behind the link, the subtree marked left out; a file where a directory is to be → `not_a_directory`; a real directory is used; a missing one is created (finding 7) |
| `same_local_file_twice` | transfer | two targets with equal device and inode (two hard links stand in for a folding file system) are both reported; `existed` is false for a target that is not there (finding 6) |
| `cache_invalidate` | session | exact entry; subtree; other shares untouched; the root with subtree empties the share; keys ignore case |
| `classify_stage2` | session | `DIRECTORY_NOT_EMPTY` → `not_empty`; `SHARING_VIOLATION` → `other` with `in use`; `CANNOT_DELETE` → `other`; fifteen kind names; exit codes |
| `cleanup_goes_to_a_new_connection` | session | the pure function that decides what a failed cleanup delete means: a lost connection, a sharing violation, `CreditStarvation` and the timeout → "new connection"; `NotFound` → "gone"; `AccessDenied` → "stays, no second try" (finding 4) |
| `rename_errors_have_one_wording` | session | the pure function that words a failed rename from (status, replace, what `stat` found): the four cases of 5.3 (finding 11) |
| `valid_names` | session | refused: `a:b`, `x?`, `star*`, `quote"`, `back\slash`, a trailing dot, a trailing space, a control character, the empty name; accepted: every fixture name, a name with U+F022 |
| `registry_refuses_a_second_use` | session | adding a temporary name twice fails the second time; taking it out frees it |
| `flags_of_get_and_mkdir` | commands | `get -r -f a b`; `get -- -r`; `get -rf` is a positional argument; `mkdir -p x` |
| `table_is_complete`, `help_all_has_everything` | commands | as in Stage 1, now over 17 commands; the "transfers" topic and the scratch sentence are in the manual |
| `local_candidates` | shell | in a temporary directory: files and directories by prefix, directories only for `LocalDir`, `/` after a directory, a name with a space quoted |

### 6.4 Integration tests

Every test runs the real binary. "tap" = through `smbtap`; "direct" = to the endpoint. Server
checksums are taken with `docker exec … sha256sum`. "No temp" means `temps_on_server` of the
scratch directory and `temps_local` of the test's directory are both empty. The assertion text
is what the failure message says.

**`tests/fast.rs`** (fast endpoint, parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| F21 | `get_put_every_size` | direct; each of the 11 files of `sizes/`: `get`, local sha against the server's; `put` into the scratch directory, server sha against the source's; the `transfer` objects | "sha256 of `<n>.bin` differs: server …, result …" never; `bytes` = n for each, the 0-byte file included; no temp | 2.1 |
| F22 | `threshold_boundary` | direct; local files of 262,143, 262,144 and 262,145 bytes made by the test: `put`, then `get` back under another name; through the tap for the counts | checksums equal on the server and after the round trip; the first two `put`s are 7 requests (WRITE 1), the third has CREATE 3 and QUERY_INFO 3 (the stream path) | 2.1, S2-5 |
| F23 | `transfer_costs` | tap; in a scratch directory | `get /sizes/65537.bin`: "expected 7 requests (CREATE 1, QUERY_INFO 4, READ 1, CLOSE 1) in 3 flights"; `get /sizes/1048577.bin`: READ 3, 9 requests; `put` of 3,000 bytes: "7 requests (CREATE 2, WRITE 1, FLUSH 1, CLOSE 2, SET_INFO 1) in 3 flights"; `put` of 1,048,577 bytes: CREATE 3, QUERY_INFO 3, WRITE 3, FLUSH 1, SET_INFO 1, CLOSE 3 (14 requests), the same with `-f` (the check is always made); `cat /sizes/1.bin` 7; `mkdir d` 2 in 1 flight; `mkdir -p a/b/c` 6 in 3; `rm` 3 in 1; `rmdir` 3 in 1; `lcd`, `lpwd`, `lls`: 0; a `get` refused because the local file exists: 0; a small `put` refused: 12 (7, the delete 3, the `stat` 2); a large `put` onto a directory, with `-f`: 2 (no WRITE) | 2.15, S2-2 |
| F24 | `structure_round_trip` | direct; `get -r /tree`, `/deep`, `/chain`; then `put -r` of each into the scratch directory; a locally made tree with an empty directory, put and fetched back | `tree`: the local tree equals the server's without `forbidden/` and `unreadable.txt` (paths and checksums), exit 1, exactly two item errors (`access_denied` with those two paths), `summary.failed` 2; `deep` and `chain` (60 levels): equal, exit 0; each tree put back equals what was fetched, in `<scratch>/<name>/…` (decision 4); the empty directory exists on both sides; a second `put -r` of the same tree without `-f`: every file `already_exists`, nothing changed; with `-f`: exit 0. **Leftovers** (finding 1): a local tree holding `a.txt` and a file named `temp_name("a.txt")` with other content: `put -r`, with and without `-f`: `a.txt` arrives with its own checksum, the leftover is one `invalid_name` error and is not on the server, exit 1; the mirror for `get -r` (the leftover planted in the scratch directory): `a.txt` local with its checksum, the leftover not fetched, exit 1; `get <leftover>` and `put <leftover>` alone: `invalid_name`, 0 requests; `cat` and `rm` of it work | 2.7, P16 |
| F25 | `names_round_trip` | direct; `get -r /cjk`, `/special`, `/longnames`; `put -r` of each into the scratch directory; then `rm` and `rmdir` by name | names byte-identical locally and on the server (`find -print0` against `find -print0`); checksums equal; `special`: `Case.txt` and `case.txt` are both fetched with their own content, and on the way back both are reported (`invalid_name`, two errors), neither is on the server, the other 20 files and the directory chain are, exit 1; `rm` of every uploaded `cjk` name and `rmdir` of the nine directories succeed, then the directory is empty on the server | 2.8 |
| F26 | `mangled_names` | direct; `get -r /mangled` | 5 files fetched, each containing a name of `MANGLED_NAMES`; 4 item errors of kind `invalid_name`; exit 1; no temp | 2.9 |
| F27 | `link_loops` | tap; `get -r /loops` | ends; "expected 1 listing (CREATE 2, QUERY_DIRECTORY 2, READ 1, CLOSE 2: 7 requests)"; the local tree is exactly `loops/file.txt`; two item errors naming `/loops/self` and `/loops/up`, the first with `/loops` in its message; exit 1 | 2.10 |
| F28 | `overwrite_rules` | tap; local and remote targets that exist, small and large | every row of 5.7 without a prompt: without `-f` kind `already_exists`, exit code 1 in a one-shot run, the existing file's checksum unchanged on its side, no temp; with `-f` the new checksum; `get` onto an existing directory name → `is_a_directory`; `put x dir` (no slash), small and large, with and without `-f`: kind `is_a_directory`, the message contains `dir/`, nothing in or instead of the directory, no temp; `put x dir/` → `dir/x` | 2.11, finding 11 |
| I21 | `overwrite_prompt` | pty; `get` and `put` onto existing targets | the line `Overwrite? [y/N]` appears; `n` → the error line, checksum unchanged; `y` → replaced; Ctrl-C at the question → not replaced, the prompt returns; Up after it does not recall `y` | 2.11 |
| F29 | `disk_full_tiny` | direct, share `tiny`; `put` of a 12 MiB local file, then of a 5 MiB one | first: kind `disk_full`, exit 1, `docker exec … ls -A /srv/tiny` empty; second: succeeds, checksum equal (the share is usable afterwards) | 2.13 |
| F30 | `read_only_share` | direct, share `readonly`; `put` small and large, `put -r`, `mkdir`, `mkdir -p`, `rm`, `rmdir` on fixture names | each `access_denied`, exit 1; the fixture untouched (checksum of the `rm` target, the directory still there); `get` and `cat` there work | 2.14 |
| I22 | `file_ops_and_cache` | pty through the tap, in a scratch directory | after `ls` (listing cached): `mkdir newdir`, then `cd n` Tab → `newdir/`, "expected exactly one listing (5 requests) for the Tab after mkdir"; a second Tab 0; the same after `put`, `rm`, `rmdir` and `put -r`, the Tab showing the new state each time; each effect seen in the container; `rmdir` of a directory with a file → `not_empty`; of the working directory → `usage` | 2.15 |
| F31 | `cat_output` | direct | `cat /large/medium.bin` to a file: sha equal to the server's (binary, 50 MiB); peak RSS ≤ 64 MB; `cat /large/big.bin \| head -c 1000`: rsmb ends within 5 s with a non-zero code and no `panicked`; `--json`: `cat /tree/d0/file.txt` gives one `content` with the text; `cat /sizes/65537.bin` (not UTF-8) and `/sizes/8388608.bin` → kind `other`, the message contains `get`; `cat /tree` → `is_a_directory`; stderr empty | brief `cat` |
| F32 | `local_commands` | tap | `lcd <dir>`, `lpwd`, `lls`, `lls sub`, `lcd` with `HOME` set: the right objects; 0 requests each; `lcd /nonexistent` → `local_io`, the directory unchanged; `lls` shows a `.rsmb-….part` the test planted; all of them work before `use` | brief |
| F33 | `local_failures` | direct | a child started by `bash -c "trap '' XFSZ; ulimit -f 1024; exec rsmb …"` (bash counts 1,024-byte blocks: 1 MiB; `sh` may be dash, which counts 512-byte blocks, so the test names bash) runs `get /large/medium.bin`: kind `local_io`, exit 1, no temp, nothing under the final name; `get` into a directory with mode 0555: `local_io`, nothing created; the next command of the session works | task: local disk full |
| F34 | `recursive_put_refuses_and_goes_on` | direct; a local tree with a symbolic link to a file, one to a directory, a FIFO, a file `a:b.txt`, a directory `bad?dir` with two files, a directory `locked` with mode 0000 (holding a file), a file with mode 0000, and three good files | the three good files are on the server with their checksums; seven item errors (`other` ×3, `invalid_name` ×2, the directory's saying 2 files, `local_io` ×2); **`locked` does not exist on the server**; nothing else on the server (no private-use name); exit 1; the run ends (the FIFO is not opened) | P5, P6, finding 11 |
| F35 | `single_transfer_arguments` | direct | `get f` → `<lcwd>/f`; `get f dir` → `dir/f`; `get f new.bin`; `get -r` on a file fetches the file; `get dir` → `is_a_directory`, message has `-r`, no local file of size 0 (the crate's open of a directory succeeds with size 0, finding 14); `put dir` likewise; `put f` → `<cwd>/f`; `put f sub/` → `sub/f`; `put -r d sub` → `sub/d/…`; `get -r /` into an empty directory is not run (too large); `get -r /loops x` with `x` missing → `local_io` | decision 4 |
| I23 | `progress_on_terminal_only` | pty and pipe; `get /large/medium.bin`, `put` of it back | on the pty: the output contains `100%` and `MiB/s` and the result line with `52432897 bytes`; with stdout and stderr piped: stderr empty, the result line on stdout; with `--json`: one `transfer` with `bytes` 52432897 | 2.21 |
| F36 | `mkdir_rules` | tap, in a scratch directory holding a file `f` | `mkdir d` then `mkdir d` → `already_exists`; `mkdir x/y` → `not_found`, the message names `-p`; `mkdir -p x/y/z` creates three (6 requests); `mkdir -p x/y/z` again → ok, nothing changed; `mkdir -p f` and `mkdir -p f/sub` → `not_a_directory`, `f` unchanged (finding 8); `put -r` of a local tree whose directory `f` meets the remote file `f` → one item error `not_a_directory` for it, its subtree counted, the rest uploaded | S2-8 |
| F37 | `get_r_keeps_inside_its_destination` | direct; the destination prepared with `dest/tree/d0` as a symbolic link to a directory outside `dest` that holds a file `file.txt`; `get -r /tree dest`, then again with `-f` | nothing is created or changed in the directory outside (its `file.txt` has its old checksum, also after `-f`); one item error for `/tree/d0` saying how much was left out; everything else of `tree` arrived; exit 1 | finding 7 |
| N4 | `every_json_line_has_a_type` (extended) | direct; every Stage 2 command once, a failing one of each, a recursive transfer with an item error | as in Stage 1; `path` and `left_behind`, when present, have the documented types | 1.11 |
| H2, H4 | (unchanged in form) | | now over 17 commands | 1.12 |
| H3 | `examples_run` (extended) | direct; every example in a fresh session and a fresh local directory holding `notes.txt` and `photos/a.jpg`, `photos/b.jpg`, with `HOME` set to it; for `get`, `put`, `mkdir`, `rm`, `rmdir` also a fresh scratch directory holding `old.txt` and `olddir/`, entered with `cd` first | `done` with `ok: true` for every example | 1.12, D12 |
| F14 | `counts_add_up` (extended) | tap; the extended script of X1, each run with its own scratch and local directory | as in Stage 1 | §5 method |

**`tests/slow.rs`** (slow endpoint, edge, wan-bad, stress fixtures; parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| S21 | `big_file` | tap, fast endpoint; `get /large/big.bin`, local sha; `put` of it into the scratch directory, server sha | both `ad2f1026…`; `bytes` 1,181,116,007; "peak RSS ≤ 64 MB" after each; "never more than 16 requests outstanding in the tap"; one connection | 2.2 |
| S22 | `over_4g` | direct, fast endpoint; `get /large/over4g.bin`, `put` back | both `d0b7cf9b…`, 4,617,089,843 bytes; no temp | 2.3 |
| S23 | `two_thousand_small_files` | tap, fast endpoint; `get -r /smallfiles`, `put -r` into the scratch directory | local tree and uploaded tree equal the fixture (2,000 paths and checksums, 20 directories); "expected 21 listings and 2,000 small reads: 6,084 requests" down and "21 mkdir and 2,000 small writes: 14,042" up; peak outstanding ≤ 64 down and ≤ 48 up, and above 3; peak RSS ≤ 64 MB; the time of each is printed | 2.5 |
| S24 | `small_files_counts_slow` | tap, slow; `get -r /smallfiles/d01`, `put -r d01` into the scratch directory | "expected 304 requests (1 listing, 100 files × 3)" and "702 (1 mkdir, 100 × 7)"; peak outstanding ≤ 48 and ≥ 6; checksums equal; one connection | 2.6 |
| S25 | `cut_mid_get` | tap, slow; `get /large/medium.bin`; at the 8th READ `drop_connections()` | kind `connection_lost`, `ok` false; nothing under the final name, no temp; "expected 0 new connections for the failed get"; the next `ls` works on "exactly 1 new connection"; the tap's log has no READ on the second connection | 2.12, 2.16 |
| S26 | `cut_mid_put` | tap, slow; `put` of a local `medium.bin`; at the 8th WRITE `drop_connections()` | `connection_lost`; nothing under the final name and no temp on the server (checked in the container); "expected exactly 1 new connection, carrying no WRITE and no CREATE of the target: NEGOTIATE 1, SESSION_SETUP 2, TREE_CONNECT 1, CREATE 1, SET_INFO 1, CLOSE 1"; the next command 0 new connections; an existing target (second run, with `-f`) keeps its old checksum | 2.12, 2.16 |
| S30 | `kill_9_leaves_only_temporaries` | tap, slow; `kill -KILL` at the 8th READ of a `get`, at the 8th WRITE of a `put` | nothing under the final name on either side; exactly one `.rsmb-*.part` where the data was going, with the name `temp_name` gives (`fnv_temp`); after `wait_no_open` for the remote one, a new session runs the same transfer: success, checksum equal, the temporary file gone | task: kill -9, S2-1 |
| S31 | `cut_during_mkdir` | tap, slow; `mkdir x`; at its CREATE `drop_connections()` | `connection_lost`, the message contains `may or may not`; the request log has exactly one CREATE for `mkdir` (not sent twice); the next `ls` works on 1 new connection | 2.16 |
| S32 | `edge_and_encrypted` | tap; on edge: `get` and `put` of `medium.bin`, `get -r /sizes`, `get -r /fanout` (2,001 listings, 2,000 files of a few bytes, 16 credits); on `encrypted` (fast): `get` and `put` of `medium.bin`, `get -r /sizes` and `put -r` back | checksums and trees equal; every command done within 60 s ("no stall"); on edge peak outstanding ≤ 16; on `encrypted` only opaque frames after `use` | 2.17 |
| S33 | `wan_bad_transfer` | direct, wan-bad; `get /sizes/1048577.bin`, `put` back | checksums equal; both times printed | 2.22 |
| S34 | `file_shrinks_during_get` | direct, slow; a 50 MiB copy in the scratch directory, truncated to 20 MiB in the container when the local temporary file has passed 4 MiB | kind `other`, the message contains `changed`; nothing under the final name, no temp; the next command works | 5.8 |
| S35 | `file_vanishes_during_get_r` | tap, slow; a scratch copy of `smallfiles/d01`; at the first READ the test deletes `f100.bin` in the container: the list is sorted by path (P20), so that file is in the last of seven waves, more than a second away | 99 files with their checksums; one item error `not_found` with that path; exit 1 | 5.9, finding 15 |
| S36 | `two_processes_one_target` | direct, slow (50 MiB takes about 4 s, so the two overlap; at the fast endpoint they would not, finding 15); two rsmb processes, the second started when the first's local temporary file has passed 4 MiB, both `get /large/medium.bin` to the same path; then two `put`s of that file to one remote name with `-f`, the second started when the remote temporary file has passed 4 MiB | `get`: the second exits 1 with the "another transfer" message while the first is still running, the first exits 0, the final file has the fixture's checksum, no temp. `put`: the second exits 1 (the "in use" message), the first exits 0, checksum on the server, no temp | P15 |
| S37 | `target_appears_during_put` | direct, slow; `put` of a local `medium.bin` without `-f`; when the remote temporary file has passed 4 MiB (`wait_server_file`) the test creates the final name in the container with other content | kind `already_exists`; the planted file has its own checksum and size; no temp on the server; exit status of the command 1. This is the only test in which the rename itself has to refuse: a rename sent with replace would pass every other row | finding 2 |
| S38 | `target_appears_during_get` | direct, slow; `get /large/medium.bin` without `-f`, then again **with** `-f`; each time, when the local temporary file has passed 4 MiB, the test creates the final name locally with other content | both runs: kind `already_exists`; the planted file unchanged; no temp. (With `-f` this is P17: only what was there at the start is replaced.) A third run with `-f` and the final name present from the start: replaced, checksum of the fixture | finding 2, P17 |
| S39 | `source_changes_during_put` | tap, slow; `put` of a local copy of `medium.bin`; at the 8th WRITE the test appends 1,000 bytes to the local file | kind `other`, the message contains `changed`; nothing under the final name on the server, no temp; the next command works | finding 2, 5.8 |

**`tests/serial.rs`** (one at a time, exclusive lock)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| X21 | `timing_small_files` | direct, slow; `connect_ms` just before; in one session `get -r /smallfiles/d01` and `put -r d01`, each timed from sending the line to its `done` | "get: ≤ 1.5 × (⌈100 ÷ 16⌉ × 1 + 1 listing) × round trip = 1.5 × 8 × rt" (2.4 s at 200 ms; 1.6 s measured for the files alone); "put: ≤ 1.5 × (7 × 3 + 1 mkdir) × rt = 1.5 × 22 × rt" (6.6 s; 4.5 s measured); both printed beside the sequential figures (101 and 301 round trips); checksums equal; `ctl.sh drops` unchanged | 2.6 |
| X22 | `timing_medium_slow` | direct, slow; `get` and `put` of `medium.bin` | checksums equal; MB/s of each printed beside the iperf3 figures of `docs/test-environment.md` (20 and 17 MB/s); asserts only success and that netem dropped nothing | 2.4 |
| X23 | `silent_link_mid_get` | direct to 10.213.45.30:4450, `--timeout 5`; `get /large/medium.bin`; when the local temporary file has passed 4 MiB, `ctl blackhole on` | kind `connection_lost`, "13 to 20 s after the blackhole"; nothing under the final name, no temp locally; after `blackhole off` the next `ls` works | 2.18 |
| X24 | `silent_link_mid_put` | the same with `put`, the fault when the server's temporary file has passed 4 MiB (seen in the container) | `connection_lost` 13 to 20 s after; **`left_behind` names the temporary file** and the message says so; nothing under the final name; after `blackhole off`: the file named exists on the server. Then, in a new session at once: the same `put` either succeeds or fails with the "in use" message, and in the second case nothing under the final name has changed (the old session may still hold the file: the adversary measured a sharing violation at 1.2 s and success at 14.6 s). Then `wait_no_open` (limit 60 s) and the same `put` once more: success, the right checksum, the temporary file gone | 2.18, D7, finding 3 |
| X25 | `signal_mid_put_edge` | tap, edge (16 credits); five rounds: `put` of a 300 MiB file the test makes locally (64 KiB writes, 4,800 of them); SIGINT at the 50th WRITE | every round: `interrupted`, exit 130; no temp and nothing under the final name in the edge container (after `wait_no_open`); no `left_behind`. Whether the delete went on the first connection or a second is printed, not asserted. Five rounds because the adversary saw the failure of the dropped-writer design in 4 of 11 | finding 4 |
| X26 | `signal_in_the_small_file_phase` | tap, slow; a scratch copy of `smallfiles/d01`; `put -r d01`, SIGINT at the 40th CREATE; then `get -r` of the scratch copy, SIGINT at the 40th READ | each: `interrupted`, exit 130 within 6 s; no `summary` object. `put`: every file under a final name on the server has the checksum of its source; every `.rsmb-*.part` still on the server is in `left_behind` (expected: none). `get`: every local file under a final name has the checksum of its source; no local temp after the process has ended | finding 9, 13 |
| X27 | `signals_mid_get` | tap, slow; non-interactive; SIGINT at the 8th READ of `get /large/medium.bin`; again with SIGTERM; once more with two SIGINTs 10 ms apart | an `error` of kind `interrupted`, exit 130 within 3 s of the signal; nothing under the final name, no temp | 2.12 |
| X28 | `signals_mid_put` | tap, slow; SIGINT at the 8th WRITE; again with SIGTERM; once more with two SIGINTs 10 ms apart (the second falls into the gap before the cleanup or into its start) | `interrupted`, exit 130 within 4 s; nothing under the final name on the server; for the single signals: no temp, no `left_behind`, "expected the abort and the delete on the first connection (0 new connections)"; for the double signal: no temp, or the temp is named in `left_behind` | 2.12, S2-3, finding 13 |
| X29 | `ctrl_c_in_the_shell` | pty through the tap, slow; `get` then `put` of `medium.bin`, Ctrl-C in the middle of each | `interrupted`, the prompt returns; no temp on either side; then `ls /sizes` lists 11 names within 3 s on "exactly 1 new connection" per interrupt; `pwd` unchanged | 2.12 |
| X1, X2 | `one_connection_fast`, `one_connection_edge` (extended) | the Stage 1 script followed by: `lcd`, `cd <scratch>`, `get /sizes/65537.bin`, `put 65537.bin`, `put -f 65537.bin`, `cat /sizes/1.bin`, `mkdir d`, `put 65537.bin d/`, `rm d/65537.bin`, `rmdir d`, `get -r /smallfiles/d01`, `put -r d01`, `lpwd`, `lls` | "expected 1 TCP connection in the tap and 1 on the server" | 2.20 |

Rows of master plan section 8 and where each is enforced: 2.1 F21, F22 · 2.2 S21 · 2.3 S22 ·
2.4 X22 · 2.5 S23 · 2.6 S24, X21 · 2.7 F24 · 2.8 F25 · 2.9 F26 · 2.10 F27 and the walk unit
tests · 2.11 F28, I21, S37, S38 · 2.12 S25, S26, S30, X25–X29 · 2.13 F29 · 2.14 F30 · 2.15 I22, F23,
F36 · 2.16 S25, S26, S31 · 2.17 S32 · 2.18 X23, X24 · 2.19 `local_components`, F37 · 2.20 X1,
X2 · 2.21 I23 · 2.22 S33. Brief: streaming (S21, S22, F31: peak RSS), byte counts (F21, I23),
exit status (every failure row), no file that looks complete (S25, S26, S30, S34, S39,
X23–X28, F24's leftovers, F29, F33), no silent overwrite (F28, I21, S36, S37, S38), the six
small commands (F23, F30, F31, F32, F36, I22).

Not in the gate, and why: the read-only attribute, a real DFS share, a Windows server, a local
file system that folds case or normalises names (no fixture; the rule for the last is tested
with hard links in `same_local_file_twice` and by S38); two uploaders of **different** files
to one name (the outcome is the documented limitation of assumption 10); a `put` whose rename
request is cut before its answer (it cannot be hit on purpose; the rule of 5.11 for it is a
message, tested by S26 in its other branch).

### 6.5 `testenv/bench-rsmb.sh transfers`

A second mode beside `quick` and the full navigation run; not part of the gate; **exclusive**
lock (timed bulk transfers at the slow endpoint; the lock table of `docs/test-environment.md`
gets the row). About 20 minutes. `ctl.sh drops` before and after.

| Row | Method | Reported |
|---|---|---|
| `big.bin` down ×3 and up ×3, slow | one rsmb run each, checksum checked; `iperf3` (10 s, each direction) just before | MB/s per run beside the iperf3 figure; fails if a run takes more than three times the median (the Stage 0 rule) |
| `over4g.bin` down and up, slow, once | as above | MB/s, checksum |
| `get -r /smallfiles` and `put -r` of it, slow, once | time; requests through the tap in a second run | seconds beside the wave bound (126 waves × p × round trip: 25 s and 76 s) and the sequential figure (2,000 × p round trips: 400 s and 1,200 s) |
| `get -r /tree`, slow | time | beside 23 waves × 1 round trip + ⌈341 ÷ 16⌉ × 1 |
| wan-bad: `iperf3` through 10.213.45.31:5201 both ways, then `sizes/8388609.bin` down and up | | the raw TCP rate beside the transfer's, so a client defect can be told from the link |
| fast endpoint: `get -r /smallfiles`, once | time of the product | the cost of the sync before rename (P7) as it is in the product, beside the two figures of 2.5 (0.07 s without a sync, 12.3 s with one sync after another). No switch is added to turn the sync off |

It removes what it wrote on the server and locally. The implementing agent runs it once at the
end of the stage and puts the table into `docs/performance.md`.

## 7. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` runs after T4,
T6, T8 and T10. Documentation is written with the task that makes it true; `docs/commands.md`
gets a command's section in the task that adds the command, because H2 fails otherwise.

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T0 | Groundwork | — | `Kind::NotEmpty`, `Error::path` and `left`, the `classify` additions; flags passed to `dispatch`; `Arg::LocalPath`, `LocalDir` and local completion; `Out::raw`, `begin`, `item_error`; `lcd`, `lpwd`, `lls`; the harness additions of 6.1; the budgets in `test.sh` | `classify_stage2`, `flags_of_get_and_mkdir`, `local_candidates`; F32; H2, H3 (the new context) |
| T1 | `Remote`, `mkdir`, `rm`, `rmdir` | T0 | `open_close` (and `cd` on it), `valid_name`, `crate_path`, `Cache::invalidate`, `Remote` with `stat`, `mkdir` (both dispositions), `remove`, `Session::remote` and `finish` (without the registry), the generation; the three commands with their invalidation | `cache_invalidate`, `valid_names`; F23 (these rows), F30 (these rows), F36 (the `mkdir` rows), I22 (these rows); F4 and F5 of Stage 1 still pass |
| T2 | `get` of one file, `cat` | T1 | `Remote::download`, the dirty mark; `temp_name`, `is_temp_name`, `local_component`, `LocalTemp` (with the link-count check and the `existed` rule), `local_target`, absolute local paths; `get` without `-r`; `cat`; `Progress` | `fnv_vectors`, `temp_names`, `local_components`, `local_temp_lifecycle`, `target_rules`, `progress_format`; F21 (down), F31, F33, F35 (`get` rows), I23 (down); S38 |
| T3 | `put` of one file | T2 | `Remote::put_small`, `upload` with the writer slot and the check that is always made, the rename rule and its one wording, the registry, `cleanup` (abort, deletes together, when it goes to a new connection) and its use in `finish`; `remote_target`; `put` without `-r` | `registry_refuses_a_second_use`, `cleanup_goes_to_a_new_connection`, `rename_errors_have_one_wording`; F21, F22, F23, F28, F29, F30, F35, I23 complete; S36, S37, S39 |
| T4 | The prompt | T3 | `Flow::Confirm`, `Out::interactive`, `run_line`'s `force`, the question in the shell | I21 |
| T5 | Interrupts, cuts, silence | T3 | the cleanup in `run_blocking` with `BUSY` held across it, the quiescence wait, the `may or may not` tail, the rename-cut message | S25, S26, S30, S31, S34; X23, X24, X25, X27, X28, X29 |
| T6 | The walk | T1 | `list_dir` returning `.` and `..`, `Remote::list`, `walk.rs` with both caps | the seven `walk_*` tests |
| T7 | `get -r` | T2, T5, T6 | 5.9, with the local directories made link by link, `existed`, the sorted list, the `Alive` in the write closure | `local_dirs_never_through_links`, `same_local_file_twice`; F24 (down, with the leftover rows), F26, F27, F37; S23 (down), S24 (down), S35; X26 (down) |
| T8 | `put -r` | T3, T7 | 5.10; `case_collisions` | `local_walk_rules`; F24, F25, F34, F36 complete; S23, S24 complete; S32; X21; X26 complete |
| T9 | The remaining rows | T8 | nothing new in `src/` unless a test shows a defect | S21, S22, S33; X22; X1, X2, F14, N4 extended |
| T10 | Bench and documents | T9 | `bench-rsmb.sh transfers`; the documents of section 8; the final full gate and its budget line | — |

If a count in F23, S23 or S24 differs from this plan on the first run, the implementing agent
finds out why before changing the number: the numbers come from the measurements of section 2,
and a difference is either a defect or a finding to record under "Deviations during
implementation" at the end of this file.

## 8. Documents

New, added to the index in `AGENTS.md` in the same change:

| Document | Outline |
|---|---|
| `docs/transfers.md` | The two paths and when each is taken (threshold, single file against recursive). What is in memory and in flight. Temporary names: the format, the hash, where the file is, the lock on the local one, what the crate's open does to the remote one. The rename on each side, with and without `-f`. The overwrite table. What "complete" means: the checks, the sync, what is not checked (growth, no checksum). Recursive transfers: the three phases, what counts as not transferred, the exit status, fatal against per-item errors, names that are refused, case pairs, links, empty directories, files that change. The failure matrix of 5.11 in full. What is left after a kill and how to remove it. Timeouts, credits and reconnect in a transfer. That names of the temporary form are never transferred, and why. That `-f` replaces only what was there at the start. That a leftover on the server stays "in use" for a while after a silent link. Limits: nothing resumed, nothing retried, times and attributes not kept, changes on the server during a download that are not detected (growth, a rewrite of the same size), shares where one may create but not rename, the window between CLOSE and rename for two uploaders with its exact outcome, a server that normalises names under `put -r -f`. Each rule with the test that enforces it |

Extended:

| Document | What is added |
|---|---|
| `docs/commands.md` | a section ``## `name` `` for each of the nine commands (syntax, flags, what the second argument means, cost in requests, output, errors, the examples); the scratch sentence for the examples; flags inside commands (`-rf` is not a flag); completion of local paths; the overwrite prompt; `not_empty`; in "Timeouts and reconnect": the per-request deadline in a transfer, which commands are not run again, what a failed `put` does on a new connection; differences from smbclient-ng (`get -r` keeps the structure below the named directory, temporary names, `-f`) |
| `docs/scripting.md` | the new objects with an example line each; `path` and `left_behind`; the order of objects in a recursive transfer; that no transfer ever prompts without a terminal; reading a long transfer line by line; a worked example (upload a directory, check `summary`) |
| `docs/architecture.md` | the six files; `Remote` and why it exists; the registry, the writer slot, `Alive`, `finish`, `cleanup` (what sends it to a new connection), the generation; the transfer engine; `LocalTemp`; the walk (loop, ids, depth cap, fatal errors, why the lister is a parameter); the phases and their bounds; where local I/O runs; the prompt as a `Flow`; invalidation; "What Stage 3 adds where" |
| `docs/testing.md` | the harness additions; the new budgets and the measured step times; the per-command counts of F23, S23, S24; every new test with its row; the line for `over_4g`; what is not in the gate and why |
| `docs/performance.md` | per-file costs (2.1); small files 16 wide against one after another; the walk at 1 flight per listing against Stage 0's four; the `transfers` table of the bench against iperf3; the cost of the sync before rename; peak RSS; "not done": a `put` in two flights, a compound read for a single small `get`, skipping what exists |
| `docs/library-decision.md` | under "What rsmb builds on top of `smb2`": the stream types and compound calls Stage 2 uses, `open_close`; under the behaviours: a download ends silently when the file shrinks; a full disk shows at `finish`; the private-use mapping and why rsmb refuses such names instead; "Not verified" loses the rows Stage 2 has now measured (4.3 GiB, the slow endpoint, an encrypted share) |
| `docs/known-limitations.md` | a section "Transfers": nothing resumed; a recursive transfer that failed is run again with `-f` and transfers everything again; a file that grows during a download; timestamps, attributes and permissions; names SMB forbids are refused, also on upload from Linux; names that differ only in case are not uploaded (folded with Rust's tables); local symbolic links and special files are not uploaded; a directory reached twice (a link, or a server that reuses file ids across file systems) is copied once and reported; the read-only attribute untested; one file per `rm`; `rmdir` empty only; leftovers after `kill -9`, and that one on the server is locked until the server notices a silent connection is dead; names of the temporary form are not transferred; `-f` does not replace a target that appeared during the transfer; a file rewritten in place with the same size during a download is not detected; the window for two uploaders of one name and what it does (reports the wrong way round, never a partial file); `put -r -f` to a server that normalises names differently from Rust's lower-casing can write one local file over another; a walk on a server without file ids stops after 100,000 directories; a local tree that loops through a bind mount is cut at the loop; a share where rename or delete is refused; a file system without locks or hard links; `cat --json` up to 1 MiB of UTF-8; after any interrupted or locally failed transfer the next command reconnects |
| `docs/test-environment.md` | the lock table: `bench-rsmb.sh transfers` (exclusive); "Tests": the scratch directories, the two budgets |
| `docs/development.md` | where a new `Remote` method goes and the shape every command using one has; `transfer.rs` and `walk.rs` in "Where things go"; the examples' scratch context |
| `docs/reviews.md` | the Stage 2 section, after the review |
| `README.md` | status: Stage 2; two example lines (`get -r`, `put`); the transfer rates beside iperf3 and the small-file figures |
| `AGENTS.md` | the index rows for `docs/transfers.md` and this plan (the row for this plan is added with the plan); "Working commands": `bench-rsmb.sh transfers`, the new budgets; principles, **only those the work has shown** (candidates with evidence from section 2: derive a temporary name from the final one so that a retry cleans up; let the server refuse rather than check first when a refusal is cheap; one rule for the exit status of a batch) |

## 9. What Stage 3 gets, and notes for it

- `walk::walk` with `max_depth` already in it, `Report` (denied, links, capped, failed), the
  fake-tree unit tests to extend, and `Remote::list` (4 requests, 1 flight; f = 1 for the
  bounds of master plan section 9). `tree` and `find` are two `on_dir` callbacks plus filters
  on the entries a listing already carries; neither needs a new request type.
- Every command that walks has the shape `remote()` / work / `finish()`. `tree` and `find`
  register no temporary file, so `finish` only settles `in_flight`.
- A walk is not run again inside `run_parsed` unless Stage 3 sets `rerun: true` for its
  commands; it may (they print only at the end, or `find` decides otherwise: S3-4).
- Exit status: Stage 2 decided for **transfers** that anything left out is exit 1. S3-1 (a walk
  that skipped denied directories) is still open and its default is 0; the two are different
  questions, and `docs/commands.md` has to say so side by side.
- JSON: `summary` exists with a `direction`; Stage 3 needs its own summary fields. `error`
  objects with `path` are how Stage 2 reports a directory that was not entered; Stage 3's
  `denied` and `skipped` objects (master plan section 9) are new types.
- The loop check trusts file ids. A server that gives two different directories the same id
  (different file systems under one share) loses the second one from a walk, reported as a
  link. Stage 2 makes that an error for transfers; Stage 3 decides what `find` says.
- `Arg::LocalPath`, flags in `dispatch`, `Out::item_error` and `Out::begin` are there.
- The depth cap and the cap of 100,000 directories apply only to directories without a file
  id; both end in `Report::capped`.
- `Progress` can show `listing: N directories`; `tree` and `find` draw nothing unless Stage 3
  wants it.

## 10. Assumptions this plan could not verify

Marked so that the adversary can attack them. Each says what would show it wrong. **After the
review:** the adversary checked seven of them. 2, 3, 5, 15 and 17 hold (15 with the correction
of finding 17: dash counts 512-byte blocks). 1 was wrong on a 16-credit server and the design
changed (finding 4). 9 was avoidable and is gone (finding 8). 10 reproduces and is now a
documented limitation with the decision recorded below. The numbers are kept so that references
stay valid.

1. **Shown wrong, design changed.** "A dropped writer's CLOSE reaches the server before a
   delete sent right after it, on every server": on edge the delete failed for want of credits
   and the CLOSE was never sent (finding 4). The writer is now ended with `abort()` from the
   cleanup, and a sharing violation or a credit failure sends the cleanup to a new connection.
   What is still assumed: `abort()` on a writer whose `write_chunk` future was dropped behaves
   as on one that was not (the crate documents `write_chunk` as safe to drop; the adversary's
   10 of 10 were aborts without a dropped call). X25 and X28 test exactly that.
2. **After a cut, the server has closed the old session's handle before the new connection's
   delete arrives.** Measured once at the fast endpoint (6.7 ms, first try). At the slow
   endpoint the new login takes four round trips, which should be more than enough; S26 tests
   it there. A cut the server does not see (a NAT that drops state) leaves the handle open, the
   delete is refused, and the file is named: that case is a rule, not a test.
3. **Sixteen compound reads or writes of 256 KiB fit a new connection's credits.** Measured
   with files of at most 4 KiB (3 credits each). At 256 KiB a read charges 4 + 2: 96 of 513.
   F22 moves three files near the threshold, one at a time; no test has sixteen large "small"
   files in flight. Shown wrong by a recursive transfer of many 200 KiB files that stalls.
4. **Samba's FLUSH inside a compound write, sixteen at a time, stays cheap.** 100 files took
   100 to 127 ms at the fast endpoint; 2,000 were not measured before S23.
5. **The sync before the local rename is affordable when sixteen overlap.** Measured one after
   another: 6.2 ms a file, 12 s for 2,000. S23 prints the product's time; if it is far above
   2 s the orchestrator should hear of it, because P7 is a choice the user may want to make.
   Not measured on a rotating disk or a network file system.
6. **`File::try_lock` is supported where people download to.** A file system without advisory
   locks gets no protection against a second rsmb (the lock error is ignored on purpose). Not
   tested: NFS, FUSE, CIFS mounts.
7. **`hard_link` then unlink is an acceptable "rename without replace".** On a file system
   without hard links the fallback checks and renames, with a window in which a file that
   appears is replaced. Not tested on FAT or exFAT.
8. **The read-only attribute.** This Samba does not present mode 0444 as read-only (2.2), so
   nothing here shows what `rm` or a replacing rename does to a read-only file. The crate's
   notes say Windows refuses both. rsmb reports the server's status; it does not clear the
   attribute.
9. **Gone.** `mkdir -p` over a file reported success in the first version; the open-or-create
   compound of finding 8 answers `not_a_directory`. What is still assumed: servers other than
   Samba answer the same four ways (it is in assumption 11's list).
10. **Two uploaders of one remote name: reproduced, a documented limitation.** Between the
    first's CLOSE and its rename, a second rsmb can write the same temporary name to its end.
    The adversary reproduced the bad interleaving: A's rename succeeds while the final file
    holds B's bytes, and B fails with `not_found`. What it is and is not: the final name holds
    one **complete** file that one of the two processes meant to put there; nothing partial,
    nothing lost; the two reports are the wrong way round. It needs two rsmb processes
    uploading **different** content to the same remote name at the same moment, and B's whole
    upload inside A's gap of one round trip, so in practice a small file.
    **Decision: not mitigated in Stage 2.** The cheap checks do not close it. A `stat` of the
    final name after the rename, compared with the size sent, costs every upload a round trip
    (a small `put` would go from 3 to 4) and is blind to equal sizes; a file id cannot be
    compared, because `write_file_compound` returns none. A check
    before the rename only narrows the gap. The one thing that closes it is to rename **on the
    handle that wrote the data**: for the small path a compound of our own, `[CREATE, WRITE,
    FLUSH]` then `[SET_INFO rename, CLOSE]` on the same handle, about 70 lines on the raw API,
    which would also bring a small `put` from 3 flights to 2; for the stream path the crate's
    writer offers no rename on its handle. That is recorded in `docs/known-limitations.md` and
    in "not done" of `docs/performance.md` as the fix to build if the user wants the gap
    closed; it is a change to S2-2 and to every count of a small `put`, so it is the
    orchestrator's or the user's call, not a detail of this revision. S36 tests the same-source
    case in both directions.
11. **A server other than Samba accepts what Stage 2 sends.** The compound read and write, the
    rename with `FileRenameInformation`, the delete by disposition, the directory create in a
    compound: Samba 4.23.8 only. The crate's own notes describe Windows differences (a
    replacing rename onto a directory is `ACCESS_DENIED` there; rsmb then says `access_denied`
    instead of `is_a_directory`).
12. **Names fold as the server folds them.** `to_lowercase()` decides which local names
    collide. A server with a different table may still write one file over another (names the
    plan lets through) or accept names the plan refuses.
13. **File ids are unique within a share.** See section 9. A share that spans file systems
    with equal inode numbers loses a directory from a recursive `get`, reported as an error.
14. **The budget.** The step times of 6.2 are estimates from request counts and the Stage 0
    and Stage 1 timings. `over_4g` depends on the speed of `sha256sum` in the container, which
    was not timed for 4.3 GiB.
15. **`ulimit -f` with SIGXFSZ ignored gives `EFBIG` on the write** (F33), and the limit does
    not disturb anything else rsmb writes in that test. Standard behaviour, not tried here.
16. **An aborted task never sends after the quiescence wait, and the wait ends.** The wait of
    `cleanup` step 1 relies on every sender of the command being inside a `Remote` or an
    `Alive`, on an aborted task being dropped at its next await, and on the one kind of
    closure that holds an `Alive` (the local write of a small file) ending within
    milliseconds. A local disk that hangs in `sync_data` makes the cleanup hang with it, until
    a signal ends the cleanup. A sender kept anywhere else (a static, a long-lived closure)
    would make the wait hang for good; nothing but review checks that.
17. **The stream window stays at 8 reads or 4 writes on `samba`.** Stage 0 saw peaks of 8 and
    4 (16 and 10 on edge). The crate's window is adaptive, with a cap of 4 MiB and 32 requests;
    S21 asserts at most 16.
18. **A `put` that loses its connection exactly between the rename request and its answer** is
    handled by the message of 5.11 only. Whether the rename happened is inferred from the
    temporary file being gone, which another client could also have caused.
19. **`cat` of a large file to a slow reader** holds at most the crate's 4 MiB window while
    stdout blocks; if the reader stalls for longer than the server keeps the handle or the
    session, the download fails with whatever the server says. Not measured.
20. **An interrupt during the walk of a recursive transfer** abandons up to 16 listings; the
    connection is replaced (Stage 1's rule). Nothing local or remote exists yet at that point.
    Covered by reasoning and by Stage 1's S12, not by a Stage 2 test.
21. **A server that folds names differently from `to_lowercase()`, under `put -r -f`.** The
    mirror of finding 6. Locally, a recursive `get` replaces only what was there at the start,
    so two names the local file system folds together cannot overwrite each other. On the
    server that rule cannot be applied: with `-f` the rename replaces whatever has the name,
    and a server that treats two of our names as one (NFC and NFD on a normalising server, a
    case rule Rust does not have) lets the second upload replace the first, exit 0. Without
    `-f` the second fails with `already_exists`. Samba on Linux does not normalise; not
    testable here. A limitation, stated in `docs/known-limitations.md`.
22. **`smbstatus -L` in the container shows every open the tests have to wait for.**
    `wait_no_open` relies on it (X24, X25, S30). The binary is there and prints `No locked
    files` on an idle server; that it lists a dead session's handle until the server lets go
    was not checked while planning.
23. **`abort()` and the deletes fit into 2 s where it matters.** At the slow endpoint 603 ms.
    On a link like wan-bad the writes on the wire can take longer than 2 s to be answered;
    the cleanup then gives up on that connection and deletes on a new one, which is slower
    but correct. No test interrupts an upload on wan-bad.
24. **A second signal that falls between the dropped command and the cleanup is dropped
    silently.** `notify_waiters` stores nothing, so the cleanup runs to its end and only a
    third signal ends it. That is the intended behaviour (finding 13); a user who presses
    Ctrl-C twice in the same instant waits for the cleanup, at most its 2 s and one reconnect.
25. **The cap of 100,000 directories without a file id** is a number chosen, not measured. A
    server without ids and with more directories than that in one walk gets a truncated walk,
    reported; no such server is at hand.

## 11. Adversary findings and what changed

One adversary reviewed the first version of this plan; the orchestrator validated the seventeen
findings against the plan text and the adversary's probes (`spikes/smb2/target/adv-stage2/`, not
committed). All seventeen are accepted. Nothing was re-measured for this revision; the numbers
below are the adversary's.

| # | Finding | Evidence | What changed |
|---|---|---|---|
| 1 | A leftover `.rsmb-<16 hex>.part` can itself be an item of a recursive transfer: uploaded or fetched under its own name, then written over by the transfer of the file whose hash it carries, exit 0; with `-f` and both in flight, partial bytes can reach a final name | the plan's own 5.3 (`FileOverwriteIf` on the temporary name) and 5.4 (`create` truncates) | `is_temp_name`; a name of that form is never a source or a target: `invalid_name` for a single `get` or `put`, an item error in a recursive one (P16, 5.4, 5.5, 5.6, 5.9, 5.10); the rows in F24; `temp_names` |
| 2 | No test reached the rename or the commit with a target in the way, so a rename sent with replace, or `force` handed to `commit`, would pass the gate; "the local file changes while it is read" had no test | every check came before the transfer | S37 (the final name planted on the server during a `put`), S38 (planted locally during a `get`, without and with `-f`), S39 (the source appended to at the 8th WRITE) |
| 3 | X24 could not pass, and S2-1's "the next attempt writes over the leftover" was wrong after a silent link: the dead session keeps the temporary file open | probe: a new connection got `SHARING_VIOLATION` at 1.2 s, success only at 14.6 s | S2-1 corrected; the open's "in use" message (5.3); the row in 5.4's table and in the matrix; X24 expects success **or** "in use" at once and success after `wait_no_open`; S30 waits too; `docs/transfers.md` and `help all` say it |
| 4 | On a 16-credit server a dropped `FileWriter` followed by a delete loses the handle: the delete fails for want of credits, the writer's CLOSE is never sent, the file stays locked; a new connection deletes it; `abort()` then delete works | edge: `CreditStarvation` in 1 of 5 and 3 of 6 rounds; still `SHARING_VIOLATION` 2 s later; a new connection deleted in 7 ms (3 of 3); `abort()` then delete 10 of 10 | the writer lives in a slot the command's future does not own, and the cleanup ends it with `abort()`; a sharing violation or a credit failure in the cleanup means "new connection", like a lost one (5.2); D5 withdrawn; S2-3 rewritten; `cleanup_goes_to_a_new_connection`; X25 on edge, five rounds, in `serial` |
| 5 | A crash between the hard link and the unlink of a commit without replace leaves the temporary name as a second name of the final file; the next `get -f` truncates the user's complete file in place | 5.4 of the first version: no-truncate open, lock, inode check, `set_len(0)` | after the lock, more than one link → remove the temporary name and start again (5.4); a row in the matrix; the case in `local_temp_lifecycle` |
| 6 | `get -r -f` onto a local file system that folds case or normalises names loses files silently: the second replacing rename overwrites the first, the summary counts two | 5.4 of the first version: `commit(.., force)` | `commit(target, force && existed)`: only what was there when the transfer started is replaced (P17); two targets that are one existing local file are both reported (5.9 step 4); S38 and `same_local_file_twice`; the mirror on a normalising server is assumption 21 and a known limitation |
| 7 | `create_dir_all` and the commits follow a symbolic link inside the destination tree: files land outside the target and `-f` replaces files there | the plan claimed "nothing a server sends can name a path outside it" | local directories are made one component at a time after `symlink_metadata`; a link is an item error and its subtree is left out (P18, 5.9 step 3); `local_dirs_never_through_links`, F37 |
| 8 | `mkdir -p` over an existing file reporting success is avoidable | probe: CREATE with `FileOpenIf` and the directory flag + CLOSE: created / success for a directory / `NOT_A_DIRECTORY` for a file / `OBJECT_PATH_NOT_FOUND` | `Remote::mkdir(path, existing_ok)`; used by `mkdir -p` and by every directory of `put -r` (5.2, 5.10, 5.14, S2-8); assumption 9 gone; F36 |
| 9 | The reason for leaving out an interrupt test in the small-file phase was false by the plan's own numbers (1.6 s and 4.5 s), and sixteen deletes one after another exceed the 2 s of the cleanup | probe: a delete after an aborted small `put` in flight answered after 940 ms | the cleanup's deletes are in flight together (5.2); X26: SIGINT at the 40th CREATE of `put -r` and at the 40th READ of `get -r` at the slow endpoint; the paragraph "not in the gate" corrected |
| 10 | S27, S28 and S29 asserted times and "0 new connections" while running beside tests that stream 50 MiB over the same link | 6.4 of the first version | the three are X27, X28, X29 in `tests/serial.rs`; the rule is in 6.1; the budget in 6.2 |
| 11 | Three contradictions: `put a.txt docs` onto a directory had two different answers; an interrupt was said to print the summary although the command's future is dropped; an unreadable local subdirectory of `put -r` was not specified | 5.6 and F28 against P2 and 5.3; 5.9 against 5.11 | one wording for a failed rename, with a `stat` on the collision path that tells a directory from a file: `is_a_directory` and the hint, on both paths, with and without `-f` (5.3, 5.6, 5.7, F28, `rename_errors_have_one_wording`); no summary after an interrupt (5.9, 5.11, X26); an unreadable directory is an item error and is not created on the server (5.10, F34) |
| 12 | A rewrite of the same size during a `get` is not detected; a replacing rename onto a target another client has open answers `ACCESS_DENIED`, and the message misled; `put -f` of a large file onto a directory uploaded everything first | the crate opens with full sharing; the first version checked the target only without `-f` | documented in 5.8, `docs/transfers.md` and the limitations (the change-time comparison is not done: a round trip per file, not measured for reliability); the message for `ACCESS_DENIED` on a replacing rename (5.3); the stream path checks the target always (P2, D3, F23) |
| 13 | The `spawn_blocking` closures of `get -r` outlive Ctrl-C and are killed by `exit`, leaving unnamed temporary files; relative local paths resolve against a later `lcd`; a second SIGINT while `BUSY` is false exits without the cleanup | 5.3 and 5.11 of the first version | the write closure holds an `Alive`, so the cleanup waits for it (5.2 step 1); local paths are absolute from the start of the command (5.3, 5.5); `BUSY` is held from before the command until after the cleanup (5.11); the double-signal rounds of X27 and X28; assumptions 16 and 24 |
| 14 | `Tree::download` on a directory succeeds with size 0; `read_file_compound` on a directory fails with `STATUS_INVALID_DEVICE_REQUEST` (kind `Unsupported`), not `is_a_directory` | probe | the check on the open's answer stays and F35 asserts that no empty local file appears; the READ's status is mapped to `is_a_directory` in `read_small` (5.2, 5.9) |
| 15 | F36 at the fast endpoint rarely overlaps its two processes; S35 depended on an order the plan did not fix | 50 MiB takes about 30 ms there | the two-process test is S36 at the slow endpoint, the second process started on an observed event; the file list is sorted by path (P20) and S35 says why its file is in the last wave |
| 16 | Without file ids the depth cap bounds depth, not breadth (two links at every level: about 2^100 listings); the local walk of `put -r` has no loop check | 5.9 of the first version | `NO_ID_CAP`: a walk enters at most 100,000 directories without an id (5.1, 5.9, `walk_breadth_cap_without_ids`, assumption 25); the local walk records device and inode of every directory it enters (5.10, `local_walk_rules`) |
| 17 | `ulimit -f` in dash counts 512-byte blocks: 1,024 is 512 KiB, not 1 MiB | dash's manual; the write failed at 524,288 bytes | F33 starts its child with bash and says so |

The adversary's check of the assumptions of section 10: 2, 3, 5, 15 and 17 hold; 1 is wrong on
edge (finding 4); 9 was avoidable (finding 8); 10 reproduces. For 10 the decision is recorded
there: a documented limitation, with the one real fix named and left to the orchestrator.

Changed beyond the findings:

- The stream path of `put` now makes its target check also with `-f`, so `put -f` of a large
  file costs 14 requests, not 12 (F23).
- A refused small `put` costs 12 requests, not 10 (the `stat` that words the error).
- A target that appears during a `get` is not replaced even with `-f` (P17). The first version
  replaced it.
- D5 is withdrawn: on this point the plan follows the master plan again.
- Two more unit tests for decisions that were prose before (`cleanup_goes_to_a_new_connection`,
  `rename_errors_have_one_wording`), and `local_walk_rules`.
- Assumptions 21 to 25 are new.
- The user's decision of 2026-11-18 is recorded in 5.7: no extra confirmation for `-f` on a
  recursive transfer, no prompt for `rm`. Nothing in the plan had to change for it.

Budget and order: `tests/serial` grows from an expected 175 s to about 225 s (X25 to X29),
`tests/slow` loses three tests and gains four that assert no time; the full run is expected at
about 365 s of the 420 s allowed, `fast` unchanged at about 62 s of 120 s. The task order is
unchanged. T2 gains the link-count check, the `existed` rule and S38; T3 the writer slot, the
new cleanup, the always-made check, the one wording and S36, S37, S39; T5 the `BUSY` change and
X25, X27 to X29; T7 now also needs T5 (its interrupt test X26 needs the cleanup) and gains the
link-safe directories and F37; T8 gains the open-or-create `mkdir`, the local walk rules and
the second half of X26.

## Deviations during implementation

Recorded by the implementing agent, 2026-11-18. Each is the smallest change that keeps the plan's
intent where the plan met the real crate or server, with the evidence. Nothing else departs.

| # | What | Why | Evidence |
|---|---|---|---|
| I1 | `mkdir -p` opens or creates the components **below the part of the path it shares with the working directory**, not every component from the share root | F23 and F36 count 6 requests in 3 flights for `mkdir -p a/b/c` typed in a scratch directory two levels below the root; from the root it costs 10 in 5. The working directory and its ancestors were checked by `cd` (a missing one is still `not_found`). The table's `details` and `docs/commands.md` say so | first run of `mkdir_rules`: 10 requests in 5 flights |
| I2 | The progress line shows rates in B/s, KiB/s and MiB/s, never GiB/s (sizes still go up to GiB) | I23 asserts `MiB/s`; at the fast endpoint 50 MiB arrive at more than 1 GiB/s | first run of `progress_on_terminal_only`: `1.1 GiB/s` |
| I3 | F23 checks `cat /sizes/1.bin` for its 7 requests and adds `cat /tree/d0/file.txt` (7 requests, 3 flights, success); the session script of X1, X2 and F14 uses `cat /tree/d0/file.txt` | `sizes/1.bin` is one byte that is not UTF-8, so with `--json` `cat` reads it and then refuses it (S2-4), and the script's lines must succeed | first runs of `transfer_costs`, `counts_add_up`: `not UTF-8 text; use get` |
| I4 | A small `put` refused because the target exists costs **13 requests in 6 flights**, not 12 | the crate's `rename_with` closes the handle with a standalone CLOSE when the server refused the SET_INFO (the compound's CLOSE was cascaded): `smb2` 0.43.0, `src/client/tree.rs`, "Issue standalone CLOSE to avoid leaking the handle". F23 asserts 13 / 6; `docs/transfers.md` and `docs/testing.md` say why | first run of `transfer_costs`: 13 requests, a lone CLOSE after the rename compound |
| I5 | F14 compares requests over the whole extended script, and flights over the script without its two recursive transfers | with 16 files in flight, how many sends follow an answer depends on the order the answers arrive in; two runs of the same script differ in flights, never in requests | first run: requests 1085 = 1085, flights 266 against 273 |
| I6 | S32 checks on edge that the client **never spends a credit it was not granted** (the credit balance over the whole connection, start-up included, from its first credit, never below 0), and prints the peak outstanding, instead of asserting peak outstanding ≤ 16 | Samba with `smb2 max credits = 16` grants a few credits more while 16 are outstanding; the client used them legitimately. The plan's intent (the client stays within the server's credits, no stall) is kept and checked more strictly | peaks of 17 to 19 in `get`, `put` and `get -r /sizes`, one READ answer granting 3 credits while 16 were outstanding; lowest balance 0 in every run |
| I7 | `login` sets the connection's **send timeout to max(`--timeout`, 15 s)** (the crate's default is 20 s) | a frame being written when the link goes silent keeps blocking in a full TCP window until the send timeout, also after the receive side has declared the server silent, so an upload failed later than any other command (X24 asserts 13 to 20 s). With the default `--timeout 30` the new value is also more tolerant (17 KiB/s per 512 KiB frame instead of 26 KiB/s), matching the 23 KiB/s of 5.12 | X24 failed at 20.4 and 20.6 s; afterwards 15.0, 15.5, 15.6 s and the full gate |
| I8 | `finish` marks a connection that a lost-connection error showed gone **before** its cleanup | otherwise the cleanup's step 2 spends its 2 s on a link known to be silent (the crate does not flag a silent connection as disconnected), which also pushed X24 past 20 s; step 2 already skips a disconnected connection, so this only makes the error's verdict count | X24 at 20.4 s before |
| I9 | `ensure_connected` increments the generation but does **not** clear `in_flight`; the cleanup sets and clears `in_flight` around its own deletes on the new connection | Stage 1's sending methods set `in_flight` before calling `tree()`; clearing it in a reconnect would let an interrupt right after a reconnect keep a connection with requests on the wire. What the plan's rule protects (the cleanup's new connection is not killed by `abandon`) holds | code reading (`cd`, `ls`, `use_share` set `in_flight` first) |
| I10 | `run_parsed` skips `mark_dead` for a lost connection only when **the failed command's own cleanup** made the new connection (`replaced_by_cleanup_since`), not after any reconnect | a command that starts on a dead connection reconnects in `remote()` (the generation changes); if the new connection is then lost, it must still be marked dead | code reading |
| I11 | `crate_path(components)` takes no tree | the crate adds the DFS prefix itself; the tree was not needed | — |
| I12 | `get` onto an existing local **directory** is `is_a_directory` before any request, with or without `-f`; the check in `commit` stays as a backstop | a directory is never a file target (5.7, last row); without `-f` the plan's wording would have given `already_exists` | `overwrite_rules` |
| I13 | `Flow::Confirm(message)`: on "no" the shell prints the `already_exists` error and the `done` line itself | `run_line` printed nothing for the first attempt | `overwrite_prompt` |
| I14 | `walk`'s `max_depth`: a directory at depth `max_depth` or deeper is not listed (`max_depth` 1 lists the start only, 2 the start and its subdirectories) | the plan left the boundary to the test (`walk_max_depth`); this makes `find -maxdepth N` a direct mapping in Stage 3 | `walk_max_depth` |
| I15 | X24 and the retries of X28 use `put -f` | after a retry that succeeded, "the same `put`" without `-f` would be `already_exists`; the first `put` of X24 has no target, so `-f` changes nothing there | — |
| I16 | `bench-rsmb.sh transfers` takes its times from the product's own `seconds` field (first request to last answer), not the wall time of the process, and runs about 21 minutes | start-up is not part of a transfer's rate | the bench run in `docs/performance.md` (1,281 s) |
| I17 | The harness has a few helpers beyond 6.1 (`ready`, `docker_sh`, `make_file`, `transfers`, `item_errors`, `Remove`, `Client::take_tap`, `credit_balance`); `Scratch::sh` runs as tester | small pieces the tests share; files planted by a test belong to the same user as files Samba writes | — |
| I18 | Unit tests that need a directory use `target/unit-tmp/<pid>-<name>` | the tests write only below the build's own directories | — |
| I19 | After the review: `cleanup` empties the registry (`temps_left` became `take_left`, which drains it); a leftover is named once and not retried | a name that stayed registered made every later command rerun the cleanup, get a sharing violation and replace the connection | `leftover_temp_is_reported_once`; `docs/reviews.md` |
| I20 | After the review: the temporary file is created with `FileCreate` (`write_file_compound_exclusive`, `create_file_writer_exclusive`); a collision removes the object itself (`FILE_OPEN_REPARSE_POINT`) and repeats once. Replaces "`FileOverwriteIf`: a stale one is written over" of 5.2 | `FileOverwriteIf` wrote through a link at the temporary name; Samba's plain delete deletes a link's target. A dangling link is not caught (documented) | `link_at_the_temporary_name`; `docs/reviews.md` |

Measured while implementing, not deviations: X21 `get -r d01` 1,689 ms (bound 2,402) and `put -r
d01` 4,523 ms (bound 6,606) at the slow endpoint; X22 `medium.bin` 10.8 MB/s down and 10.0 MB/s up
there (iperf3: about 20 and 17); after a silent link the server kept the leftover "in use" for the
retry at once in every X24 run, and let go of it within the 60 s `wait_no_open` allows; X25: in 5 of
5 rounds the abort and the delete went on the first connection.
