# Stage 3 plan: `tree` and `find`

> Written on 2026-11-18 by a planning agent that read the repository and nothing else, before any
> Stage 3 code exists. It follows [`master-plan.md`](master-plan.md) (sections 3–6, 9, 11–14) and
> the decisions the user took for this stage (section 3); where it departs from the master plan,
> section 4 says where and why. It is written for one implementing agent who was not present:
> everything needed to build Stage 3 in one pass is here or in a document this one names.
> **Revised on 2026-11-18 after the adversary's review:** section 11 lists the eight findings,
> the evidence for each and what changed; the text above it is the revised plan. Section 10
> lists what could not be verified.
>
> Words are used as in `docs/measuring.md` (request, flight, outstanding) and in the master plan
> (fast, slow, edge, wan-bad). "The crate" is `smb2` 0.43.0. A **walk** is one run of
> `walk::walk`; a **listing** is one `Remote::list` (4 requests in 1 flight on Samba). The
> **start** is the directory a walk begins at; it has depth 0 and its entries have depth 1.

## 1. Scope, what is not in it, exit criteria

**In scope** (brief, Stage 3; master plan section 9): `tree [-L <n>] [<dir>]` and
`find [<dir>] [-name <glob>] [-iname <glob>] [-type f|d] [-size [+|-]<n>[BKMGTP]] [-maxdepth <n>]`;
both walk with at most 16 listings in flight on the one connection, go on past directories that
deny access and report those at the end; link loops end and say why; `--json` gives one object
per line; the listings a walk fetched feed the cache that Tab reads; `help all` and
`docs/commands.md` complete with examples; flags that take a value in the command layer; one
new fixture, `links/` (6.1).

**Not in Stage 3:** `-mindepth` (P9), other predicates (`-mtime`, `-newer`, `-path`, `-empty`,
`-exec`, `-delete`, `-ls`), negation and `-o`, several start directories, a file as start,
content search, sorting options, character classes such as `[[:digit:]]`, a flag for the width,
following or not following links as an option, `tree -d` and other `tree` options, completion of
flag names, a walk that reads the cache instead of the server, resuming a walk on a new
connection, a larger buffer for one very large directory inside a walk (2.1).

**Exit criteria** (master plan section 9): every row of section 6.4 passes in `./test.sh`; the
full run is within 9 minutes of counted time (540 s) and `./test.sh fast` within 2; the walk
measurements of 6.5 are recorded in `docs/performance.md`; the documents of section 8 match the
code; the review's confirmed findings are fixed and their tests are in the suite.

## 2. What was measured while planning

All on 2026-11-18, Samba 4.23.8, read-only, under the shared lock, with a throwaway program
under `spikes/smb2/target/scratch-stage3/` (git-ignored, not committed). It compiles the
product's own `src/session.rs` (so every listing is the product's `Remote::list` through
`Session::remote` / `finish`) with a copy of `src/walk.rs` that has two switches: results taken
in the order the listings were queued, and a stop from `on_dir`. Counts through `smbtap`, times
direct to the endpoint, a new connection for every run. netem dropped nothing (`ctl.sh drops`
0 before and after); the connect time at the slow endpoint was 200.2 ms.

### 2.1 What a walk costs

`exp-stage3 addr=<endpoint> path=<dir> [tap]`. Requests are 4 per listing (CREATE,
QUERY_DIRECTORY with 1 MiB, the 64 KiB probe, CLOSE in one compound) unless a directory holds
more than 1 MiB of entries.

| Walk | Endpoint | Listings | Requests / flights | Peak outstanding | Time |
|---|---|---|---|---|---|
| `tree/` | fast, tap | 341, 1 denied (`tree/forbidden`) | 1,368 / 326 | 64 | 86.4 ms |
| `fanout/` | fast, tap | 2,001 | 8,004 / 1,986 | 64 | 475.0 ms |
| `chain/` (60 levels) | fast, tap | 61 | 244 / **61** | 4 | 21.4 ms |
| `deep/` (12 levels) | slow, tap | 13 | 52 / **13** | 4 | 2,610.6 ms |
| `loops/` | fast, tap | 1 | 4 / 1 | 4 | 1.2 ms |
| `cjk/` | fast, tap | 20 | 80 / 11 | 40 | 18.1 ms |
| `special/` | fast, tap | 5 | 20 / 5 | 4 | 3.2 ms |
| `many/` (10,000 entries) | fast, tap | 1 | 9 / 5 (CREATE 2, QUERY_DIRECTORY 5, CLOSE 2) | 4 | 197.5 ms |
| `huge/` (100,000 entries) | fast, tap | 1 | 19 / 15 (CREATE 2, QUERY_DIRECTORY 15, CLOSE 2) | 4 | 1,034.0 ms |
| the whole share `/` | fast, tap | 2,471, 3 denied, 2 links | 9,921 / 2,356 | 64 | 1,771.1 ms |
| `/` with a depth limit of 3 | fast, tap | 2,053, 3 denied, 2 links | 8,249 / 1,955 | 64 | 1,654.0 ms |
| `/` with a depth limit of 2 | fast, tap | 16, 2 denied, no link reported | 97 / 24 | 64 | 1,210.8 ms |

| Walk | Slow endpoint, direct | Beside |
|---|---|---|
| `tree/` | 4,822.3 / 4,826.5 / 4,826.8 ms | 23 waves × 200.2 ms = 4.6 s; one after another 342 × 0.2 = 68.5 s; Stage 0 through the crate 18.5 s |
| `tree/d0` (85 listings) | 1,615.2 ms | 7 waves = 1.4 s |
| `fanout/` | 26,316.1 / 26,321.8 ms | 126 waves = 25.2 s; the first listing (2,000 entries, new connection) took 1,022 ms; one after another 400 s |
| `chain/` | 12,255.6 ms | 61 listings one after another, 200.9 ms each |
| `deep/` | 2,610.6 ms | 13 × 200.8 ms |
| `huge/` | 5,285.1 / 5,292.0 ms | 15 flights; `ls /huge` is 1 flight, 2.3 s warm and about 4 s on a new connection (`docs/performance.md`) |
| `many/` | 2,395.7 ms | 5 flights; `ls /many` 1 flight |
| the whole share `/` | 43,529.1 / 43,600.5 ms | 2,471 listings |

On `samba-edge` (16 credits, 64 KiB), through the tap: `tree/` 1,368 requests, peak outstanding
16, 85.6 ms; `fanout/` 8,011 requests (the 2,000-entry root needs the listing's second phase:
7 more), peak 16, 484.9 ms; `many/` 26 requests in 22 flights; `cjk/` 86 requests; the whole
share 10,140 requests, peak 16, 1,632.7 ms, with the same 2,471 listings and 119,874 entries as
on `samba`. Nothing stalled.

So f = 1 (master plan section 9), a walk is bounded by its waves, and a directory above 1 MiB of
entries costs one flight per MiB inside a walk (the walk's buffer is 1 MiB so that 16 listings
fit a new connection's 513 credits, Stage 2 S2-5). That is 1 s more than `ls` for `huge/` at the
slow endpoint; it is not changed here.

### 2.2 The walk sees what the server has

The whole-share walk was compared with `find` run in the container (`docker exec rsmb-samba sh
-c 'cd /srv/share && find . -mindepth 1 -print0'`), as sorted lists of paths: 119,874 entries
from the walk, 119,881 on disk. The difference is exactly what the fixtures predict: the three
files inside the three directories that deny listing (`forbidden/secret.txt`,
`traverse-only/known.txt`, `tree/forbidden/secret.txt`), `loops/dangling` (Samba does not list a
dangling link; `loops/` shows `file.txt`, `self`, `up`), and `mangled/` (12 on disk, 9 mangled
names over SMB). The directories listed (2,471) are the server's 2,474 minus the three denied.
`loops/self` was reported as the same directory as `/loops` and `loops/up` as the same as `/`
(walking `/loops` itself: `self` → `/loops`, `up` → the parent of the start).

`upload/` is on the same file system as the rest of the share here (`stat -c %d`: device 55 for
both), so two different directories with one file id cannot be produced in this environment
(section 10, assumption 2).

### 2.3 Taking results in queue order instead of completion order

A walk that takes listings in the order they were queued (a window of 16 sequence numbers)
prints the same lines in the same order on every run. Measured at the slow endpoint, direct,
alternating with the plain walk:

| Walk | Completion order | Queue order |
|---|---|---|
| `tree/` | 4,826.8 / 4,822.3 / 4,826.5 ms | 4,825.0 / 4,827.2 / 4,820.4 ms |
| `fanout/` | 26,316.1 / 26,321.8 ms | 26,315.7 / 26,330.7 ms |
| the whole share | 43,600.5 / 43,529.1 ms | 46,759.7 / 46,745.1 ms (+7.3 %) |

It costs nothing where listings take equal time and 3.2 s on the whole share, where the walk
waits behind `huge/` (15 flights). The first version of this plan did not build it; after the
review it is how the walk works (finding 1, 5.3, and the measurements of 2.10).

### 2.4 Stopping in the middle

| Case | Measured |
|---|---|
| the product, `get -r /fanout` at the slow endpoint, SIGINT 4 s into its walk (26 s long) | exit 130, 1 ms after the signal; `error` of kind `interrupted`, then `done` with `ok` false; no local file. SIGTERM: the same, 0 ms |
| the probe, `fanout/` at the slow endpoint through the tap, every connection cut at the 600th QUERY_DIRECTORY | the walk returned `connection lost: Disconnected from server` (closed, not silent) after 4,637.9 ms with 286 directories listed; a second walk on the same `Session` (`remote()` reconnects) listed all 2,001 in 27,136.2 ms; the tap saw 2 connections in all |
| the same at the fast endpoint, cut at the 1,500th | lost after 192.4 ms with 738 listed; the second walk 2,001 in 478.2 ms; 2 connections |

### 2.5 Memory

| What | Measured |
|---|---|
| the walk of the whole share, nothing kept (what `find` does): 119,874 entries, one directory of 100,000 | RSS 20.5 MB after, peak (VmHWM) 26.4 MB |
| the same with every listing kept as `Arc<Vec<Entry>>` by path (what `tree` does) | RSS 25.7 MB after, peak 25.7 MB; drawing all 119,877 lines took 8.0 ms |
| `huge/` alone | peak 25.2 MB streaming, 23.7 MB kept |
| `fanout/` (2,001 listings) | RSS 8.8 MB |
| synthetic, no server: 500,000 entries kept in that structure | 44.3 MB (12-byte names, 100 a directory), 67.7 MB (40-byte names), 43.3 MB (5,000 a directory), **166.8 MB when every directory holds one entry** |
| synthetic: 1,000,000 entries | 88.5 MB (12-byte names, 100 a directory), 135.4 MB (40-byte names), 109.0 MB (10 a directory) |

About 90 to 140 bytes an entry and about 230 bytes a directory.

### 2.6 The cache at its cap

`Cache::put` finds the listing to evict with a linear search (its own comment names the
ceiling). With listings of one entry each, the product's `Cache`, release build: 20,000 puts at
a cap of 10,000: 104.5 ms; 40,000 at 20,000: 437.0 ms; 80,000 at 40,000: 1,636.7 ms. Four times
the time for twice the size: at the real cap of 500,000 the same pattern is about four minutes.
`ls` and Tab never produce it; a walk that stores its listings would (5.4).

### 2.7 The glob matcher against the server's `find`

The containers are Alpine: `find` is BusyBox on musl. It has `-name`, `-iname`, `-type`,
`-maxdepth`, `-mindepth`, `-print0` and `-size N[bck]` (no `-printf`, no `M` or `G`), and its
matcher takes names as UTF-8 without any locale variable (`find cjk -name '??'` gave 2 with and
without `LC_ALL=C.UTF-8`). Run as root it sees inside the denied directories; run as `tester`
(`docker exec -u tester`) it sees what the SMB user sees (`find tree -mindepth 1`: 683 as root,
682 as tester).

A prototype of the matcher of 5.6 walked `special/` and `cjk/` and its result set was compared
with `find <dir> -mindepth 1 -name|-iname <pattern>` in the container, as sorted path lists.
**All 60 patterns gave the same set:**

| Fixture, flag | Pattern → matches |
|---|---|
| `special/`, `-name` | `*.txt` 21 · `[[]abc[]].txt` 1 · `\[abc\].txt` 1 · `{a,b}.txt` 1 · `-*` 2 · `--help*` 1 · `*space*` 3 · `?ase.txt` 2 · `[Cc]ase.txt` 2 · `[!C]ase.txt` 1 · `caf?.txt` 1 (NFC) · `caf??.txt` 1 (NFD) · `*😀*` 2 · `? smile.txt` 1 · `Gr??e.txt` 1 · `it's.txt` 1 · `*%*` 1 · `*[;&]*` 2 · `#*` 1 · `$*` 1 · ``*`*`` 1 · two spaces then `*` 1 · `*` 26 · `[` 0 · `*[*` 1 · `[a-c]*` 5 · `leaf.tx[s-u]` 1 · `*.TXT` 0 |
| `special/`, `-iname` | `CASE.TXT` 2 · `GRÜSSE.TXT` 0 · `GRÜßE.TXT` 1 · `привет.txt` 1 · `CAFÉ.TXT` 1 (NFC only) · `*.TXT` 21 · `[A-C]ASE.txt` 2 · `DIR WITH SPACE` 1 · `😀*` 2 |
| `cjk/`, `-name` | `简体中文*` 115 · `*中文` 4 · `한글` 2 · `?글*` 114 · `??` 2 · `*_0999.txt` 1 · `*_00[0-4]?.txt` 50 · `ＦＵＬＬ*` 114 · `東京　大阪*` 114 · `𠮷*` 114 · `?野家*` 114 · `[简繁]*` 229 · `[一-龥]*` 343 · `*.txt` 1,010 · `leaf.txt` 1 · `パン*` 0 (the fixture is NFD) · `???` 4 · `か*` 114 · `が*` 0 |
| `cjk/`, `-iname` | `ｆｕｌｌ*` 114 · `简体中文*` 115 · `LEAF.TXT` 1 · `[ｆ]*` 114 |

### 2.8 Fixture counts, taken on the server as `tester`

`docker exec -u tester rsmb-samba sh -c 'cd /srv/share && find <dir> -mindepth 1 <predicate> | wc -l'`:

| Fixture | Counts |
|---|---|
| `tree/` | all 682 (341 directories, 341 files) · `-maxdepth 1` 6 · `-maxdepth 2` 26 · `-maxdepth 3` 106 · `-name file.txt` 340 · `-maxdepth 3 -type f -name file.txt` 20 |
| `fanout/` | 4,000 (2,000 directories) |
| `cjk/` | 1,029 (19 directories) · `-maxdepth 1` 1,010 |
| `special/` | 26 (4 directories) |
| `chain/`, `deep/` | 61, 13 |
| `sizes/`, `-type f -size` | `+65536c` 7 · `-65536c` 3 · `65536c` 1 · `+1048576c` 4 · `-1048576c` 6 · `1048576c` 1 · `+0c` 10 · `0c` 1 |
| `large/`, `-type f -size` | `+1073741824c` 2 · `-1073741824c` 1 · `+4294967296c` 1 |
| `many/` | `-name 'f0999?'` 10 |
| the share root | 17 directories at depth 1, two of which deny listing |

### 2.9 smbclient-ng

Through the tap at the fast endpoint, one process per run, the method of `docs/measuring.md`
(a run with `use share` and an empty line alone: 19 flights):

| Run | Flights above the empty run | Per directory |
|---|---|---|
| `tree tree/d0/d0` (21 directories) | 86 | 4.1, one after another (peak outstanding 1) |
| `tree tree/d0` (85 directories) | 342 | 4.02 |
| `find tree/d0/d0` | 86 | the same walk |

At the slow endpoint, direct: `tree tree/d0` took **69.3 s** above the empty run (74,289 ms
against 4,998 ms); the walk of the same 85 directories through the product's listing took
1.6 s. On all of `tree/` smbclient-ng prints `forbidden/`, then an `[error]` line with
`STATUS_ACCESS_DENIED`, and goes on; `bench-smbclientng.sh` treats an `[error]` line as a failed
run, so the fair row is `tree/d0`, which has no denied directory (6.5).

### 2.10 Measured after the review

With the same probe, its matcher changed to the rule of finding 3 and a mode that prints paths
to stdout and stops at a write error. The files were made with `docker exec` below
`upload/rsmb-plan3` on `samba` and removed afterwards (`upload/` empty again).

| What | Measured |
|---|---|
| a link to a sibling: `links/a/to-c -> ../b/c`, `links/b/c/file.txt` (inodes 67540714 for the link, 67540667 for `c`; over SMB both carry one file id) | **completion order**, 10 walks: 7 reported `links/b/c` as the repeat and listed `a/to-c`, 3 the other way round. **Queue order**, 10 walks: all 10 listed `a/to-c` and reported `b/c` as already listed; the output was `a`, `b`, `a/to-c`, `b/c`, `a/to-c/file.txt` every time |
| the order of the printed paths, two or three runs each (md5 of the output) | completion order: different every time (`tree/` at the slow endpoint 2 of 2, `fanout/` at the fast endpoint 3 of 3). Queue order: identical (2 of 2, 3 of 3) |
| `fanout/` printed into `head -n 5`, the walk stopping at the write error, through the tap | **4 requests** of the 8,004, at the slow endpoint (3 of 3, the walk ended after 1,023 ms) and at the fast one (3 of 3): the 2,000 lines of the first listing are more than `head` reads before it exits |
| `huge/` printed into `head -n 5` | 19 requests, which is all of them: one directory, nothing to stop (why the first F48 proved nothing) |
| two directories that differ in case, `cs/Foo` (holding `x`) and `cs/foo` (holding `y`) | this Samba lists both and serves each its own content: `cs/Foo/x`, `cs/foo/y`. One folded cache key would hold only one of them |
| `-iname` under the rule "equal if the one-character lower-case forms are equal or the one-character upper-case forms are", against the server's `find` | the same for `INDEX.TXT` (finds `INDEX.txt` and `ındex.txt`), `Σ.txt` (finds `σ`, `Σ`, `ς`), `İSTANBUL.TXT`, `ISTANBUL.TXT`, `STRASSE.TXT` (nothing), `STRAßE.TXT`, `[A-Z].TXT`. **Different:** `istanbul.txt` (the server also finds `İstanbul.txt`; rsmb does not), `index.txt` and `ındex.txt` (rsmb finds both `INDEX.txt` and `ındex.txt` for either; the server only one), `ς.txt` and `σ.txt` (rsmb finds all three sigmas for either; the server one and two). rsmb's rule is symmetric, the server's is not |
| classes, `-name`, against the server's `find` | the same for `[a-z].txt`, `[]].txt`, `[!a-m].txt`. **Different:** `[\]].txt` (rsmb: `\` escapes inside a class, finds `].txt`; the server: nothing), `[z-a].txt` (rsmb: a reversed range holds nothing; the server finds `a.txt` and `z.txt`), `[[:lower:]].txt` (the server knows the class; rsmb refuses the pattern) |

## 3. Decisions

Taken with the user before this plan, binding:

| # | Decision |
|---|---|
| U1 | Depth counts as in Unix tools: `tree -L 1` and `find -maxdepth 1` mean the direct children |
| U2 | `find` has `-name` and `-iname` (glob), `-type f\|d`, `-size [+\|-]N[BKMGTP]`, `-maxdepth` |
| U3 | `--json` gives one object per line |
| U4 | A walk over `loops/` ends and says why |
| U5 | What a walk lists feeds the listing cache where that is sound |
| U6 | `help all` and `docs/commands.md` are complete with examples; the three uses hold |

On the open questions of master plan section 14:

| # | Question | Decision | Reason |
|---|---|---|---|
| S3-1 | Exit status of a walk that skipped denied directories | **0.** Denied directories, directories with the file id of one already listed and directories that vanished during the walk are reported at the end (`denied` and `skipped` objects, lines on stderr in text mode) and counted in the summary. **1** when the walk is incomplete for another reason: a directory failed with any other error, or the walk hit the cap for servers without file ids. The start's own error (`not_found`, `not_a_directory`, `access_denied`) is the command's error, exit 1 | The default. A share with a directory one may not list is the normal case, and the non-interactive runner stops at the first non-zero status. A program reads `walk_summary.denied`. This differs on purpose from `get -r` (anything not copied is exit 1): there something asked for did not arrive; here nothing that can be listed is missing. `docs/commands.md` states the two side by side |
| S3-2 | The glob matcher | Our own, about 80 lines in `src/walk.rs`, no dependency: `*`, `?`, `[…]` with ranges and negation, backslash escape, on Unicode scalar values (5.6) | Measured (2.7): a prototype agreed with the server's `find` on 60 patterns over `special/` and `cjk/` |
| S3-3 | Does `find` print the start; the path form | The start is **not** printed. Paths are printed **from the share root** (`/tree/d0/file.txt`), in text and in `--json`, whatever the argument and the working directory | The start is not an entry of any listing, so its line would have no size, time or id. The path form departs from the default (section 4, D2) |
| S3-4 | A walk that loses its connection | **`tree` runs again once** (it prints nothing before the walk has ended). **`find` runs again once if it has printed nothing yet** (the start's listing was lost, or nothing had matched); once it has printed, it fails with `connection_lost`, what it printed stays, no summary follows, and the error says the results are incomplete | Measured (2.4): the walk returns the error once the listings in flight have failed, and a second walk on the session completes on one new connection. A second run after output would print the first part twice (the rule that keeps `cat` from running again, Stage 2 D4); before any output there is nothing to repeat (finding 5) |

On points the master plan left to this plan:

| # | Point | Decision |
|---|---|---|
| P1 | `tree`: stream or collect | Collected, then printed in tree order (master plan). At most `TREE_MAX` = 500,000 entries: past that the walk stops and the command fails with `other`, naming `-L` and `find`, and prints no tree. Measured (2.5): 44 to 68 MB at the cap, 167 MB in the worst shape |
| P2 | `find`: stream or collect | Streamed: the matches of one directory are printed and flushed when its listing arrives (master plan). Nothing but the listings in flight or waiting for an earlier one (16 at most), the queue and the set of visited directories is held |
| P3 | Order of `find`'s output | **The same on every run** (finding 1): the walk takes its listings in the order they were queued (5.3), so `find` prints breadth first: the start's entries, then the entries of each of its directories in name order, and so on, each directory's entries by name (bytes). A directory's own line always comes before the lines of what is in it. Measured: 0 % on `tree/` and `fanout/`, 7 % on the whole share (2.3); identical output across runs (2.10) |
| P4 | Order of `tree`'s output | By name (bytes) within a directory, files and directories mixed, depth first. The same on every run, also which of two paths to one directory is drawn with its content: the one the breadth-first order meets first |
| P5 | `-size` | Bytes, exact, no rounding: `+N` more than N units, `-N` less than N units, `N` exactly N units; units `B` or `c` (bytes), `K`, `M`, `G`, `T`, `P` (powers of 1024, upper or lower case), none = bytes. A lower-case `b` is a usage error: in Unix `find` it means 512-byte blocks (finding 6). **Files only**: a directory never matches `-size` (5.7) |
| P6 | Case and Unicode in patterns | `-name` compares code points exactly; `-iname` takes two characters for equal when their one-character lower-case forms are equal or their one-character upper-case forms are (finding 3). No normalisation: an NFC pattern does not match an NFD name (5.6) |
| P7 | Depth 0 | `-maxdepth 0` and `-L 0` are usage errors (the start is not printed, so they could print nothing) |
| P8 | What a directory that could not be entered looks like | It is an entry like any other in `find` and `tree` (its parent's listing has it); the end of the walk says why it was not entered. `tree` in text mode also marks it on its line (`forbidden/ [access denied]`) |
| P9 | `-mindepth` | Not built. It is one comparison in the filter and one table line, and nobody asked for it; add it when someone does |
| P10 | Flags that take a value | The command layer learns them once (5.2): a table entry declares `-name <glob>`, and the next token is the value, whatever it is. A flag given twice is a usage error |
| P11 | What feeds the cache | Every listing a walk fetched, complete and sorted, through the ordinary `Cache::put`: the cache's cap evicts the oldest. No budget of its own (finding 8). Two directories of one walk that fold to one cache key are stored neither (finding 4). A walk never reads the cache |
| P12 | Progress | The line `find /x  listing: N directories` on stderr when stderr is a terminal and not with `--json`: `transfer::Progress::listing`, which exists. Cleared before every line that goes to stdout |
| P13 | A closed stdout (`find … \| head`) | The walk stops: no further listing is started, those in flight are awaited, the command ends (5.3). The run then exits 1 as for any closed stdout (Stage 1) |
| P14 | A directory that vanished between its parent's listing and its own (`not_found`) | `skipped` with reason `gone`, exit status unchanged. On a share in use this happens, and nothing that exists is missing from the result |
| P15 | Completion | The directory argument of both commands completes like `cd`'s. After a flag that takes a value, Tab offers nothing and sends nothing. Flag names are not completed (as for `get -r` today) |
| P16 | Where the code goes | Pure parts in `src/walk.rs` (matcher, size, filter, the drawing, the end report); the two commands and the flag parsing in `src/commands.rs`; the cache changes in `src/session.rs`. No new source file |

## 4. Departures from the master plan

| # | Master plan | Here | Why |
|---|---|---|---|
| D1 | 4.7: the cache is "not filled by `tree`, `find` or recursive transfers, so a walk over a whole share cannot grow it" | `tree` and `find` store what they list (5.4). Recursive transfers still store nothing | The user's decision U5. A walk's listing is the same data as `ls`'s (the same routine, complete, sorted), so storing it is sound; the cache's cap, which now also counts listings (D3), keeps a whole-share walk from growing it without bound |
| D2 | S3-3 default: paths "relative to the argument, as `find` does" | from the share root | Every other path rsmb prints is from the share root (errors, `transfer` lines, the `path` of `entry`); the reports at the end of the walk are too. One form in text and JSON, and every printed path works as an argument from any working directory |
| D3 | 4.7: the cap counts entries; eviction by a linear search | the cap counts one unit per entry **plus one per listing**; the oldest listing is found through an ordered index | Measured (2.6): eviction is quadratic in the number of cached listings, and a walk stores thousands of small ones. And a listing costs about 230 bytes even when it is empty (2.5), which a cap on entries alone does not see |
| D4 | section 9: `find` "prints matches as directories complete"; `tree` collects | unchanged, with a cap on what `tree` collects (P1) | The task of this plan asks for a memory bound; without a cap `tree` on a large share has none |
| D5 | 4.9: `find` is among the commands run again after a reconnect | `find` is run again only while it has printed nothing | S3-4 |
| D6 | section 9, row 3.9: tap cut "in the middle of `find` over `fanout/`, fast" | `tree/` at the slow endpoint, for `tree` and for `find` | At the fast endpoint the walk of `fanout/` takes 0.47 s and "the middle" is a race; at the slow endpoint `tree/` takes 4.8 s (Stage 2 D6, the same reason) |
| D7 | section 9, row 3.3: "peak outstanding ≤ 16 × the requests of one listing step" | ≤ 64 on `samba`; on edge the credit balance is asserted instead of a peak | One listing is one compound of 4 requests. On edge Samba grants a few credits beyond its 16 at moments (Stage 2 I6), so the test checks that the client never spends a credit it was not granted |
| D8 | 4.11, 5.19 of Stage 2: the JSON types | `entry` gains `depth` (from `tree` and `find` only); new `denied`, `skipped`, `walk_summary` | master plan section 9 asks for them; `summary` stays the transfers' object |
| D9 | Stage 2, 5.17: flags are tokens without a value | flags may take a value (5.2) | `-L 2`, `-name '*.txt'` |
| D10 | Stage 2, 5.9: `on_dir: &mut dyn FnMut(Dir)`; results taken as the listings complete; the start's parent always seeded | `on_dir` returns `bool`, `false` stops the walk; results are taken in the order the listings were queued; the parent's id is seeded only below the share root (5.3) | P1 (the cap) and P13 (a closed stdout) both need to end a walk without abandoning requests; finding 1 (which of two paths to one directory is listed must not depend on timing); finding 7 |

Nothing else departs. In particular: one connection, L = 16, the 1 MiB walk buffer, CLOSE inside
every listing's compound, no second timer, no dependency added, all use of `smb2` in
`src/session.rs`.

## 5. Design

### 5.1 Files, dependencies, constants

No new dependency; `Cargo.toml` is unchanged.

| File | Stage 3 adds |
|---|---|
| `src/walk.rs` | `on_dir` returns `bool`; results in queue order; the parent seeded only below the root; `Glob`, `parse_size`, `Filter`, `draw_tree`, `end_report`, `TREE_MAX`; their unit tests |
| `src/commands.rs` | `Args` and flags with a value in `split_args`; `CmdId::Tree`, `CmdId::Find`, their table entries and functions; `run_walk`; `entry_obj` (shared with `ls`); `Out::diag`, `Out::written`; the rerun rule and the tail for `find` in `run_parsed`; the JSON types; the topic "tree and find" |
| `src/session.rs` | `Cache`: units, the ordered index, `Cache::feed`; `Session::feed` |
| `src/transfer.rs` | `get_tree`'s `on_dir` returns `true`; `seconds` becomes `pub` |
| `src/shell.rs` | nothing but what `positional_index` now returns (5.2) |
| `tests/common/mod.rs`, the three test files, `test.sh`, `testenv/bench-rsmb.sh` | section 6 |
| `testenv/samba/fixtures.sh`, `testenv/check.sh` | the fixture `links/` and its check (6.1) |

Constants, each in one place:

| Name | Value | Where | Meaning |
|---|---|---|---|
| `TREE_MAX` | 500,000 | `walk.rs` | entries `tree` collects before it gives up |
| `L` | 16 | `walk.rs` | unchanged in value; now also the window of the queue order: listings in flight plus results waiting for an earlier one |
| `DEPTH_CAP`, `NO_ID_CAP`, `WALK_BUFFER`, `CACHE_CAP` | 100, 100,000, 1 MiB, 500,000 | unchanged | |

### 5.2 Flags that take a value

The table keeps its type, `flags: &[(&str, &str)]`. A declaration with a space takes a value:
`("-L <n>", "…")`, `("-name <glob>", "…")`. The flag's name is the text before the space.

```rust
/// What follows the command name.
pub struct Args {
    pub flags: Vec<String>,            // flags without a value, as now ("-r", "-f", "-p")
    pub opts: Vec<(String, String)>,   // flags with a value: ("-name", "*.txt")
    pub pos: Vec<String>,
    pub open: Option<String>,          // the last token is a flag that still wants its value
}
pub fn split_args(cmd: &Cmd, tokens: &[String]) -> Args
pub fn opt<'a>(a: &'a Args, name: &str) -> Option<&'a str>
```

- Before `--`, a token equal to a declared flag name is that flag; anything else is a
  positional (unchanged). After a flag that takes a value, **the next token is its value
  whatever it is**: `find -name -rf` looks for the name `-rf`, `find -name --` for `--`.
- `parse` turns `open` into a usage error (`-name needs a value; usage: <syntax>`) and a flag
  with a value given twice into one (`-name given twice`).
- `positional_index` returns `None` when `open` is set, so Tab after `find -name ` offers
  nothing and sends nothing; otherwise `Some(pos.len())` as now.
- `run_line`, `run_parsed` and `dispatch` pass `&Args`. `transfer::get` and `put` keep their
  signatures and get `&a.flags, &a.pos`. `run_line`'s `force` pushes `-f` onto `flags` as now.
- Flags and positionals may come in any order: `find /tree -name x` and `find -name x /tree`
  are the same. A directory called `-name` is `find -- -name`.
- `command_text` and `command_obj` print the declaration as it is (`-L <n>`).

### 5.3 The walk core: three changes

```rust
pub async fn walk<F, Fut>(list: F, start: &[String], max_depth: Option<usize>,
                          enter: &dyn Fn(&[String], &Entry) -> bool,
                          on_dir: &mut dyn FnMut(Dir) -> bool) -> Result<Report>
```

**1. A stop.** `on_dir` returns whether the walk goes on. After `false`: nothing more is
spawned, the listings in flight are awaited and their results dropped (no request is abandoned
on a connection that lives, as after a fatal error), and `walk` returns `Ok(report)` with what
it had; the caller knows that it stopped. `get_tree` returns `true`. `Dir::depth` loses its
`#[allow(dead_code)]`.

**2. Results in queue order** (finding 1). Every listing gets a sequence number when it is
spawned. A result that arrives before an earlier one waits (`BTreeMap<u64, …>`); results are
handed to `State::take` strictly by number. A listing is spawned only while
`spawned − taken < L`, so listings in flight plus results waiting are never more than 16 (the
unit test `walk_width` keeps its bound). Everything that depends on the order then follows the
order of a breadth-first walk done one listing at a time, whatever the network does: which
directories are queued and in which order, which of two paths to one file id is listed (the
first in that order) and which is reported, the calls of `on_dir`, the vectors of `Report`.
Same tree, same result.

- The probe's form of the loop (2.3, 2.10):

  ```rust
  while fatal.is_none() && go && spawned - taken < L as u64 { /* pop the queue, spawn with seq = spawned, spawned += 1 */ }
  let Some(joined) = set.join_next().await else { break };
  ready.insert(seq, (path, depth, result));
  while let Some((p, d, r)) = ready.remove(&taken) { taken += 1; /* the match of today's loop */ }
  ```
- The price is waiting behind a slow listing: once the 15 listings queued after it have
  returned, nothing new starts until it has. Measured: nothing on `tree/` and `fanout/`, 3.2 s
  (7.3 %) on the whole share behind `huge/` (2.3). `get -r` walks the same way now; its counts
  do not change (the same listings, the same 4 requests each).
- After a fatal error or a stop nothing is taken any more; results still waiting are dropped
  with those in flight.

**3. The parent is seeded only below the share root** (finding 7): `if first.parent != 0 &&
!start.is_empty()`. Samba gives the root's `..` the id 0; a server that gives it a real id
would otherwise put an id into the set whose path (`start[..start.len() - 1]`) does not exist.

Everything else stays: the start listed first and alone, its `.` id seeding the set of visited
ids, `links` for an id seen before, `capped` for directories without an id past depth 100 or
past 100,000 of them, `denied`, `failed`, a lost connection or an interrupt as a fatal error.
`max_depth` N means a directory at depth N or deeper is not listed (Stage 2 I14), so
`-maxdepth N` and `-L N` are passed as they are.

**What a repeated file id is called.** A directory whose id was seen is "already listed as
`<path>`". The plan does not call it a link: with `links/a/to-c -> ../b/c` the walk lists
`a/to-c` (the first in order) and it is the real directory `b/c` that is reported (2.10). The
content is printed once, under the first path. `Report::links` keeps its name and holds `None`
for "the start's parent"; both commands print that as the path `start[..start.len() - 1]`,
which exists whenever the parent was seeded.

A link that leads **out of** the start's subtree to a directory that is neither the start nor
its parent is entered: Samba presents it as a directory, its id was never seen, and the walk
then covers that directory too, each directory once (as `find -L` would). It ends; it is a
documented limitation, not a loop.

### 5.4 The cache: what a walk stores

```rust
impl Cache {
    /// A walk's listing. `fed`: the folded keys this walk has stored (true) or has given up (false).
    pub fn feed(&mut self, fed: &mut HashMap<(String, String), bool>, share: &str, dir: &[String],
                entries: Arc<Vec<Entry>>)
}
impl Session { pub fn feed(&mut self, fed: &mut HashMap<(String, String), bool>, dir: &[String], entries: Arc<Vec<Entry>>) }
```

- **What is stored.** In `run_walk`'s `on_dir`, each listed directory: its entries without `.`
  and `..`, sorted by name, as `Arc<Vec<Entry>>`: exactly what `Session::list` stores for `ls`.
  `tree` keeps the same `Arc` for its drawing, so storing costs it no memory. Every listing is
  offered; there is no budget per walk (finding 8). At the cap the cache drops its oldest
  listings, so after a walk larger than the cap it holds what the walk listed last.
- **Why it is sound.** `Remote::list` and `ls` call the same `list_dir` with the same
  information class, and both return the whole directory (the walk with a 1 MiB buffer and the
  second phase for larger directories; 2.2 compared the result with the server). Filters and
  the depth limit decide what is printed and what is entered, never what a listing holds. A
  listing stored by a walk that was interrupted or lost its connection later is still a
  complete listing. Invalidation is unchanged: every command that changes a directory drops its
  listing before its first request, `ls` always asks the server, `refresh` empties the cache.
- **Two directories, one key** (finding 4). Cache keys fold case (`cache_key`), so `cs/Foo` and
  `cs/foo` are one key, and this Samba serves them as two directories with different content
  (2.10). `Cache::feed` therefore looks the folded key up in the walk's own map: not there →
  `put`, remember `true`; there as `true` (another directory of this walk has the key) →
  `invalidate` that key, remember `false`; there as `false` → nothing. Neither of the two is in
  the cache afterwards, and Tab in either asks the server. The map holds one key per directory
  the walk listed, beside the path the walk keeps per directory anyway (5.12).
  **`ls` and Tab have the same collision today** and keep it: `ls cs/Foo` followed by Tab in
  `cs/foo/` offers `Foo`'s entries. It is in `docs/known-limitations.md` ("Cache keys fold
  case … two directories that differ only in case share a cache entry, which only completion
  can show"); it needs the user to visit both, and `ls` itself always asks. A walk would have
  produced it on its own for every such pair, which is why the walk gets the rule.
- **What is not stored.** A directory that denied listing or failed (Tab asks itself and
  remembers the refusal, as now). Listings of `get -r` and `put -r`.
- **The cache's own accounting** (D3). `Cache::total` counts `len + 1` per listing, so a walk
  over many small or empty directories is bounded too; `CACHE_CAP` stays 500,000 (about 50 MB
  when directories hold many entries; about 115 MB if every listing were empty, derived from
  the 230 bytes a directory of 2.5). A listing above the cap is not stored, as now.
- **Eviction.** `Cache` gains `order: BTreeMap<u64, (String, String)>` (tick → key). `put`
  takes the oldest with `pop_first`; `put` over an existing key, `invalidate` and `clear` remove
  the key's tick from it. The linear search and its comment go (2.6: it is quadratic, and a
  walk fills the cache with thousands of small listings).

### 5.5 Depth

The start is depth 0 and is never printed. Its entries are depth 1. `tree -L n` and
`find -maxdepth n` print entries of depth 1 to n and list directories of depth 0 to n − 1: the
walk gets `max_depth = Some(n)`. A directory at depth n is printed and not listed; it costs no
request, and if it is a link back to a visited directory nothing is said about it (it was not
going to be entered: measured, `find / -maxdepth 2` reports no link and `-maxdepth 3` reports
`/loops/self` and `/loops/up`). `n` is a decimal integer of at least 1; anything else is a usage
error. The JSON `depth` of an entry is `dir.depth + 1`.

### 5.6 The glob matcher

```rust
pub struct Glob { /* tokens: Vec<Tok>, fold: bool */ }
enum Tok { Lit(char), Any, Star, Class { negated: bool, ranges: Vec<(char, char)> } }
impl Glob {
    pub fn new(pattern: &str, fold: bool) -> Result<Glob>   // usage: a `/` in the pattern, `[:` in a class
    pub fn matches(&self, name: &str) -> bool
}
```

The pattern is matched against the **name** of an entry (its last path component), and the whole
name must match. Pattern and name are sequences of Unicode scalar values (`char`), not bytes
and not UTF-16 units.

| In the pattern | Means |
|---|---|
| `*` | any run of characters, also none; a leading `.` is not special; `**` is `*` |
| `?` | exactly one character: `?` matches `简`, `𠮷` (two UTF-16 units) and `😀`; `caf?.txt` matches the NFC `café.txt` and `caf??.txt` the NFD one |
| `[abc]`, `[a-c]`, `[一-龥]` | one character out of the set; ranges by code point |
| `[!…]`, `[^…]` | one character not in the set |
| `]` first in a class (`[]a]`, `[!]]`) | a member |
| `-` first or last in a class | a member |
| `\x` | the character `x`, outside and inside a class (`[\]]` is the class holding `]`); a lone `\` at the end is a backslash |
| a range written backwards (`[z-a]`) | holds nothing |
| `[:` inside a class (`[[:digit:]]`) | a usage error: `character classes like [:digit:] are not supported; write the range` (finding 6: silently matching nothing would read as "no such file") |
| `[` with no closing `]` after it | the character `[` |
| anything else, `{`, `}`, `,` included | itself |

Not supported, and documented: `[[:alpha:]]` classes (refused), `{a,b}` alternatives, `**`
across directories, matching the path.

**`-iname`** (finding 3). Two characters are equal when they are the same, or their lower-case
forms are one character each and equal, or their upper-case forms are one character each and
equal (`char::to_lowercase`, `char::to_uppercase`). A class matches a character if it holds
the character or its one-character lower-case or upper-case form. So `ｆｕｌｌ*` finds
`ＦＵＬＬ…`, `привет.txt` finds `Привет.txt`, `GRÜßE.TXT` finds `Grüße.txt`, `INDEX.TXT`
finds `ındex.txt` (dotless ı and I share the upper-case form), `Σ.txt` finds `σ.txt` and
`ς.txt`; `GRÜSSE.TXT` does not find `Grüße.txt` (the upper-case form of `ß` is two
characters), and `istanbul.txt` does not find `İstanbul.txt` (the lower-case form of `İ` is
two characters, its upper-case form is itself).

This **agrees with the server's `find` on the 13 `-iname` patterns of 2.7** and is not the
same rule: the server also folds `İ` with `i`, and its folding is not symmetric where rsmb's
is (2.10: `index.txt`, `ındex.txt`, `ς.txt`, `σ.txt`). Inside classes the server takes `\` as
a member and a backwards range as its two ends; rsmb does not. `docs/commands.md` and
`docs/known-limitations.md` list these.

**No normalisation.** The pattern `한글` in NFC does not match the NFD name, and `パン*` typed
precomposed does not match the NFD `パン…`; rsmb never normalises names (master plan 4.8).

**The algorithm** is the usual one with one backtrack point (the last `*`): linear in name ×
pattern, no recursion, so no pattern is slow.

**A pattern with `/`** is a usage error (`-name matches the last component of a path`): it
could never match. Both usage errors come from `Filter::parse`, before any request.

**Quoting.** The tokeniser does no globbing and knows no special character but quotes, the
backslash and ASCII white space, so `find -name *.txt` works unquoted inside rsmb. Given with
`-C` from a shell, the shell's own quoting applies first: `-C "find -name '*.txt'"`. Outside
quotes the tokeniser takes a backslash for itself, so a glob escape is written in single quotes
(`-name '\[abc\].txt'`), doubled (`-name \\[abc\\].txt`), or as a class (`-name '[[]abc[]].txt'`).

### 5.7 Size

```rust
/// `[+|-]N[BKMGTP]`: how a size must compare, and with how many bytes.
pub fn parse_size(s: &str) -> Result<(std::cmp::Ordering, u64)>
```

`N` is a decimal integer; the unit is `B` or `c` (1), `K` (1024), `M` (1024²), `G`, `T`, `P`,
the last five in upper or lower case, none meaning bytes. A lower-case `b` is a usage error
that says why (`b means 512-byte blocks in find; write B or c for bytes`), and so is `C`
(finding 6: `find -size 10c` is what a Unix user types for bytes, and `10b` would be off by a
factor of 512 without a word). `+N` → `Greater`: the size is **more than** N units.
`-N` → `Less`: **less than** N units. `N` → `Equal`: **exactly** N units in bytes. No rounding:
a file of 1,048,577 bytes matches `+1M`, one of 1,048,576 matches `1M` and neither `+1M` nor
`-1M`. A value that does not fit 64 bits, an unknown unit, a fraction or an empty number is a
usage error that shows the form.

`-size` matches **files only**. SMB reports 0 for a directory, so `-size -1K` would otherwise
list every directory. `-size` together with `-type d` matches nothing.

Differences that `docs/commands.md` states: Unix `find` rounds a size up to the unit before
comparing (`-size -1M` there means empty files only) and its `b` is 512-byte blocks;
smbclient-ng's `+` and `-` include the boundary. rsmb compares bytes, strictly.

### 5.8 `find`

```rust
pub struct Filter { /* name: Option<Glob>, iname: Option<Glob>, dir: Option<bool>, size: Option<(Ordering, u64)> */ }
impl Filter {
    /// From the flags with a value; also returns `-maxdepth`. Usage errors for a bad value.
    pub fn parse(opts: &[(String, String)]) -> Result<(Filter, Option<usize>)>
    pub fn matches(&self, e: &Entry) -> bool          // every given test must hold
}
async fn find(sess: &mut Session, out: &mut Out, a: &Args) -> Result<()>      // commands.rs
```

- The start is `sess.resolve(a.pos[0])`, default the working directory. All tests are ANDed;
  `-name` and `-iname` may both be given. `-type` takes `f` or `d`; anything else is a usage
  error. With no test every entry matches.
- For every listed directory, in the walk's order (breadth first, the same on every run, 5.3):
  each entry that matches is printed, the entries of one directory in name order, then stdout
  is flushed once. Filters cost no request: they read what the listing carries. A directory is
  entered whether or not it matches.
- **Text:** one line per match, the path from the share root, with `/` after a directory
  (as `ls`, `lls` and completion mark one): `/tree/d0/file.txt`, `/tree/d0/`. Nothing else goes
  to stdout; no summary line.
- **JSON:** one `entry` per match (5.14).
- Then the end of the walk (5.10).
- Cost on Samba: 4 requests in 1 round trip per directory listed, 16 at a time; 0 per entry;
  a directory above 1 MiB of entries more (2.1).
- `rerun: false` in the table, with one exception in `run_parsed` (S3-4, finding 5): `find`
  runs again once after a closed connection if `Out::written()` is false, that is if nothing
  of this command has gone to stdout. `Out` sets the flag in `write` and `raw` and clears it in
  `begin`. That covers a start listing that was lost and a walk that had found nothing yet;
  nothing can be printed twice.

### 5.9 `tree`

```rust
/// The lines of a tree below `start`, depth first, entries in the order of each listing.
/// `nodes`: the listed directories by path. `notes`: why a directory was not entered.
/// `line(depth, path, entry, text)` gets every entry with its drawn line.
pub fn draw_tree(start: &[String], nodes: &HashMap<Vec<String>, Arc<Vec<Entry>>>,
                 notes: &HashMap<Vec<String>, String>,
                 line: &mut dyn FnMut(usize, &[String], &Entry, &str)) -> (usize, usize)   // (directories, files)
async fn tree(sess: &mut Session, out: &mut Out, a: &Args) -> Result<()>       // commands.rs
```

- The walk collects `path → Arc<Vec<Entry>>` for every listed directory and counts the
  entries. When the count passes `TREE_MAX`, `on_dir` returns `false` and the command fails:
  `tree /x: more than 500000 entries; limit the depth with -L, or use find` (kind `other`),
  nothing drawn.
- After the walk, `notes` is built from the report (`access denied`, `already listed as
  /loops`, `gone`, the reason of any other failure), and the tree is drawn:

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

  The first line is the start, from the share root. Each entry: the prefix of its ancestors
  (`│   ` while that ancestor has entries still to come, four spaces after its last), `├── ` or
  `└── ` for the last entry of its directory, the name, `/` after a directory, and ` [note]`
  when it has one (`├── forbidden/ [access denied]`). Then an empty line and
  `N directories, M files`, counting what was drawn. The drawing characters are always these;
  names are UTF-8 already. A directory at depth `-L` is drawn without children and without a
  note. `draw_tree` recurses once per level of depth.
- **JSON:** one `entry` per drawn entry, in the same order, with `depth`; no drawing.
- Then the end of the walk (5.10). In text mode the notes are thus said twice: on the line and
  at the end.
- `rerun: true`.

Both commands share

```rust
/// remote(), the walk with the progress line and the cache feed, finish(). `each` gets every
/// listed directory (path, depth, entries) and says whether to go on.
async fn run_walk(sess: &mut Session, out: &mut Out, label: &str, start: &[String],
                  max_depth: Option<usize>,
                  each: &mut dyn FnMut(&mut Out, &[String], usize, &Arc<Vec<Entry>>) -> bool)
                  -> Result<(walk::Report, usize /* entries seen */, Duration)>
```

which has the shape every command with a `Remote` has (`docs/development.md`); it registers no
temporary file, so `finish` only settles `in_flight`. Both return a usage error before any
request when no share is selected or a flag value is bad.

### 5.10 The end of a walk

```rust
pub struct EndLine { pub kind: &'static str /* "denied" | "skipped" */, pub path: String,
                     pub reason: &'static str /* "" | "already_listed" | "gone" | "no_file_id" */,
                     pub same_as: Option<String>, pub text: String }
/// What to say after a walk, sorted by path within each kind; the failures; whether the walk is incomplete.
pub fn end_report(start: &[String], r: &Report) -> (Vec<EndLine>, Vec<Error>, bool)
```

After a walk that ran to its end, in this order:

1. every **denied** directory, sorted by path: `{"type":"denied","path":"/tree/forbidden"}`;
   text, on stderr: `rsmb: /tree/forbidden: access denied; not listed`;
2. every **skipped** directory, sorted by path:
   - `already_listed`: `{"type":"skipped","path":"/loops/self","reason":"already_listed",
     "same_as":"/loops"}`; text: `rsmb: /loops/self: not entered: already listed as /loops`
     (no claim about which of the two is a link, finding 1);
   - `gone` (P14): `same_as` null; text: `rsmb: /x/y: gone before it was listed`;
   - `no_file_id` (`Report::capped`): text: `rsmb: /x/y: not entered: the server gives no file
     ids and the walk is at its limit (depth 100, 100000 directories)`;
3. every other **failure**, as `Out::item_error` prints it (an `error` object with `path`);
4. the summary: `{"type":"walk_summary","command":"find","path":"/tree","directories":341,
   "entries":682,"matched":682,"denied":1,"skipped":0,"failed":0,"seconds":0.086}`.
   `directories` are the listings that succeeded (the start included), `entries` every entry
   those listings held, `matched` the `entry` lines printed. Text: `tree` prints its count
   line, `find` nothing;
5. if any directory failed or was skipped for `no_file_id`: the command fails with `other`,
   `find /tree: incomplete: 2 directories could not be listed` (exit 1). Otherwise it succeeds
   (S3-1).

`Out::diag(line)` is new: in text mode it flushes stdout and writes `rsmb: <line>` to stderr;
with `--json` it does nothing (the object carries it).

No summary and no end report after an interrupt, a lost connection, the `tree` cap or a closed
stdout: the absence of `walk_summary` together with `done`'s `ok: false` is how a program sees
a result that is not whole. `docs/scripting.md` states that rule.

### 5.11 Interrupt, lost connection, timeouts, credits

- **Ctrl-C, SIGINT, SIGTERM** (Stage 1 and 2 mechanics, unchanged): the command's future is
  dropped, the `JoinSet` aborts its listings, `Session::cleanup` waits until every `Remote`
  clone is gone (nothing is registered, so it sends nothing), `abandon()` marks the connection
  dead because `in_flight` is set. `find` has printed and flushed the matches of every
  directory that had arrived, always whole lines; `tree` has printed nothing. Then the
  `interrupted` error and `done`; the non-interactive runner exits 130, the shell returns to
  the prompt and the next command that sends reconnects (4 round trips). Measured for the walk
  phase of `get -r` (2.4): exit 130 within 1 ms.
- **A closed connection.** `walk` returns `connection_lost` once the listings in flight have
  failed. `tree`: `run_parsed` runs it again once on a new connection (2.4: the second walk
  completes; 2 connections in all). `find`: run again the same way while it has printed
  nothing (5.8); otherwise the error, with the tail `'<line>' was not run again: the results
  above are incomplete` (in `run_parsed`, in place of the tail for commands that change
  something), and the next command reconnects.
- **A silent link**: neither is run again (Stage 1 rule); the error comes after between
  `--timeout` and max(`--timeout`, 15 s).
- **The deadline** is per listing: one listing is one reply (or, above 1 MiB of entries, one
  reply per MiB), which must be complete within 6 × `--timeout`.
- **Credits.** 16 listings are 16 compounds of 19 credits on Samba (304 of a new connection's
  513) and of 4 on edge, where the crate waits for credits and four run at a time. Measured
  (2.1): peak outstanding 64 and 16; the whole share on edge without a stall.
- **One connection.** Nothing in this stage dials.

### 5.12 Memory

| Command | Holds | Bound |
|---|---|---|
| `find` | at most 16 listings in flight or waiting for an earlier one, the directories queued, the id and path of every directory entered, the folded cache key of every directory listed | one listing is as large as its directory (`huge/`: peak 25 MB for the process); the rest grows with the number of **directories**, about 230 bytes each for the paths (2.5; `fanout/` 8.8 MB) and a key more for the cache. Not capped; a known limitation |
| `tree` | the above and every listing | `TREE_MAX` entries: 44 to 68 MB measured, 167 MB in the worst shape |
| the cache after a walk | whatever the walk listed, shared with `tree`'s own copy while it runs | `CACHE_CAP` units, a listing counting `len + 1` |

Gate rows: `find /huge` and `tree /huge` peak at most 64 MB (F43).

### 5.13 The table entries, the manual, completion

```rust
Cmd { id: CmdId::Tree, name: "tree", syntax: "tree [-L <n>] [<dir>]",
      summary: "draw a directory and everything below it as a tree",
      flags: &[("-L <n>", "at most n levels below the directory; 1 shows its entries only")],
      args: &[Arg::RemoteDir], required: 0, rerun: true, .. }
Cmd { id: CmdId::Find, name: "find",
      syntax: "find [<dir>] [-name <glob>] [-iname <glob>] [-type f|d] [-size [+|-]<n>[BKMGTP]] [-maxdepth <n>]",
      summary: "list what is below a directory, filtered by name, type, size and depth",
      flags: &[("-name <glob>", "the name matches the pattern: * ? [a-z] [!x], \\ escapes"),
               ("-iname <glob>", "the same without regard to letter case"),
               ("-type <f|d>", "files only, or directories only"),
               ("-size <[+|-]n[BKMGTP]>", "files of more than (+), less than (-) or exactly n units: B or c bytes, K M G T P powers of 1024"),
               ("-maxdepth <n>", "at most n levels below the directory; 1 is its entries only")],
      args: &[Arg::RemoteDir], required: 0, rerun: false, .. }
```

`details` of each say: the default directory, depth, the cost in requests, the order, what the
output is in text and with `--json`, what is reported at the end and the exit status, that
`tree` is limited to 500,000 entries, the error kinds, and for `find` that it is run again after
a lost connection only while it has printed nothing.

Examples (each runs after `use share` at the share root, as H3 runs them, and succeeds):

| Line | Does |
|---|---|
| `tree /tree/d0/d0` | draw a directory and everything below it |
| `tree -L 2 /tree` | two levels only (one directory in it denies listing: reported, still exit 0) |
| `find /tree -name file.txt` | every entry called file.txt below /tree |
| `find /sizes -size +1M` | files larger than 1 MiB |
| `find /special -iname '*.TXT' -type f` | files ending in .txt in any letter case |
| `find /tree -maxdepth 2 -type d` | directories at most two levels down |
| `find /cjk -maxdepth 1 -name '简体中文*'` | a pattern with non-Latin characters |
| `find /special -name '[[]abc[]].txt'` | a name with a literal `[` |

A seventh topic of the manual, "tree and find", holds the rules a reader needs in one place:
depth, the pattern table of 5.6 with quoting, the size rules, the path form, the order, what
is reported at the end and when the exit status is 1, the summary rule of 5.10, the cache, the
`tree` limit. `topics().len()` becomes 7 and `help_all_has_everything` gets the part
`TREE AND FIND`, the three new JSON types and a few phrases of the topic.

Completion: 5.2 and P15.

### 5.14 Output: the objects

| Type | Fields |
|---|---|
| `entry` | as now (`name`, `path` from the share root, `is_dir`, `size`, `mtime`, `file_id`), and from `tree` and `find` also `depth` integer (1 = directly in the start directory). One function, `entry_obj(dir, entry, depth: Option<usize>)`, builds it for `ls`, `tree` and `find` |
| `denied` | `path` string: a directory whose listing the server refused; what is in it is unknown |
| `skipped` | `path` string, `reason` `"already_listed"` \| `"gone"` \| `"no_file_id"`, `same_as` string or null (for `already_listed`: the path under which the directory with the same file id was listed) |
| `walk_summary` | `command` `"tree"` \| `"find"`, `path` string (the start), `directories`, `entries`, `matched`, `denied`, `skipped`, `failed` integers, `seconds` number; once, only after a walk that ran to its end |
| `error` with `path` | a directory that failed for another reason; makes the command fail |

Fifteen error kinds as before; no new kind.

## 6. Tests

### 6.1 The harness, additions to `tests/common/mod.rs`

```rust
/// The server's own find below /srv/share, run as tester so that it sees what the SMB user sees:
/// `docker exec -u tester -w /srv/share <container> find <dir> -mindepth 1 <args…> -print0`,
/// stderr and the exit status ignored (a directory it may not read). Paths relative to the share root.
pub fn server_find(container: &str, dir: &str, args: &[&str]) -> BTreeSet<Vec<u8>>
impl Reply { pub fn paths(&self) -> BTreeSet<Vec<u8>> }     // `path` of every entry object, without the leading /
pub fn skipped(r: &Reply) -> Vec<&Value>;  pub fn denied(r: &Reply) -> Vec<&Value>
pub fn walk_summary(r: &Reply) -> Option<&Value>
```

`piped(ep, line, filter)`: a tap in front of `ep`, then `bash -c '<rsmb through the tap> -C
"use share" -C <line> | <filter>; exit ${PIPESTATUS[0]}'` (`run_sh`); returns rsmb's exit
code, the filter's output, the time and the tap's `Stats`.

**One new fixture, `links/`** (finding 1), in `testenv/samba/fixtures.sh` beside `loops/`:

```
mkdir -p links/a links/b/c
echo linked > links/b/c/file.txt
ln -s ../b/c links/a/to-c
```

A link to a sibling: over SMB `links/a/to-c` and `links/b/c` are two directories with one file
id and the same content (2.10). `docs/test-environment.md` gets the row ("which of two paths
to one directory a walk lists"), `check.sh` one check (with smbclient: `links/a/to-c/file.txt`
reads `linked`; in the container `links/a/to-c` is a symbolic link). What it changes elsewhere:

- The image is rebuilt once from the fixtures layer on (the 5.5 GiB layer of the large files
  is a layer of its own and stays); `./testenv/down.sh` and `up.sh` recreate the three Samba
  containers. That is in "test environment up", which is not counted time. Not measured
  (assumption 16).
- The share root has 18 directories instead of 17; a walk of the whole share has 4 more
  listings and 6 more entries than section 2 shows (measured before the fixture). X44 takes
  its expected number from the server when it runs; the bench's `find /` row is printed, not
  asserted.
- No test of Stage 1 or 2 lists the share root with a count or completes a name at the root
  that starts with `l` (checked: `grep` over `tests/` for root listings and Tab lines), so none
  changes.

Rules:

- The arguments go to `find` as an argument vector, never through a shell, so a pattern needs
  no quoting in the test.
- Sizes are compared with `-type f -size <bytes>c`, the bytes computed by the test from the
  unit: BusyBox `find` has no `M`, and `c` does not round.
- The containers' `find` is BusyBox; the 60 patterns of 2.7 are the ones known to agree. A new
  pattern in a test is first checked by hand the same way.
- No test in `tests/fast.rs` or `tests/slow.rs` walks into `upload/`: other tests create and
  remove directories there while it runs. The one walk from the share root (X44) is in
  `tests/serial.rs`.
- A test that acts in the middle of a walk waits for an observed event:
  `wait_requests("QUERY_DIRECTORY", n, …)`.

### 6.2 `./test.sh`

Steps and locks as now. The budget of the full run becomes **540 s** (master plan section 6,
"after stage 3: ≤ 9 min"); `fast` stays 120 s. Counted today: 351 s full, 43 s `fast`.
Expected: `check.sh` +1 s (the `links/` check), unit tests +1 s, `tests/fast` +9 s (most rows
are a few milliseconds each at the fast endpoint; the eight examples are eight processes; F50
is twenty walks of five entries), `tests/slow` +12 s of wall time (its tests run side by side;
S44 is the longest new one at about 25 s with its third cut, S41 walks `tree/` three times),
`tests/serial` +36 s (X41 12 s, X42 10 s, X43 10 s, X44 2 s, X1 and X2 2 s): **about 410 s of
540 for the full run, about 54 s of 120 for `fast`.** After the review the estimate moved by
3 s (it was 407): the queue order costs the gate nothing (`tree/` and `fanout/` measured the
same, 2.3), the new rows are small. The one-time rebuild of the image for `links/` is in "test
environment up", which is not counted. Nothing has to move to the bench. No new test is longer
than 30 s. If the first full run is over, X41's second half (`tree /tree`, 5 s) goes first:
S41 already counts it.

### 6.3 Unit tests (no server)

| Test | File | Asserts |
|---|---|---|
| `glob_basics` | walk | literals; `*` matches nothing, everything and a leading dot; `?` one character and not none; `a*b*c`; `**`; a pattern longer than the name; the empty pattern matches only the empty name |
| `glob_classes` | walk | `[abc]`, `[a-c]`, `[!a]`, `[^a]`, `[]a]`, `[!]]`, `[a-]`, `[-a]`, `[[]`, `\[`, `\*`, `\\`, a lone `\` at the end; `[\]]` matches `]` and nothing else; `[z-a]` matches neither `a` nor `z`; `[` and `[abc` and `*[*` without a closing bracket are literal; `{a,b}` is literal; `[[:digit:]]*` and `[x[:alpha:]]` are usage errors that name the class; `a/b` is a usage error (finding 6) |
| `glob_unicode` | walk | `?` matches `简`, `𠮷`, `😀` (one character each); `caf?.txt` matches the NFC name and not the NFD one, `caf??.txt` the reverse; the NFC `한글` pattern does not match the NFD name; U+3000 inside a pattern; `[简繁]*`; `[一-龥]*`; every name of `CJK_NAMES` and `SPECIAL_NAMES` matches the pattern made by putting `\` before each of its characters, and matches `*` (master row 3.11) |
| `glob_fold` | walk | with `fold`: `CASE.TXT`/`case.txt`; `ｆｕｌｌ*`/`ＦＵＬＬ…`; `привет`/`Привет`; `GRÜßE`/`Grüße`; `GRÜSSE` does not match `Grüße`; `CAFÉ` (NFC) matches the NFC name only; `[A-C]ase.txt` matches `case.txt`; **`INDEX` matches `ındex` and `index`, `ındex` matches `INDEX`; `Σ`, `σ` and `ς` match one another; `istanbul` does not match `İstanbul`, `İSTANBUL` does** (finding 3; the names of 2.10); the relation is symmetric for every pair tried; without `fold` none of these (3.11) |
| `glob_is_linear` | walk | `a*a*a*a*a*a*a*a*b` against 5,000 `a`: false, within a second |
| `size_parse` | walk | `0`, `10`, `10B`, `10c`, `1K` = 1,024, `1k`, `1M`, `1m`, `1G`, `1T`, `1P`, `+1M` → `Greater`, `-1M` → `Less`; usage errors: the empty string, `+`, `K`, `1X`, `1.5M`, `1KB`, `--1`, `99999999P`, `10C`, and **`10b`, whose message contains `512`** (finding 6) |
| `filter_rules` | walk | `+N` strictly more, `-N` strictly less, `N` equal, at the boundaries 1,048,575 / 1,048,576 / 1,048,577; a directory never matches `-size`; `-type`; `-name` and `-iname` and `-size` together; no test matches everything; `-type x`, `-maxdepth 0`, `-maxdepth x`, a pattern with `/` are usage errors |
| `tree_drawing` | walk | a fake tree: the exact text, with `├──` and `└──`, the prefixes two levels down, `/` after a directory, a note, a directory that was not listed drawn without children, an empty directory, a name with a space and one from `CJK_NAMES` unchanged; the two counts |
| `walk_stops_when_told` | walk | `on_dir` returns `false` at the third directory of a wide fake tree: no listing is started after that, the ones in flight return, the result is `Ok` |
| the seven `walk_*` tests of Stage 2 | walk | unchanged but for the closure's `true`; `walk_width` still sees 16 at once and never more |
| `walk_takes_results_in_queue_order` | walk | a fake tree three levels deep whose listings take longer the earlier they were queued: `on_dir` is called in the order of a breadth-first walk done one at a time, 20 runs alike; **a fake `links/`** (`a/to-c` and `b/c` with one id, `a`'s listing slower than `b`'s): in 20 runs `a/to-c` is listed and `b/c` is in `links` with `a/to-c` as its first path; never more than 16 listings in flight or waiting (finding 1) |
| `walk_root_parent_is_not_seeded` | walk | a fake root whose `..` has the id 5 and which holds a directory with the id 5: no panic, that directory is listed and not reported; below the root (`start` = `[x]`) the same id is reported with `None` (finding 7) |
| `end_report_rules` | walk | a report with a denied directory, a link with a first path, a link to the start's parent (`same_as` is the parent's path), a capped one, a `not_found` failure and an `other` failure: the lines in order and sorted, `gone` for `not_found`, one failure left, incomplete true; without the capped and the `other` one: incomplete false |
| `cache_units_and_order` | session | a listing costs `len + 1`; an empty listing costs 1; the oldest goes first; a `put` over the same key and an `invalidate` leave no stale index entry; 200,000 listings of one entry into a cap of 100,000 finish within 2 s (D3; 2.6 measured 1.6 s for 80,000 at 40,000 before) |
| `cache_cap`, `cache_invalidate`, `cache_ignores_case` | session | as now, the numbers of `cache_cap` adjusted to units |
| `cache_feed_collision` | session | `Cache::feed` with one walk's map: `cs/Foo` is stored; `cs/foo` after it removes the entry, and `get` of either finds nothing; a third `cs/FOO` stores nothing; another directory is stored as usual; a new walk (a new map) stores `cs/Foo` again (finding 4) |
| `out_written` | commands | `Out::written` is false after `begin`, true after `text`, `obj` or `raw` wrote, false after the next `begin`; in JSON mode `text` does not set it (finding 5) |
| `flags_with_values` | commands | `find -name x /d` and `find /d -name x` parse alike; `find -name` → usage; `find -name -type` takes `-type` as the value; `find -name --` takes `--`; `find -- -name` is a positional; `find -name a -name b` → usage; `tree -L 2 d`; `tree -L` → usage; `positional_index` is `None` after `find -name` and `Some(0)` after `find -name x`; `get -r -f a b` and `mkdir -p x` as before |
| `table_is_complete`, `help_all_has_everything` | commands | as now, over 19 commands; the types `denied`, `skipped`, `walk_summary`; the topic |
| `no_candidates_for_a_flag_value` | shell | the completer's pure part: after `find -name ` no argument kind |

### 6.4 Integration tests

Every test runs the real binary. "tap" = through `smbtap`; "direct" = to the endpoint. "The
server's set" is `server_find` with the same predicate.

**`tests/fast.rs`** (fast endpoint, parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| F41 | `walk_depth` | direct; `find /tree -maxdepth 1`, `2`, `3`, no limit; `tree -L 1`, `2`, `3`, no limit | each set of `path`s equals the server's set with the same `-maxdepth` (6, 26, 106, 682 entries); every `depth` equals the number of components below `/tree`; `-maxdepth 0` and `-L 0`: kind `usage`, exit 2, 0 requests; from `cd /tree/d0`: `find` and `find .` give `/tree/d0/…` paths; text mode: `tree /tree/d0/d0/d0` is exactly the drawing of 5.9; `tree /tree` has 682 + 3 lines, the first `/tree`, the last `341 directories, 341 files`; `find /tree` in text prints 682 lines, directories with `/` | 3.1 |
| F42 | `find_filters` | direct; one session, a table of predicates | `-type f` and `-type d` on `/tree` (341 each); on `/sizes`: `-size +65536` (7), `-65536` (3), `65536` (1), `+64K`, `-64K`, `64K`, `+1M` (4), `-1M` (6), `1M` (1), `+0` (10), `0` (1), `1024k`, `1T` and `1P` (0); on `/large`: `+1G` (2), `-1G` (1), `+4G` (1); each set equals the server's `-type f -size <bytes>c`; `65536c` (1) and `+65536B` (7); `-size 10b`: `usage`, the message has `512`; `-name '[[:digit:]]*'`: `usage`, 0 requests; `-size +0 -type d` on `/tree`: nothing; `-name file.txt -type f -maxdepth 3` on `/tree` (20); `-iname '*.TXT' -size +0 -type f` on `/special`; every one of the 60 patterns of 2.7 on `/special` and `/cjk`, the set equal to the server's `-name` or `-iname` with the same pattern | 3.2 |
| F43 | `walk_counts_fast` | tap | `find /fanout`: "expected 8,004 requests (2,001 listings × 4)", 4,000 entries equal to the server's set, peak outstanding ≤ 64 and > 4, 0 new connections; `find /chain`: 244 requests in exactly 61 flights, `bottom.txt` found at depth 61; `find /many -name 'f0999?'`: 10 matches, 9 requests (a filter costs none); `find /huge`: 100,000 entries, 19 requests; `tree /huge`: the same count; after each of the last two "peak RSS ≤ 64 MB" (a session of its own) | 3.4, 3.7 |
| F44 | `walk_past_denied` | direct | `find /tree` and `tree /tree`: `ok` true, exit code 0 in a one-shot run, exactly one `denied` (`/tree/forbidden`), `walk_summary.denied` 1 and `directories` 341, the entries equal to the server's set; `/tree/forbidden` is itself an `entry`; `find /forbidden`, `find /traverse-only`: kind `access_denied`, exit 1, no `walk_summary`; `find /nope`: `not_found`; `find /sizes/1.bin`: `not_a_directory`; text mode: stderr has `rsmb: /tree/forbidden: access denied`, stdout only paths; `tree` in text shows `forbidden/ [access denied]`; `find` before `use`: `usage`, exit 2 | 3.5 |
| F45 | `walk_link_loops` | tap | `find /loops` and `tree /loops`: end; 4 requests in 1 flight; the entries are `file.txt`, `self`, `up`; two `skipped` with reason `already_listed`: `/loops/self` with `same_as` `/loops`, `/loops/up` with `same_as` `/` (the start's parent); `ok` true, exit 0; `walk_summary`: `directories` 1, `skipped` 2; `find /loops -maxdepth 1`: the same three entries and no `skipped`; text mode: stderr has `already listed as /loops` and `already listed as /`, and no line says `link`; `tree` marks both on their lines | 3.6 |
| F46 | `walk_objects` | direct; `find` and `tree` on `/tree`, `/loops`, `/special` | every line parses; `entry` has the documented fields with `depth`; `denied`, `skipped`, `walk_summary` have theirs with the documented types; `matched` equals the number of `entry` lines, `entries` ≥ `matched`; with `-name nomatch`: `matched` 0 and `entries` 682; the objects come in the order entries, denied, skipped, summary, done | 3.8 |
| F47 | `names_in_patterns` | direct | through the tokeniser: `find /cjk -maxdepth 1 -name '東京　大阪*'` quoted and unquoted (114 each: U+3000 does not split); `find /special -name "it's.txt"`; `-name '\[abc\].txt'`, `-name \\[abc\\].txt` and `-name '[[]abc[]].txt'` (1 each); `-name -rf` finds `/special/-rf`; `-name '--help*'`; `find "/special/dir with space" -name leaf.txt`; the NFC and the NFD Korean directory are found by their own pattern and not by the other's; `-name 'a/b'`: `usage` | 3.2 |
| I41 | `walk_feeds_the_cache` | pty through the tap | after `tree -L 2 /tree`: `cd /tree/d0/` + Tab shows `d0/` … `d3/` with "0 requests"; `cd /tree/d0/d0/` + Tab: "one listing (5 requests)" (depth 2 was drawn, not listed); after `find /cjk -maxdepth 1 -name nomatch`: Tab in `/cjk/`: 0 requests; `find -name ` + Tab: 0 requests, no candidates; in a scratch directory: `find .`, `mkdir x`, Tab: one listing (the walk's listing was dropped) and `x/` shown; in a scratch directory holding `cs/Foo/x` and `cs/foo/y` (made in the container): after `find .`, Tab in `cs/Foo/` and Tab in `cs/foo/` cost "one listing (5 requests)" each, neither was stored (finding 4); Tab in the scratch directory itself: 0 requests | U5, finding 4 |
| F50 | `walk_is_the_same_every_run` | direct; the fixture `links/`; `find /links` ten times and `tree /links` ten times in one session; `find /fanout` twice | every `find /links` prints the same lines in the same order, every `tree /links` too: entries `a`, `b`, `a/to-c`, `b/c`, `a/to-c/file.txt` (five, `file.txt` once); one `skipped`: `/links/b/c`, `already_listed`, `same_as` `/links/a/to-c`, in all twenty; `walk_summary.directories` 4; the two `find /fanout` runs print the same 4,000 paths in the same order, and it is the order of 5.3 (the 2,000 directories by name, then each directory's `file.txt` in that order). Completion order failed this in 7 of 10 and in 3 of 3 runs (2.10) | finding 1, P3, P4 |
| F49 | `walk_encrypted` | direct, share `encrypted`, in the session of `encrypted_share` or one of its own | `find /tree`: the 682 entries of the server's set; `tree -L 1 /tree` | 3.2, risks |
| N4 | `every_json_line_has_a_type` (extended) | direct | both commands once, a failing one of each | 1.11 |
| H2, H3, H4 | (unchanged in form) | | over 19 commands; the eight new examples run (the count in H3's last assertion goes up by eight) | 1.12 |
| F14 | `counts_add_up` (extended) | tap; the script of X1 | requests equal over the whole script; flights compared without the lines whose requests overlap (the two transfers and now the walks, Stage 2 I5) | §5 method |

**`tests/slow.rs`** (slow endpoint, edge, wan-bad; parallel)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| S41 | `walk_counts_slow` | tap, slow; `find /tree`, then `tree /tree` | each "expected 1,368 requests (342 listings × 4)"; peak outstanding ≤ 64 and ≥ 8; 0 new connections (1 for the session); 682 entries equal to the server's set; `/tree/forbidden` denied; `find /tree` a second time: the same 682 lines in the same order as the first (at the fast endpoint answers rarely overtake one another; here they do) | 3.3, finding 1 |
| S42 | `walk_deep_slow` | tap, slow; `find /deep` | 52 requests in exactly 13 flights (nothing can overlap, so flights mean round trips); `bottom.txt` found | 3.7 |
| S43 | `walk_edge` | tap, edge (16 credits) | `find /fanout`: 4,000 entries equal to the edge server's set, done within 60 s; `find /tree`: 1,368 requests; `tree /many`: 10,000 entries; over the whole connection the credit balance never below 0 (`credit_balance`); the peak outstanding is printed | 3.4 |
| S44 | `cut_mid_walk` | tap, slow; three sessions: `tree /tree`, `drop_connections()` at the 100th QUERY_DIRECTORY; `find /tree`, the same cut; `find /tree`, the cut at the **first** QUERY_DIRECTORY (the start's listing is on the wire, nothing printed) | `tree`: `ok` true, 682 entries, a `walk_summary`, "exactly 1 new connection". `find` cut in the middle: kind `connection_lost`, the message contains `incomplete`, `ok` false, **no `walk_summary`**, fewer than 682 entries before the error, 0 new connections for the failed command; then `find /tree/d0` succeeds on "exactly 1 new connection" with 169 entries. `find` cut at its first listing: **run again**, `ok` true, 682 entries each once, a `walk_summary`, "exactly 1 new connection" (finding 5) | 3.9 |
| S46 | `find_and_a_closed_pipe` | tap, slow; `piped(Ep::Slow, "find /fanout", "head -n 5")` | five lines; rsmb's exit code is not 0; it ended within 5 s (the whole walk takes 26 s); "expected at most 200 requests after the reader left, of the 8,004 the whole walk sends" (measured 4; a walk that does not stop sends all 8,004); stderr has no `panicked` | P13, finding 2 |
| S45 | `walk_wan_bad` | direct, wan-bad; `find /tree/d0` | 169 entries; the time is printed | 1.15 style |

**`tests/serial.rs`** (one at a time, exclusive lock)

| # | Test | Fixture, how observed | Asserts | Master row |
|---|---|---|---|---|
| X41 | `timing_walk_slow` | direct, slow; `connect_ms` just before; in one session `find /tree`, then `tree /tree`, each timed from the line to its `done` | each "≤ 1.25 × 23 waves × round trip" (5.76 s at 200.2 ms; measured 4.82 to 4.83 s for the walk); printed beside one after another (342 round trips, 68.5 s) and Stage 0's 18.5 s; `ctl.sh drops` unchanged | 3.3 |
| X42 | `signals_mid_walk` | tap, slow; non-interactive; `find /fanout`, SIGINT at the 200th QUERY_DIRECTORY; again with SIGTERM; `tree /fanout` with SIGINT | each: an `error` of kind `interrupted`, exit 130 within 3 s of the signal, no `walk_summary`; `find`: at least one `entry` line before the error and every stdout line parses (whole lines); `tree`: no `entry` line | brief, three uses |
| X43 | `ctrl_c_mid_walk` | pty through the tap, slow; `find /fanout`, Ctrl-C at the 200th QUERY_DIRECTORY | `interrupted`, the prompt returns within 3 s; `ls /sizes` then lists 11 names on "exactly 1 new connection"; `pwd` unchanged; `find /tree/d0` then prints 169 lines with 0 new connections | brief, three uses |
| X44 | `walk_root_loops` | tap, fast; under the exclusive lock nothing else writes to `upload/`; `find / -maxdepth 3 -name self`, then `tree -L 3 /` | both end; `skipped` holds `/loops/self` → `/loops`, `/loops/up` → `/` and `/links/b/c` → `/links/a/to-c` in the `find` (in `tree -L 3` the last is cut by the depth, 5.5); `denied` holds `/forbidden`, `/traverse-only`, `/tree/forbidden`; `walk_summary.directories` equals 1 + the server's directories of depth 1 and 2 (`server_find(".", ["-maxdepth", "2", "-type", "d"])`, 2,055 when measured, 3 more with `links/`) − 3; no path below `/loops/self` or `/loops/up` is printed; `ok` true; 0 new connections | 3.6 |
| X1, X2 | `one_connection_fast`, `one_connection_edge` (extended) | the script followed by `tree -L 2 /tree`, `find /tree -name file.txt`, `find /fanout -maxdepth 1 -type d` | "expected 1 TCP connection in the tap and 1 on the server" | 3.10 |

Rows of master plan section 9 and where each is enforced: 3.1 F41 · 3.2 F42, F47, F49 · 3.3
S41 (counts, in-flight bound, one connection), X41 (time) · 3.4 F43, S43 · 3.5 F44 · 3.6 F45,
F50, X44 and the walk unit tests · 3.7 F43 (`chain/`), S42 (`deep/`) · 3.8 F46, N4 · 3.9 S44 ·
3.10 X1, X2 · 3.11 the five `glob_*` tests. The task's further rows: interrupt X42, X43; help
completeness H2 to H4 and the two table tests; the cache I41; memory F43; the same output on
every run F50, S41; a closed stdout S46. (There is no F48: it became S46, finding 2.)

Not in the gate, and why: a walk of a million entries (`xl/` is opt-in; the `tree` cap is
tested through `walk_stops_when_told` and the numbers of 2.5 are synthetic); a server without
file ids (the unit tests of Stage 2 cover the caps); two **unrelated** directories with one
file id (no fixture, assumption 2; `links/` covers two paths to one directory); the names of
2.10 against the server's `-iname` (they are not in the image; the unit test has them);
`fanout/` at the slow endpoint (26 s; in the bench); a silent link in
a walk (the path is Stage 1's, tested there for `ls`; 15 s more for no new code); a Windows
server.

### 6.5 `testenv/bench-rsmb.sh walk`

A third mode beside `quick` and `transfers`; not part of the gate; **shared** lock for the
whole run (it changes nothing; the lock table of `docs/test-environment.md` gets the row).
About 5 minutes. `ctl.sh drops` before and after; the connect time first.

| Row | Method | Reported |
|---|---|---|
| `tree /tree/d0` (85 directories, none denied), slow: rsmb 3 runs, smbclient-ng 1 run | for both clients one process per run and (time with the command − time without it), as the navigation rows; counts through the tap in a further run each; smbclient-ng judged by the rules of `bench-smbclientng.sh` (login line, no `[error]`, the prompts) and by 85 `file.txt` lines | seconds, requests and flights of each (expected from 2.9 and 2.1: 69 s in 342 flights against 1.6 s in 340 requests) |
| `find /tree`, slow, 3 runs | the product's `walk_summary.seconds` | beside 23 waves × round trip and 342 round trips one after another |
| `find /fanout`, slow, 3 runs | the same | beside 126 waves and 2,001 round trips |
| `find /chain`, slow, once | | beside 61 round trips (nothing to overlap) |
| `find /huge` and `tree /huge`, slow, once | | beside `ls /huge` from the navigation table |
| `find /`, slow, once | | seconds and the number of listings |
| `find /tree`, wan-bad, once | | seconds |

The smbclient-ng side is one row because its `tree` and `find` are the same walk (2.9) and a
run on all of `tree/` prints an `[error]` line for the denied directory; `tree/d0` is the
largest subtree without one. The implementing agent runs the mode once at the end of the stage
and puts the table into `docs/performance.md`.

## 7. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` runs after T0,
T3, T5 and T7. Documentation is written with the task that makes it true; `docs/commands.md` gets
a command's section in the task that adds the command, because H2 fails otherwise.

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T0 | Groundwork | — | `Args`, flags with a value, `opt`, `positional_index`; `&Args` through `run_line`, `run_parsed`, `dispatch`; the three changes of 5.3 in `walk` (the stop, the queue order, the parent below the root only) and `get_tree` adapted; `Out::diag`, `Out::written`; `entry_obj` (and `ls` on it); the fixture `links/` in `fixtures.sh`, its check in `check.sh`, its row in `docs/test-environment.md`, the image rebuilt (`down.sh`, `up.sh`); the harness additions of 6.1; the budget of 540 in `test.sh` | `flags_with_values`, `no_candidates_for_a_flag_value`, `out_written`, `walk_stops_when_told`, `walk_takes_results_in_queue_order`, `walk_root_parent_is_not_seeded`; **the full `./test.sh` here**, because every `get -r` test now walks in queue order and every server has a new directory |
| T1 | The pure parts | T0 | `Glob`, `parse_size`, `Filter`, `draw_tree`, `end_report`, `TREE_MAX` | the five `glob_*` tests, `size_parse`, `filter_rules`, `tree_drawing`, `end_report_rules` |
| T2 | `find` | T1 | `run_walk` (without the feed), `find`, its table entry and examples, the rerun rule and the tail in `run_parsed`, the JSON types, the section in `docs/commands.md` | F41 (the `find` half), F42, F43 (its `find` rows), F44, F45, F46, F47, F50 (the `find` half); H2 to H4, N4 |
| T3 | `tree` | T2 | `tree` with the cap, its table entry and examples, the notes, the count line, the section in `docs/commands.md`; the topic "tree and find" | F41, F43 to F46 and F50 complete; `table_is_complete`, `help_all_has_everything` |
| T4 | The cache | T2 | `Cache` units and index, `Cache::feed` with the collision rule, `Session::feed`, the feed in `run_walk` | `cache_units_and_order`, `cache_feed_collision`, the adjusted `cache_cap`; I41 |
| T5 | Latency, edge, cuts | T3, T4 | nothing new in `src/` unless a test shows a defect | S41 to S46, F49; X41, X44; X1, X2, F14 extended |
| T6 | Interrupts | T5 | nothing new in `src/` unless a test shows a defect | X42, X43 |
| T7 | Bench and documents | T6 | `bench-rsmb.sh walk`; the documents of section 8; the final full gate and its budget line | — |

If a count in F43, F45, S41, S42 or S43 differs from this plan on the first run, the
implementing agent finds out why before changing the number: the numbers come from section 2,
and a difference is a defect or a finding to record under "Deviations during implementation"
at the end of this file.

## 8. Documents

No new document. Extended, each in the task that makes it true:

| Document | What is added |
|---|---|
| `docs/commands.md` | sections ``## `tree` `` and ``## `find` `` (syntax, flags, depth, the pattern table and quoting with what differs from the server's and from Unix `find` (`İ`, the symmetric folding, `\` and a backwards range in a class, no `[:class:]`), the size rules (`B` and `c`, why `b` is refused), cost in requests, output in text and JSON, the order, what is reported at the end, the exit status beside `get -r`'s, errors, the examples); flags with a value under "flags inside commands"; completion (nothing for a flag's value); "Timeouts and reconnect": `tree` runs again, `find` only while it has printed nothing, and why; "Differences from smbclient-ng": paths from the share root and the start not printed, `-size` strict and in bytes, no `-mindepth`, `-ls`, `-download`, `--exclude-dir`, one start directory |
| `docs/scripting.md` | `denied`, `skipped`, `walk_summary` and `depth` with an example line each; the order of objects in a walk; the rule "no `walk_summary` means not whole"; that the order of `find`'s lines is the same on every run and what it is (breadth first); a worked example (find files by pattern, fetch them) |
| `docs/architecture.md` | `Args`; the walk's stop, its queue order with the window of 16 and what it costs, the parent seeded below the root only; `run_walk`, `find`, `tree` with what each holds in memory; the matcher; the end report; the cache: units, the ordered index, what a walk stores, the rule for two directories with one folded key; reruns (`Out::written`); "What Stage 4 adds where" in place of the Stage 3 section |
| `docs/testing.md` | the harness additions; the budget of 540 and the measured step times; the per-command counts (4 per listing, `chain/` 244 in 61, `deep/` 52 in 13, `fanout/` 8,004, `tree/` 1,368, `huge/` 19, `many/` 9, `loops/` 4); every new test with its row; what is not in the gate and why |
| `docs/performance.md` | the walk table of `bench-rsmb.sh walk` beside the wave bound, the sequential figure and smbclient-ng; the table of 2.1; memory of `find` and `tree`; the price of the queue order (nothing on `tree/` and `fanout/`, 7 % on the whole share behind `huge/`); "not done": a larger buffer for one very large directory in a walk (`huge/`: 15 flights against 1), `-mindepth` |
| `docs/measuring.md` | what `tree` and `find` cost in smbclient-ng (4 requests per directory, one after another) and how the one walk row of the bench is taken |
| `docs/known-limitations.md` | a section "tree and find": a walk waits behind one very slow listing once the 15 queued after it are done; `tree`'s limit of 500,000 entries; memory that grows with the number of directories; a directory with the file id of one already listed is not entered, and of two paths to one directory the first in breadth-first order is the one listed, which may be the link and not the real directory; a server that reuses ids across file systems loses the second directory that way; a link that leads out of the start's subtree is followed, so the walk covers its target too; with no `.` entry in a listing the start is listed once more through a link back to it; servers without file ids; no normalisation; `-iname` maps one character at a time (`ß` is not `SS`, `İ` is not `i`) and differs from the server's `find` for `İ`, `ı` and the sigmas; `[:class:]` is refused, no alternatives; `-size` takes `B` or `c` for bytes and refuses `b`; `-size` for files only; a very large directory costs one round trip per MiB inside a walk; `find` is not run again after a lost connection; a walk's listings in the cache are as old as the walk, and after a walk larger than the cache it holds what was listed last; two directories whose names differ only in case are not cached by a walk (and still share one entry under `ls` and Tab, as before); untested against Windows |
| `docs/test-environment.md` | the fixture `links/` (content, what it targets, how Samba presents it) and its `check.sh` line; the lock table: `bench-rsmb.sh walk` (shared); "Tests": the budget; that the containers' `find` is BusyBox and what it lacks |
| `docs/development.md` | a flag with a value in the table; where a new `find` test goes (`Filter`), with its unit test and its row in F42 |
| `docs/library-decision.md` | nothing unless the implementation meets a crate behaviour not yet recorded |
| `docs/reviews.md` | the Stage 3 section, after the review |
| `README.md` | status: Stage 3; two example lines (`tree -L 2`, `find -name`); the walk times beside smbclient-ng |
| `AGENTS.md` | the index row for this plan (added with the plan) and the changed rows; "Working commands": the budget of 540 s, `bench-rsmb.sh walk`; principles, **only those the work has shown** (candidates with evidence from section 2: compare a matcher with the server's own tool before writing tests around it; run the reference tool as the user the client logs in as; take results that arrive side by side in a fixed order when the outcome must not depend on timing, 2.10) |

## 9. What Stage 4 gets, and notes for it

- `tree` and `find` reach the server only through `Session::remote` and `Remote::list`, so they
  go through whatever the dialer connects; nothing above the dialer changes for a proxy.
- Row 4.1 re-runs `tests/fast.rs` through the proxy: F41 to F49 need `server_find` to take the
  container of the proxied server (`rsmb-samba-private` is built from the same image and has
  the same fixtures). The tests that count requests (F43, F45) need the tap, which cannot read
  a connection that starts with a SOCKS5 handshake (S4-1).
- Row 4.2's "one walk" is `find /tree`: 682 entries, one `denied`, exit 0.
- A reconnect inside `tree` (S44) goes through `ensure_connected`, the same place as every
  other reconnect; row 4.7 needs no walk of its own.
- `Args` is how a Stage 4 command would take a flag with a value, should one be needed.
- `samba-private` gets `links/` with the rebuilt image, like the other two servers.

## 10. Assumptions this plan could not verify

Marked so that the adversary can attack them. Each says what would show it wrong.

1. **File ids on other servers.** The loop check was measured on Samba 4.23.8 only. A Windows
   server gives a junction or a directory symlink the id of the reparse point, not of its
   target; whether a loop through one ends by id or only by the path limit was not seen.
2. **Two unrelated directories with one file id** (file systems mounted below one share): the
   second is reported as `already listed as` the first and not listed, though its content is
   different. No fixture can produce it (2.2); `links/` covers only two paths to one directory.
   It is a documented limitation.
3. **The walk's own memory per directory** (queue and visited set) is taken from the
   synthetic figure of about 230 bytes (2.5), measured on the structure `tree` keeps, not on
   `walk`'s map; the largest real walk had 2,471 directories.
4. **The numbers for 500,000 and 1,000,000 entries are synthetic.** `xl/` was not built; no
   walk of that size ran.
5. **BusyBox `find` on musl is the reference** for patterns. GNU `find` may differ at the
   edges (an unclosed `[`, `[^…]`); the tests use the 60 patterns that were compared. Where
   rsmb is known to differ from this reference (2.10) the difference is documented, not
   tested against the server: the image has no name with `İ`, `ı` or a sigma.
6. **The time bound of X41 holds with output.** The 4.82 s were measured for the walk alone;
   the product also writes 682 JSON lines to a pipe and, for `tree`, draws. The bound leaves
   19 %.
7. **`find`'s partial output is whole lines** after an interrupt: it rests on `Out` writing
   through a `BufWriter` that is flushed only between directories and on the future being
   dropped at an await point, never inside a write. X42 parses every line.
8. **The rerun of `tree`, and of `find` before its first line,** was measured through the
   probe's own second call on the session, not through `run_parsed`; S44 is the first run of
   the real path. That nothing reaches stdout before `find`'s first match rests on `Out`
   being the only writer to stdout (Stage 1 rule: nothing uses `println!`).
9. **smbclient-ng on all of `tree/` at the slow endpoint was not run** (extrapolated: 341 × 4
   round trips, about 273 s). The bench measures `tree/d0` (69.3 s measured).
10. **Peak outstanding on edge was exactly 16** in every walk measured, but transfers there
    saw 17 to 19 (Stage 2 I6); S43 asserts the credit balance, not the peak.
11. **wan-bad** was not walked while planning; S45 is expected to pass as navigation and small
    transfers did.
12. **A server whose listings have no `.` entry** leaves the start unseeded: a link back to
    the start is then entered once (its id is recorded on that visit and the second link is
    caught). Samba always sends `.`; the behaviour is reasoned from the code, not run.
13. **The progress line and stdout on a real terminal** (`find` printing between redraws) is
    the pattern `get -r` uses; it was not looked at with `find`'s much denser output.
14. **`Cache` with the ordered index** is assumed to make eviction logarithmic; the quadratic
    cost before was measured (2.6), the cost after is the unit test's bound.
15. **A closed stdout** reaches `find` as a write error, not a signal: Rust ignores SIGPIPE,
    and `cat … | head` is tested that way (Stage 2, F31). Measured with the probe after the
    review (2.10): 4 requests, 6 of 6. If `head` were slow to exit, the first listing's 36 KB
    would fit the pipe and the error would come one wave later (about 70 to 130 requests);
    S46's bound of 200 allows that.
16. **The image rebuild for `links/`** was not run: its time (the fixtures layer, without the
    large files) and that the three servers come up unchanged otherwise are expected, not
    measured. `check.sh` proves the rest of the fixtures after it.
17. **The queue order's worst case** is a walk that waits behind one very slow listing. Measured
    once (3.2 s behind `huge/`); a directory of a million entries at the slow endpoint would
    hold the walk for the time of its own listing. No fixture in the gate shows it.
18. **`links/` on `samba-edge` and through the 64 KiB listing** behaves as on `samba`: only
    `samba` was measured (2.10).

## 11. Adversary findings and what changed

One adversary attacked the first version of this plan on 2026-11-18. All eight findings were
validated by the orchestrating session and accepted. The numbers are the adversary's; what
this plan measured again afterwards is in 2.10.

| # | Finding | Evidence | What changed |
|---|---|---|---|
| 1 | **Which of two paths to one directory is listed depended on timing** (high to medium). With a link to a sibling, the walk listed whichever of the two listings' parents came back first, called the other "a link", and so printed different output from run to run; P4 ("the same on every run") was false, and the wording blamed the real directory | the adversary: `upload/adv3/a/link -> ../b/c`, both entries with file id 67540714; 5 of 6 runs reported the real `b/c` as the link. Again here (2.10): 7 of 10 one way, 3 the other, in completion order; 10 of 10 alike in queue order | **The walk takes results in the order the listings were queued** (5.3, D10; a window of 16, measured free on `tree/` and `fanout/`, 7 % on the whole share). `find`'s output is now the same on every run too (P3, no longer "not in Stage 3"). The skip line and the JSON reason say **`already listed as <path>`** / `already_listed`, never "link" (5.10, 5.14). New fixture `links/` in the image with its `check.sh` line (6.1), the unit test `walk_takes_results_in_queue_order`, F50 (ten runs each of `find` and `tree`, compared), a second run in S41, `/links/b/c` in X44. Whole-share counts grow by 4 listings and 6 entries; no existing test depends on them (6.1) |
| 2 | **F48 could not fail** (medium): `find /huge \| head` ends the same with and without the stop, because `huge/` holds no directory | the adversary's run; here `huge/` printed into `head` sent all its 19 requests (2.10) | F48 is gone. **S46**: `find /fanout \| head -n 5` through the tap at the slow endpoint, at most 200 requests of 8,004 (measured 4), a non-zero exit within 5 s; the harness gets `piped` (6.1) |
| 3 | **`-iname` was not what the server's `find` does** (low to medium), though the plan said so: `İ`/`istanbul`, `ı`/`INDEX`, `ς`/`Σ` differ, and so do `[\]]` and `[z-a]` | the adversary's comparison; repeated in 2.10 with the new rule | The rule is now **equal one-character lower-case forms or equal one-character upper-case forms** (5.6, P6): `ı`/`I` and `ς`/`Σ` match, `ß` still is not `SS`. The claim is corrected to "agrees on the 13 tested `-iname` patterns" (re-run with the new rule: 13 of 13), and the known differences (`İ`, the server's asymmetry, `\` and a backwards range in a class) are stated in 5.6 and go into `docs/commands.md` and `docs/known-limitations.md`. `glob_fold` and `glob_classes` have those names |
| 4 | **Two directories that differ only in case shared one cache key** (low): a walk stored both, the later won, and Tab showed the other directory's content | `cache_key` lowercases (`src/session.rs`); the adversary's `cs/Foo` and `cs/foo`; 2.10: this Samba serves the two with different content | `Cache::feed` keeps the folded keys one walk has stored; a second directory with the same key removes the entry and neither is stored (5.4, `cache_feed_collision`, I41). **`ls` and Tab have the same collision today**: it stays, as the limitation `docs/known-limitations.md` already states, because it needs the user to visit both and `ls` always asks |
| 5 | **`find` was never run again**, also when it had printed nothing (low) | `run_parsed` decides by `cmd.rerun` alone | `find` runs again once after a closed connection while `Out::written()` is false (5.8, S3-4, D5): the start's listing lost, or nothing matched yet. S44 has the third cut; `out_written` |
| 6 | **Three silent traps in the syntax** (low): `-name '[[:digit:]]*'` matched nothing without a word; `-size 10b` meant bytes where Unix means 512-byte blocks; `-size 10c` was refused | the plan's own tables in 5.6 and 5.7 | `[:` inside a class is a usage error; `c` is bytes; a lower-case `b` is a usage error that says why (5.6, 5.7, P5; `glob_classes`, `size_parse`, F42) |
| 7 | **A possible panic at the share root** (low): the walk seeds the id of the start's `..`; with an empty start and a server that gives the root's `..` a real id, printing "the start's parent" would slice `start[..start.len() - 1]` | `src/walk.rs`, the seeding of `first.parent` | The parent is seeded only when the start is not the share root (5.3, D10); `walk_root_parent_is_not_seeded` |
| 8 | **`WALK_FEED`, `feed_fits` and `feed_budget` served no requirement** (low, plainness) | AGENTS.md rule 5 | Dropped. Every listing goes through `Cache::put` and the cap evicts the oldest (5.4, P11). The ordered index stays (2.6 measured the quadratic cost), and so does counting a listing as `len + 1`, without which a walk over many empty directories would not be bounded by the cap at all |

Rejected: nothing.

## Deviations during implementation

Recorded by the implementing agent, 2026-11-18. Each with the evidence that made it necessary.
None relaxes a requirement of the brief.

| # | Plan | Done | Evidence |
|---|---|---|---|
| V1 | F47: `find /cjk -maxdepth 1 -name '東京　大阪*'` finds 114 | 112; the test compares with `server_find` with the same arguments | `docker exec -u tester -w /srv/share rsmb-samba find cjk -mindepth 1 -maxdepth 1 -name '東京　大阪*' \| wc -l` → `112`; without `-maxdepth 1` → `114` (the directory, 111 files, and the two `東京　大阪.txt` files one and nine levels down). The 114 of 2.7 is for all of `cjk/` |
| V2 | X44: `find / -maxdepth 3 -name self` reports `/links/b/c` → `/links/a/to-c` | `find / -maxdepth 4 -name self` (and `tree -L 3 /` as planned); `directories` = 1 + the server's directories of depth 1 to n − 1 − 3 for each | `/links/b/c` is at depth 3: by 5.5 a directory at depth `-maxdepth` is printed and not listed, so with 3 neither `links/a/to-c` nor `links/b/c` is listed and nothing is reported (the plan's own "in `tree -L 3` the last is cut by the depth" holds for `find -maxdepth 3` alike). Measured: `find / -maxdepth 4 -name self` 2,077 listings with all three `skipped`; `tree -L 3 /` 2,056 (2,053 of 2.1 plus `links`, `links/a`, `links/b`) with the two of `loops/` |
| V3 | `run_walk`'s `each(out, dir, depth, entries)` | `each(out, progress, dir, depth, entries)` | `find` must clear the progress line before it prints and only then; cleared by `run_walk` before every directory, the line would be erased right after each redraw and never be seen while `find` prints nothing |
| V4 | P10: "a flag given twice is a usage error" | for flags with a value (5.2's wording); a flag without a value given twice (`get -r -r`) is accepted as in Stage 2 | 5.2 and `flags_with_values` name only flags with a value; changing `get`/`put`/`mkdir` would change Stage 2 behaviour the plan did not ask to change |
| V5 | `cache_cap`'s numbers adjusted to units | also `cache_invalidate`: its last `total` is 2 (one listing of one entry) | the test asserted `total == 1` in entries; with D3 that listing costs 2 units |
| V6 | T2 (`find`) and T3 (`tree`) one after the other, `flags_with_values` in T0 | the two commands were built together, with their `docs/commands.md` sections; `flags_with_values` was written in T0 against stand-in table entries and switched to the real ones in T2 (same assertions); `no_candidates_for_a_flag_value` was written in T2, when `find` was in the table | no change to what is tested; `./test.sh fast` ran after T0, T2/T3, T4, and the full gate after T0, T5/T6 and T7 |
| V7 | Assumption 16: the image rebuild for `links/` not measured | measured | `docker compose build` 36.4 s (from the fixtures layer on; the large files' layer was reused), `down.sh` + `up.sh` 6 s; `check.sh` passed after it with the new `links/` line |
| V8 | S46: "at most 200 requests after the reader left" | computed as all requests − start-up (3) − `use share` (1) − the first listing (4) | measured 0 at the slow endpoint (8 requests in all), exit 1 after 2.1 s; the walk stopped before spawning the next 16 listings |
| V9 | `tree`'s note for a directory that failed for another reason: "the reason" | the error kind in words (`[other]`, `[invalid name]`) | the message carries the whole path again, which would double the line; the reason itself is printed at the end as the `error` with `path` |
| V10 | 6.5: the `walk` mode takes about 5 minutes | 363 s; smbclient-ng's requests are counted with the encrypted frames (`requests` + `opaque_requests`), as the navigation rows do | its first run printed 0 requests for smbclient-ng (342 flights): every request of smbclient-ng is encrypted, so only `opaque_requests` sees them; with both, 342 |
| V11 | 5.3, 5.10 (and F45, `end_report_rules`): a directory with the id of the start's parent is "already listed as" the parent's path; a directory is "seen" when queued | after the review (`docs/reviews.md`, Stage 3, findings 1 and 2): such a directory is reported with reason `outside` and no path; a second path to a queued id waits and is listed when the first path's listing fails | the start's parent is never listed, so the old text named a directory the walk had not listed (`tree /loops/self` named `/loops`; the real parent is the share root); a first path that failed dropped the directory, which exists, from the output |
