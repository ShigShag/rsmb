# Stage 6 plan: what may I do here? (`access`)

> Written on 2026-11-19 by a planning agent that read the repository and measured against the
> test environment, before any Stage 6 code exists. It follows the user's request as the
> orchestrator relayed it (a separate command that tells what one may do with a remote file or
> directory without trying it; `cd` and `ls` unchanged; all three uses served) and the draft design
> given with it; where it departs from that draft, section 4 says where and why. It is written for
> one implementing agent who was not present: everything needed to build Stage 6 in one pass is
> here or in a document this one names. **Revised on 2026-11-19 after the adversary's review and
> the user's answers to the plan's two questions:** section 11 lists the twelve findings, how each
> was checked and what changed; 2.13 holds what was measured for the revision; the user's two
> decisions are A14 and A7 (the three answers of `rmdir`). The text above section 11 is the revised
> plan. Section 10 lists what could not be verified.
>
> Words are used as in `docs/measuring.md` (request, flight, outstanding). **The crate** is `smb2`
> 0.43.0. **MxAc** is the SMB2 CREATE context `SMB2_CREATE_QUERY_MAXIMAL_ACCESS_REQUEST` (MS-SMB2
> 2.2.13.2.5) and its answer (2.2.14.2.5: `QueryStatus` u32, `MaximalAccess` u32): the server
> computes the session's maximal access to the object it opens and creates nothing. **The mask**
> is that `MaximalAccess`. **The probe** is one CREATE carrying MxAc and its CLOSE. **A chain** is
> one compound of several probes. **The parent** of a path is the directory it is in. The bits are
> named as in MS-DTYP 2.4.3 / MS-SMB2 2.2.13.1: on a directory 0x1 FILE_LIST_DIRECTORY, 0x2
> FILE_ADD_FILE, 0x4 FILE_ADD_SUBDIRECTORY, 0x40 FILE_DELETE_CHILD; on a file 0x1 FILE_READ_DATA,
> 0x2 FILE_WRITE_DATA; 0x80 FILE_READ_ATTRIBUTES, 0x1_0000 DELETE, 0x2_0000 READ_CONTROL,
> 0x10_0000 SYNCHRONIZE. In the tables a mask is also shown by short names: RD 0x1, WD 0x2, AD
> 0x4, REA 0x8, WEA 0x10, X 0x20, DC 0x40, RA 0x80, WA 0x100, DE 0x1_0000, RC 0x2_0000, WDAC
> 0x4_0000, WO 0x8_0000, S 0x10_0000. **POSIX** means the main Samba servers, which have no
> `acl_xattr` and derive NT rights from Unix modes; **NT ACLs** means the DC, which stores them
> (`vfs objects = dfs_samba4 acl_xattr xattr_tdb`).

## 1. Scope, what is not in it, exit criteria

**In scope:**

- A new command `access [-e] [<path>]` that shows, for a path or for every entry of a directory,
  which rsmb commands the server would allow: for a directory `ls`, `put` (a new file into it),
  `mkdir` (a directory in it), `rmdir` (remove it); for a file `get` (and `cat`), `put` (`put -f`
  over it), `rm`. Nothing is created, changed or tried. Text for a person, one `access` object per
  path with `--json` (the raw mask beside the named answers), an entry in the command table with
  examples, a section in `docs/commands.md`, Tab completion of the path.
- The answer is the server's own (MxAc), mapped to rsmb's commands by what those commands really
  send (section 2.7, decision A7), not by the obvious bit.
- `Remote::stat`, the existence check of a large `put`, asks for `FILE_READ_ATTRIBUTES` only
  (A14, decided by the user with every effect of 2.13 shown): measured while planning, a large
  `put -f` is refused today over a file that a small one replaces (2.11), and `put` onto a
  directory the user may not read reports the wrong error.
- Tests that compare the prediction with the real command, object by object, on POSIX and on NT
  ACLs, and that pin every known disagreement (2.7).

**Not in Stage 6:** showing ACLs or owners (the brief's "ACL display" stays out of scope; this
command shows a verdict, not a security descriptor), evaluating a security descriptor in the
client (A17), changing rights, several paths per command (A1), recursion below `-e`'s directory, a
`cd` column (A8), a progress line for a large `-e` (A11), any change to `cd` or `ls` (their
requests and behaviour stay exactly as they are, as the user decided), a new start-up flag, a new
dependency, a change to the test images.

**Exit criteria:** every row of section 7.4 passes in `./test.sh`; the full run stays within 600
s of counted time and `./test.sh fast` within 150 s; `help all` has the command with its flag and
three examples and the `access` JSON object; every document of section 9 matches the code; the
counts of `cd` and `ls` in `check_cd`, `check_ls`, `counts_add_up` and the walk tests are
unchanged; the review's confirmed findings are fixed and their tests are in the suite.

## 2. What was measured while planning

All on 2026-11-19 against the test environment as it is (seven containers, Samba 4.23.8, nothing
in `testenv/` changed), with throwaway files under `spikes/smb2/target/scratch-stage6/`
(git-ignored, not committed): `mxac/` (a Rust program on `smb2 = "=0.43.0"` that sends probes
alone, as chains, or one per compound with a bound on how many are in flight; through `smbtap`
when asked; with rsmb's own transport with `TCP_NODELAY`, direct or through the SOCKS5 proxy
with `tokio-socks`; a timer that cuts the tap's connections), `fixtures/` (the two fixture scripts
of section 6 as they were run, and `act.sh`, which runs one command line through the current
release binary and prints its exit code and error kind). Fixtures were made in
`/srv/share/upload/rsmb-plan6` and `/srv/share/upload/plan6` of `rsmb-samba` and `rsmb-samba-dc`
with `docker exec` and removed at the end. Kerberos caches came from `mkcache` and were deleted.
The shared lock was held for every run; nothing needed the exclusive lock. The release binary was
built from commit `0f7bd3f`.

### 2.1 What exists today

| What | Where | State |
|---|---|---|
| Share-level access | `Session::access`, `access_inner`, `probe_access`, `ACCESS_CHAIN = 16` (`src/session.rs`); the `shares` command | TREE_CONNECT + related TREE_DISCONNECT per share, 16 per compound, compounds in flight `credits / (2 × per compound)`. The value is share-level only; `docs/commands.md` says per-directory access is not shown |
| The compound open | `open_request(name, access)`, `open_close`, `create_close` (`src/session.rs`) | rsmb builds its own `CreateRequest` (with `create_contexts: vec![]`) and sends CREATE + CLOSE with `execute_compound` |
| `cd`'s check | `check_dir` → `open_close` with `CD_ACCESS` = `FILE_READ_ATTRIBUTES \| SYNCHRONIZE` and `FILE_DIRECTORY_FILE` | denies `forbidden/` on purpose (`docs/library-decision.md`, the cd row); untouched by this stage |
| `Remote::stat` | `open_close` with `CD_ACCESS`, options 0 | the check before a large `put` and the wording after a refused rename (2.11) |
| What the commands send | the crate: `read_open_request` (`get`, `cat`) `FILE_READ_DATA \| FILE_READ_ATTRIBUTES \| SYNCHRONIZE`; `write_open_request` / `write_file_compound_exclusive` (the temporary file of `put`) `FILE_WRITE_DATA \| FILE_WRITE_ATTRIBUTES \| SYNCHRONIZE`, create-only; `rename_with` `DELETE \| FILE_READ_ATTRIBUTES` on the temporary file, then FileRenameInformation (with replace for `-f`); `delete_compound_once` (`rm`, `rmdir`) `DELETE \| FILE_READ_ATTRIBUTES`; rsmb's `mkdir` create-only with `CD_ACCESS`; rsmb's listing `LIST_ACCESS` = `FILE_READ_DATA \| FILE_READ_ATTRIBUTES \| SYNCHRONIZE` | read from `~/.cargo/registry/src/*/smb2-0.43.0/src/client/tree.rs` and `src/session.rs` |
| The command table | `Cmd` with a fixed list of positional kinds; `parse` refuses more positionals than `args.len()` | no command takes a variable number of paths |
| smbclient-ng 3.1.0 | `acls` (`smbclientng/commands/acls.py`) | reads the security descriptor of each file and prints owner and ACEs with SIDs resolved over LSA: ACL display, not a verdict. Nothing like MxAc |

### 2.2 The crate: MxAc without a patch

`smb2::msg::create_context` is public: `CreateContext::new(name, data)`, `pack_contexts`,
`parse_contexts`, `find`. `CreateRequest::create_contexts` and `CreateResponse::create_contexts`
are the raw chain (`Vec<u8>`); the crate packs and unpacks the offsets itself. The probe built
`create_contexts: pack_contexts(&[CreateContext::new(b"MxAc", vec![])])` (no timestamp: DataLength
0, DataOffset 0, as the crate writes an empty context) and read the answer with
`parse_contexts(&resp.create_contexts)` and `find(.., b"MxAc")`: 8 bytes, `QueryStatus` then
`MaximalAccess`, little-endian. **Every successful CREATE of every run (more than 130,000 probes)
answered with the context and `QueryStatus` 0**, on all four servers (`samba`, `samba-edge`,
`samba-private` through the proxy, `samba-dc`), as NTLM, anonymous, Guest and Kerberos. No
library patch is needed.

### 2.3 Which desired access the probe asks for

CREATE + CLOSE with MxAc, `FileOpen`, create options 0 (a file or a directory), share access
read, write and delete, as `tester` on `share`:

| Path | 0 | `FILE_READ_ATTRIBUTES` | `READ_ATTRIBUTES \| SYNCHRONIZE` (`cd`'s mask) | `MAXIMUM_ALLOWED` |
|---|---|---|---|---|
| share root | 0x001200a9 | same | same | same |
| `upload` | 0x001f01ff | same | same | same |
| `traverse-only` (0711) | 0x001200a0 | same | same | same |
| `traverse-only/known.txt` | 0x00120089 | same | same | same |
| `forbidden` (0700 root) | 0x00000080 | same | **ACCESS_DENIED** | 0x00000080 |
| `forbidden/secret.txt` | ACCESS_DENIED | ACCESS_DENIED | ACCESS_DENIED | ACCESS_DENIED |
| `tree/unreadable.txt` (0600 root) | 0x00000080 | same | **ACCESS_DENIED** | 0x00000080 |
| `tree/d0/file.txt` | 0x00120089 | same | same | same |
| `nosuch`, `upload/nosuch` | OBJECT_NAME_NOT_FOUND | same | same | same |

The mask does not depend on the desired access. `cd`'s mask fails on the objects that grant
nothing but `FILE_READ_ATTRIBUTES` (Samba grants that whenever the path can be reached), and
those are exactly the objects whose mask can still hold `DELETE` (2.4: `secret.txt` is RA, DE).
Samba treats 0 and `MAXIMUM_ALLOWED` like `FILE_READ_ATTRIBUTES`, but how other servers treat an
open that asks for nothing, or for "whatever is granted" when nothing is, was not checked (section
10); `FILE_READ_ATTRIBUTES` is a named right, the one a client asks for to look at a file, so the
probe asks for it (A2). `forbidden/secret.txt` cannot be reached at all:
that is the answer.

### 2.4 What Samba answers on POSIX, object by object

The fixture of section 6.1 in a directory `S` owned by `tester` (0755) below `upload/`, as
`tester` on `share` (mask and, after it, the parent's mask where it matters):

| Object | Owner, mode | Mask | Short |
|---|---|---|---|
| `S` | tester 0755 | 0x001f01ff | RD WD AD REA WEA X DC RA WA DE RC WDAC WO S |
| `mine.txt` | tester 0644 | 0x0017019f | RD WD AD REA WEA RA WA DE RC WDAC S |
| `rootfile.txt` | root 0644 | 0x00130089 | RD REA RA DE RC S |
| `secret.txt`, `big.bin` | root 0600 | 0x00010080 | RA DE |
| `dosro.txt` | tester 0644, DOS read-only attribute | 0x00040089 | RD REA RA WDAC |
| `rodir` | root 0755 | 0x001300a9 | RD REA X RA DE RC S |
| `rodir/f.txt` | root 0644 | 0x00120089 | RD REA RA RC S |
| `rodir/ww.txt` | root 0666 | 0x0012019f | RD WD AD REA WEA RA WA RC S |
| `rodir/sub` | root 0755 | 0x001200a9 | RD REA X RA RC S |
| `rodir2` | root 0755 | 0x001300a9 | RD REA X RA DE RC S |
| `rodir2/minefile.txt` | tester 0644 | 0x0016019f | RD WD AD REA WEA RA WA RC WDAC S |
| `rodir2/minedir` | tester 0755 | 0x001f01ff | everything |
| `sticky` | root 1777 | 0x001f01ff | everything |
| `sticky/rootfile.txt` | root 0644 | 0x00120089 | RD REA RA RC S |
| `sticky/mine.txt` | tester 0644 | 0x0017019f | as `mine.txt` |
| `sticky/rootsub` | root 0755 | 0x001200a9 | RD REA X RA RC S |
| `sticky/minesub` | tester 0755 | 0x001f01ff | everything |
| `sticky/rwxsub` | root 0777 | 0x001f01ff | everything |
| `emptyroot` | root 0755, empty | 0x001300a9 | RD REA X RA DE RC S |
| `emptymine` | tester 0755, empty | 0x001f01ff | everything |
| `listonly` | root 0744 | 0x00130089 | RD REA RA DE RC S |
| `listonly/a.txt` | root 0644 | ACCESS_DENIED (no `x` on `listonly`) | |
| `writeonly` | root 0733 | 0x001301f6 | WD AD WEA X DC RA WA DE RC S |

Earlier probes, same server: a root file 0666 in `S` 0x0013019f; a tester file 0444 in `S`
0x00170089; a root 0777 directory in `S` 0x001f01ff; a tester file 0755 in `rodir2` and root
files 0777 in `rodir2` and in `sticky` **0x001e01ff (no DE)**; a tester directory in a tester
directory 0x001f01ff; the root of `tiny` (1777) 0x001f01ff.

What these show:

- **On a file, DE means "the directory lets you remove this name"**: present for root's files in
  `S` (tester's), absent in `rodir` (root's, 0755) and for root's files in `sticky` (the sticky
  rule), present for tester's in `sticky`. A file's own mode never gives DE (the 0755 and 0777
  files have none). Samba adds DE when it may delete the file in its directory, sticky bit
  included.
- **On a directory, DE also comes from the directory's own mode**: `rodir2/minedir` (tester's, in
  root's 0755 `rodir2`) and `sticky/rwxsub` (root's 0777, in root's sticky directory) both say DE,
  although neither can be removed (2.7).
- WD, AD and DC on a directory move together on POSIX (`w` maps to all three): FILE_ADD_FILE and
  FILE_ADD_SUBDIRECTORY can only differ under NT ACLs (2.6).
- The DOS read-only attribute removes DE and the write bits (`dosro.txt`).

### 2.5 Share-level restrictions, logins, encryption

| Case | Measured |
|---|---|
| `readonly` (`read only = yes`, the same directory as `share`) | share root 0x000000a9, `upload` 0x000c00a9, `S` 0x000c00a9, `S/mine.txt` 0x00040089, `S/rootfile.txt` 0x00000089, `tree/d0/file.txt` 0x00000089, `traverse-only` 0x000000a0, a tester directory without `r` (0311) 0x000400a0. **Every write bit and DE are gone: the server already applies `read only`.** RC and S are gone too (Samba removes FILE_GENERIC_WRITE, which holds READ_CONTROL and SYNCHRONIZE), yet `cd`, `ls` and `get`, which ask for SYNCHRONIZE, all work on `readonly` |
| The tree's own mask (TREE_CONNECT) | `share` 0x001f01ff, `readonly` **0x001f00a9 (with DE)**, `guest` 0x001f00a9, `encrypted`, `tiny` 0x001f01ff. ANDing a path's mask with the tree's would not remove DE on `readonly`, and is not needed: the path's mask is already narrower. `Tree::connect` drops the value anyway (Stage 4's note) |
| `encrypted` (`smb encrypt = required`) | the same masks as `share` for every path tried (`upload`, `S`, `S/mine.txt`, `S/rootfile.txt`, `S/sticky`, `tree/d0/file.txt`, `traverse-only`) |
| `guest`, as anonymous, as `-u Guest --no-pass`, as `tester` | root 0x000000a9, `hello.txt` 0x00000089 for all three; `MAXIMUM_ALLOWED` gives the same |
| `share` as anonymous | the TREE_CONNECT is refused (ACCESS_DENIED): `use share` fails before any probe |
| Real commands on `readonly` (S fixture) | `cd`, `ls`, `get` work; `put`, `mkdir`, `rm`, `put -f`, `rmdir` of an empty tester directory are refused, all `access_denied` |
| Real commands on `guest` | anonymous: `ls /`, `get hello.txt` work, `put`, `mkdir` refused; Guest: `ls /` works, `put` refused |

### 2.6 NT ACLs at the DC, NTLM and Kerberos

The fixture of section 6.2, made with `smbcacls //127.0.0.1/share … -U
'RSMB\Administrator%Adm1n-testpass' --set …` inside `rsmb-samba-dc` (smbd serves the change at
once; `xattr_tdb` keeps the ACL in a file, so no privilege is needed; 1.9 s for the whole
fixture). The scratch directory is root's with mode 1777, as `Scratch::new` makes it on the DC.
As `tester` with Kerberos (`mkcache -f tester cifs/10.213.45.12`) and, in an earlier run, with
NTLM (`RSMB\tester`): **the masks are the same with both logins** (seven objects compared).

| Object | ACL for tester (granted mask) | Mask |
|---|---|---|
| `A` | 0x001200ab (list, add file), nothing inherited | 0x001200ab RD WD REA X RA RC S |
| `C` | 0x001200ad (list, add subdirectory), CREATOR OWNER inherited | 0x001200ad RD AD REA X RA RC S |
| `D` | 0x001200e9 (list, delete child) | 0x001200e9 RD REA X DC RA RC S |
| `D/file.txt` | read 0x00120089 | 0x00130089 (DE from `D`'s DELETE_CHILD) |
| `D/denydel.txt` | DENY DELETE, then read | 0x00130089 (DE all the same) |
| `E` | 0x001200af (list, add file, add subdirectory; no delete child), CREATOR OWNER | 0x001200af |
| `E/file.txt` | read and DELETE 0x00130089 | 0x00130089 |
| `E/empty` | 0x001200a9 | 0x001200a9 (no DE) |
| `F` | 0x001200ab, CREATOR OWNER | 0x001200ab |
| `F/file.txt` | read | 0x00120089 (no DE) |
| `F/dosro.txt` | full control, DOS read-only | 0x000c00e9 RD REA X DC RA WDAC WO (no DE, no WD) |
| `G` | `Domain Users` 0x001200ab (a group, not tester) | 0x001200ab |
| `H` | 0x001200a9 | 0x001200a9 |
| `H/nosync.txt`, `H/nosyncdir` | 0x00000081 (read data, attributes; **no SYNCHRONIZE**) | 0x00000081 |
| `I` | 0x001200a9 (read only) | 0x001200a9 |
| `I/sub`, `I/own.txt` | full control | 0x001f01ff |

With a parent of mode 0777 instead of 1777 the directories `A`, `C`, `D` also had DE (0x001300ab,
…): the DC's NT ACLs and the POSIX rules of the directory above both count.

**Why rsmb does not read the security descriptor and decide itself** (the alternative the
orchestrator named): `G` grants through the group `Domain Users`. An NTLM login gives the client no
SID at all, and the Kerberos PAC is encrypted for the server, so the client cannot know that
tester is in that group. The `readonly` share (2.5) refuses every write by its configuration,
which no descriptor holds. The DOS read-only attribute, the sticky bit and Samba's POSIX mapping
(2.4) are not in the descriptor either. And reading descriptors costs a QUERY_INFO per object plus
LSA lookups to name SIDs. The server evaluates all of it in one answer (A17).

### 2.7 Prediction against reality

Every command of the tables below was run with the release binary of `0f7bd3f` in one session per
fixture (`--json --keep-going`, one `-C` per line, `lcd` to a directory holding `notes.txt` of 6
bytes and `big.bin` of 300,000 bytes), after the masks above were taken, in this order, so that no
action changes an object another one acts on (`rm` acts on a copy `rm-<name>` made like the
original). "Predicted" is the rule of A7 applied to the masks above.

**POSIX** (`share`, scratch `S`):

| Line | Predicted | Real |
|---|---|---|
| `ls .`, `put notes.txt ./`, `mkdir new` | allowed | ok |
| `get mine.txt dl/`, `put -f notes.txt mine.txt`, `rm rm-mine.txt` | allowed | ok |
| `get rootfile.txt dl/`, `put -f notes.txt rootfile.txt`, `rm rm-rootfile.txt` | allowed | ok |
| `get secret.txt dl/` | refused | access_denied |
| `put -f notes.txt secret.txt`, `rm rm-secret.txt` | allowed | ok |
| **`put -f big.bin big.bin`** (300,000 bytes, root 0600) | allowed | **access_denied** (K6: the check of a large `put` asks for SYNCHRONIZE; 2.11) |
| `get dosro.txt dl/` | allowed | ok |
| **`put -f notes.txt dosro.txt`** | **refused** | **ok** (K1) |
| `rm rm-dosro.txt` | refused | other (`cannot be deleted (read-only?)`) |
| `ls rodir` | allowed | ok |
| `put notes.txt rodir/`, `mkdir rodir/new` | refused | access_denied |
| `get rodir/f.txt dl/` | allowed | ok |
| `put -f notes.txt rodir/f.txt`, `rm rodir/rm-f.txt`, `put -f notes.txt rodir/ww.txt`, `rmdir rodir/sub` | refused | access_denied (`ww.txt` has WD, the directory has not) |
| `put -f notes.txt rodir2/minefile.txt`, `rm rodir2/rm-minefile.txt` | refused | access_denied |
| `rmdir rodir2/minedir` | unknown (`?`, A7: DELETE, but `rodir2` lets nothing change) | access_denied (with DE alone it would have said allowed) |
| `ls sticky`, `put notes.txt sticky/`, `mkdir sticky/new` | allowed | ok |
| `put -f notes.txt sticky/rootfile.txt`, `rm sticky/rm-rootfile.txt` | refused | access_denied (with "DELETE or the parent's DELETE_CHILD" it would have said allowed: `sticky` has DC) |
| `put -f notes.txt sticky/mine.txt`, `rm sticky/rm-mine.txt` | allowed | ok |
| `rmdir sticky/rootsub` | refused | access_denied |
| `rmdir sticky/minesub` | allowed | ok |
| **`rmdir sticky/rwxsub`** | **allowed** | **access_denied** (K3) |
| `ls emptyroot`, `rmdir emptyroot`, `rmdir emptymine` | allowed | ok |
| `put notes.txt emptyroot/`, `mkdir emptyroot/new` | refused | access_denied |
| `ls listonly` | allowed | ok |
| `get listonly/a.txt dl/` | unknown (`?`: the probe was refused) | access_denied |
| `ls writeonly` | refused | access_denied |
| `put notes.txt writeonly/`, `mkdir writeonly/new` | allowed | ok |

Also measured with the same binary (earlier fixtures): `put notes.txt rodir2/minedir/` and `mkdir
rodir2/minedir/new` ok (predicted allowed); on `traverse-only` `cd` ok, `ls`, `put`, `rm
known.txt` refused, `get known.txt` ok (predicted `- - - -` for the directory and `get - -` for
the file); share root `ls` ok, `put`, `mkdir` refused; `tree/unreadable.txt` `get`, `rm` refused.

**NT ACLs** (DC, Kerberos, scratch 1777):

| Line | Predicted | Real |
|---|---|---|
| `put notes.txt A/` | allowed | ok (nothing inherited: the creator still writes and renames its file) |
| `mkdir A/new`, `put notes.txt C/` | refused | access_denied |
| `mkdir C/new` | allowed | ok |
| `get D/file.txt dl/`, `rm D/rm-file.txt`, `rm D/denydel.txt` | allowed | ok |
| `put -f notes.txt D/file.txt` | refused | access_denied |
| `put -f notes.txt E/file.txt`, `rm E/rm-file.txt` | allowed | ok |
| `rmdir E/empty` | refused | access_denied |
| **`put -f notes.txt F/file.txt`** | **refused** | **ok** (K2) |
| `rm F/rm-file.txt` | refused | access_denied |
| **`put -f notes.txt F/dosro.txt`** | **refused** | **ok** (K1; not in the gate: finding 1) |
| `rm F/rm-dosro.txt` | refused | other (not in the gate: finding 1) |
| `put notes.txt G/` | allowed | ok (through the group) |
| `mkdir G/new` | refused | access_denied |
| **`put notes.txt J/`** (measured for the revision, 2.13) | **allowed** | **access_denied**, the part file left behind (K7) |
| `mkdir J/new` | refused | access_denied |
| **`get H/nosync.txt dl/`, `ls H/nosyncdir`** | **allowed** | **access_denied** (K4) |
| `rmdir I/sub` | unknown (`?`, A7: DELETE, but `I` lets nothing change) | ok (K5 of the first plan; the user's rule shows `?`) |
| `rm I/rm-own.txt` | allowed | ok |
| `put -f notes.txt I/own.txt` | refused | access_denied |

**Every disagreement, and why:**

| # | Case | Predicted → real | Cause | Direction |
|---|---|---|---|---|
| K1 | a file with the DOS read-only attribute in a directory that takes files (`dosro.txt`, `F/dosro.txt`) | `put` refused → `put -f` replaces it | Samba's rename with replace does not look at the target's attribute or at DELETE on the target; the mask has no DE | safe: says no where yes |
| K2 | an NT ACL file without DELETE in a directory with FILE_ADD_FILE (`F/file.txt`) | `put` refused → replaced | the same: Samba does not check DELETE on the replaced file. Windows is expected to (not verified, section 10), so the rule keeps it | safe |
| K3 | a POSIX directory that is rwx for you, owned by someone else, in a sticky directory (`sticky/rwxsub`) | `rmdir` allowed → refused | the directory's own mode gives DE (2.4); the sticky bit is not visible in any mask (the sticky directory has WD, AD, DC like any writable one) | **unsafe: says yes where no** |
| K4 | an NT ACE that grants READ_DATA without SYNCHRONIZE (`H/nosync.txt`, `H/nosyncdir`) | `get`, `ls` allowed → refused | rsmb and the crate always ask for SYNCHRONIZE; the rule cannot require S because every mask on a `read only` share lacks it (2.5) while those opens work there | **unsafe**, made by hand with `smbcacls`; Windows' ACL editor adds SYNCHRONIZE to every allow entry it writes (not verified) |
| (K5) | an NT ACL directory with DELETE in a directory that lets you change nothing (`I/sub`) | the first plan said `rmdir` refused → removed | POSIX refuses this case (`rodir2/minedir`), NT ACLs allow it, and no mask tells the two apart | **no longer a disagreement**: the user's rule (A7) shows `?` |
| K6 | a large `put -f` over a file you may replace but not open with SYNCHRONIZE (`big.bin`) | `put` allowed → refused | rsmb's own check (2.11) | fixed by A14; afterwards it agrees |
| K7 | a drop box: a directory with FILE_ADD_FILE whose inheritable ACL gives the creator of a new file no DELETE (`J`, 2.13) | `put` allowed → refused at the rename, the temporary file left behind | `put` renames its temporary file, which needs DELETE on it; the inherited ACL is not in the directory's mask | **unsafe**, and an ordinary ACL |
| (K8) | a directory with the DOS read-only attribute (`rodos`, 2.13) | the mask drops WD, AD and DE, yet `put`, `mkdir` in it and `put -f` over its files work | Samba removes the bits for the attribute and does not enforce it on directories | **not a disagreement**: A7 shows `?` for those columns when the directory's CREATE answer has FILE_ATTRIBUTE_READONLY |

**What is not an access question and is not shown:** a directory that is not empty (`rmdir`
fails `not_empty`), a target that exists for `put` without `-f`, a file open elsewhere (sharing
violation), a full disk or a quota, rsmb's own refusals (`rmdir` of the working directory or one
above it, names of the temporary form, names SMB does not allow), and a name the server lists
but will not open for data: in `mangled/` all nine mangled names open for attributes (masks
0x00120089), but four of them answer a data open with OBJECT_NAME_INVALID (`cat
mangled/LUI9XK~T.TXT`: `invalid_name`), so `access -e /mangled` shows `get` for all nine.

### 2.8 One compound or two, and the cascade

**Two probes in one compound are faster through the proxy.** The path and its parent, as one
compound of 4 requests or as two compounds of 2 sent together, median of 7 at the slow endpoint
(100 ms each way), `TCP_NODELAY` on the client's socket:

| Route | One compound | Two compounds |
|---|---|---|
| direct to `wan` | 202 ms | 202 ms |
| proxy → `wan` | 202 ms | **242 or 403 ms** (bimodal: in three runs of 7 the medians were 243.0, 243.5 and 403.1 ms; repeated for the revision: 2 or 3 of 7 rounds near 243 ms, the rest near 403 ms; one compound 201.9 to 202.9 ms every round) |
| proxy → `samba` (tapped: 4 requests, 1 flight either way) | 0.9 ms | **41 ms** |

Five independent compounds: 204 ms direct, 405 ms through the proxy to `wan`, every time (3 of
3). microsocks does not set `TCP_NODELAY` on its socket to the server (`docs/test-environment.md`),
so the second small write waits for the first's acknowledgement.

**A failed CREATE in a chain fails everything after it.** The crate marks every request after the
first of a compound as related. A chain of seven probes `upload`, `nosuch`,
`forbidden/secret.txt`, `tree`, `tree/d0/file.txt`, `nosuch2`, `many` got: `upload` a mask, then
OBJECT_NAME_NOT_FOUND for **all six others**, `tree` and `many` included, which exist. The same
seven as seven compounds: four masks, one ACCESS_DENIED, two OBJECT_NAME_NOT_FOUND. A chain of
existing paths answers every probe (6 of 6). A shares chain does not cascade (Stage 4: a refused
TREE_CONNECT changes nothing for the others); a CREATE chain does. So the answers of a chain are
**trusted up to and including the first failed CREATE**; the paths after it are asked again.

### 2.9 Requests, flights and time

Through `smbtap` (requests / flights) and wall time of the probes alone (the listing of `-e` comes
on top: 5 requests, 1 flight, as `ls`). "Chained" is the design of A5/A6: up to 16 paths per
compound, at most min(credits / 2, 256) paths in flight, after a failure the rest of its chain one
path per compound.

| What | fast | slow | proxy → `samba` (tapped) | proxy → `wan` |
|---|---|---|---|---|
| a path and its parent, one compound | 4 / 1, 0.8 ms | 202 ms | 4 / 1, 0.9 ms | 202 ms |
| `tree` and its 6 entries plus one missing name (a failure in the chain) | 20 / 2, 4.8 ms | 406 ms | 20 / 2, 45 ms | 608 ms |
| `fanout/` 2,000 entries, chained | 4,000 requests, 441 ms | 2,091 ms | 4,000, 412 ms | 2,437 ms |
| the same, one probe per compound, 256 in flight | 4,000, 444 ms | 2,254 ms | | 2,412 ms |
| the same, one probe per compound, 16 in flight | 4,000, 474 ms | **25,453 ms** | | |
| `many/` 10,000 entries, chained | 20,000, 2,200 ms (2,082 to 2,110 in three more runs) | 8,494 ms | | |
| `huge/` 100,000 entries, chained | 200,000, 20,879 ms | not run (about 80 s computed: 391 waves of 256 at 200 ms) | | |
| `samba-edge` (16 credits) `fanout/`, chained: 8 per compound, 1 in flight | 4,000, 508 ms, peak 16 outstanding | not run (about 50 s computed: 250 round trips) | | |
| `samba-edge`, one per compound, 256 asked for | 4,000, 527 ms, peak 16 | | | |

The server's time per probe is about 0.21 ms (fast endpoint, any shape), so a large directory is
bounded by the server on the fast link and by the width at 200 ms; 256 paths in flight (512
requests) is where the slow link stops being the limit. The credits: 513 after login on the test
server, 8,190 to 8,192 after a large run; the crate waits for credits it has not been granted
(edge: never more than 16 outstanding, also when 256 were asked for).

### 2.10 A cut in the middle

`many/` at the slow endpoint, 256 in flight, the tap's connections dropped after 2,000 ms: 1,572
probes had answered, **8,428 failed at once** with the crate's `Disconnected from server` (the
whole run 2,007 ms): nothing waits for a deadline. The crate's kind for it is `ConnectionLost`,
which `classify` makes `connection_lost { silent: false }`, so the existing rule applies: a
command that has printed nothing runs once more on a new connection. An interrupt and a silent
server were not tried with the probe; they follow the existing mechanisms (`in_flight` and
`abandon`, the deadline), see 5.6.

### 2.11 Found on the way

- **`Remote::stat` asks for SYNCHRONIZE.** A `put -f` of more than 256 KiB checks the target first
  with `Remote::stat`, which opens it with `CD_ACCESS`. Over `big.bin` (root, 0600, in tester's
  directory: mask RA, DE) that open is refused, so `put -f big.bin big.bin` fails with
  `access_denied` before sending anything, while `put -f notes.txt secret.txt` (the same mode, the
  small path, no check) replaces the file. `FILE_READ_ATTRIBUTES` alone opens it (2.3). A14 fixes
  this; `cd` keeps its mask. Its other effects are in 2.13.
- **`docs/test-environment.md`, `mangled/`:** "the four whose original contains a DOS wildcard
  character cannot be opened by their mangled name: `NT_STATUS_OBJECT_NAME_NOT_FOUND`" was measured
  with `smbclient`. Over rsmb's own CREATE all nine open for attributes; the four answer a data
  open with OBJECT_NAME_INVALID (`cat` reports `invalid_name`). Not changed by this stage; the
  implementing agent adds the sentence to that document (section 9).

### 2.12 smbclient-ng

smbclient-ng 3.1.0 has `acls`, which reads each object's security descriptor and prints it with
names resolved over LSA. It does not answer "may I", and it is the "ACL display" the brief keeps
out of scope. Nothing was timed: the two commands do not do the same thing.

### 2.13 Measured for the revision

On 2026-11-19, after the adversary's review, each finding was reproduced with the same probe, the
release binary of `0f7bd3f` and the adversary's throwaway build of A14 (the check of `Remote::stat`
changed as 5.10 says, nothing else), in directories of their own below `upload/` that were removed
afterwards; every attribute set on the DC was cleared over SMB before its file was removed.

**DOS attributes on the DC outlive their file** (finding 1). The DC keeps DOS attributes in
`/var/lib/samba/xattr.tdb` (`xattr_tdb`), keyed by the file's inode. A file made with `docker
exec` (inode 18889957), set `+r` over SMB, then removed with `rm -f` outside Samba: the next file
made the same way got inode 18889957 and `allinfo` showed `attributes: R (1)`. Samba's own rename
with replace leaks the same way: `old.txt` set `+r`, `rename new.txt old.txt -f`, then the next
file made outside Samba took the old inode and was listed `R`. A file or directory created through
SMB gets a fresh record (every `put` and `mkdir` of the planning runs showed `A` or `D`). Clearing
works over SMB: `setmode <path> -rsh` as `RSMB\Administrator` turns a stale `R` or `DR` into `N`
or `D`, also on nested paths (`setmode "D/file.txt" -rsh`), for 2 objects in 0.05 s; `setmode .`
is refused (OBJECT_NAME_INVALID), so a directory is cleared from its parent. **The main servers do
not leak**: they keep the attribute on the file itself (a `+r` file removed with `rm -f`, the next
file on the same inode showed attribute 0x80). Consequences: the NT fixture writes a fresh
attribute on every object it makes outside Samba; `Scratch` on the DC does the same for its
directory and clears every attribute in it before removing it; the gate does not replace a DOS
read-only file on the DC (K1 stays pinned on POSIX only). An exact-mask test would otherwise fail
at random, by inode reuse, and a stale `R` would refuse `rm`.

**A drop box refuses `put`** (finding 2). On the DC a directory `J` with `RSMB\tester:ALLOWED/0x0/
0x001200ab` (list, add file) and the inheritable `RSMB\tester:ALLOWED/OI|IO/READ`, no CREATOR OWNER:
mask 0x001200ab (WD), so the rule says `put`. `put notes.txt J/` fails with `access_denied` and
`left_behind: [".../J/.rsmb-79cea06bdae3274c.part"]`: the temporary file was written, and the
rename, which opens it with `DELETE | FILE_READ_ATTRIBUTES`, is refused by the ACL the file
inherited. On disk the part file stays (`-r-xrwx---`, the DC's id for tester). This is K7: an
allowed column that is wrong for an ordinary ACL.

**A directory's DOS read-only attribute is not enforced** (finding 3). A tester directory `rodos`
on `share` (POSIX) with `setmode +r`: its CREATE answer has file attributes **0x11** (directory,
read-only) and mask 0x000c00e9 (RD REA X DC RA WDAC WO: no WD, AD, DE). Yet `put notes.txt rodos/`,
`mkdir rodos/x`, `put -f notes.txt rodos/f.txt` (its file: mask 0x0017019f), `rmdir rodos/sub`
(mask 0x001f01ff) and `rm rodos/notes.txt` all succeed. Only `rmdir` of a read-only directory is
refused (`rodos2`: `cannot be deleted (read-only?)`), which the mask's missing DE predicts. The
attribute is in the probe's CREATE answer, so A7 can show `?` where it hides the bits.

**A14 changes more of `put` than K6** (finding 4). The release binary against the A14 build, same
fixture, fast endpoint unless said:

| Line | Before A14 | With A14 |
|---|---|---|
| `put -f big.bin big.bin` (300,000 bytes over root's 0600 file in tester's directory) | `access_denied` | ok (K6 fixed) |
| `put big.bin secret.txt` (no `-f`, root's 0600 file) | `access_denied` | `already_exists`: `exists on the server; -f replaces it` |
| `put notes.txt rootdir700` (root's 0700 directory; the small path) | `already_exists`: `exists on the server; -f replaces it` (wrong: it is a directory; with `-f` the rename is refused) | `is_a_directory`: `is a directory; write '…/rootdir700/' to put the file into it` |
| `put big.bin rootdir700` | `access_denied` | `is_a_directory`, the same message |
| `put -f` of 50 MiB over root's 0600 file in a sticky directory (not replaceable), slow endpoint, whole run with login | refused by the check before any data: `access_denied`, 1,430 ms | all 50 MiB uploaded, then the rename refused: `the server refused to replace it (open in another program, read-only, or not permitted)`, 7,287 ms; the temporary file removed, the target untouched |

The small path's wording changes because a refused rename asks `stat` whether the target is a
directory, and `stat` could not open a directory the user may not read. The user accepted all of
it with A14.

**A chain cut in the middle, then answered** (finding 5). Samba lists a FIFO and refuses to open
it: a tester directory `mixed` with `a.txt`, `b.fifo` (`mkfifo`), `c.txt`, `d/`; `ls` shows all
four; the probe of `b.fifo` answers 0xC0000279 (rsmb's kind `other`). The chain `mixed`, `a.txt`,
`b.fifo`, `c.txt`, `d` answered three (the third a failure), and `c.txt` and `d` sent alone
answered with masks: 14 requests in 2 flights, plus the listing 19 in 3. This is the case the gate
was missing: a failure in the middle with answers after it, so a wrong index or a skipped retry
shows.

**Through the proxy** (finding 10): 2.8's two-compound figure is bimodal, 242 or 403 ms (2.8).

## 3. Decisions

| # | Decision | Why |
|---|---|---|
| A1 | **`access [-e] [<path>]`**: one path or none (the working directory); with `-e` the path is a directory and every entry of it is shown instead of the directory itself | Every rsmb command takes one path, and the command table has a fixed list of positionals (`parse` refuses more); a variable list would be a new mechanism for one command (rule 5). `-e` ("entries") answers "which of these may I touch" in two round trips. Name: `access` matches the `shares` column ("your access"), says what is asked, and is free |
| A2 | The probe: CREATE with `FILE_READ_ATTRIBUTES`, `FileOpen`, create options 0, share access read, write and delete, no oplock, the MxAc context with no data; then a related CLOSE | 2.3: the one mask that opens everything reachable, so the mask (which can hold DELETE) is never lost to a refused open; options 0 serve files and directories alike. No library patch (2.2) |
| A3 | The mask is used as the server sends it, not ANDed with the tree's | 2.5: Samba already applies `read only`; the tree's mask of a read-only share holds DELETE, so ANDing would remove nothing; and it is not available (`Tree::connect` drops it) |
| A4 | The parent is asked with the path, **first, in the same compound** (4 requests, 1 flight; the share root alone 2) | 2.8: one compound costs one round trip through the proxy, two cost two. The parent is needed for a file's `put` and for a directory's `rmdir` (A7), and the type is not known before the answer. Parent first because a missing path is the common failure and then the parent's answer is already in hand; with the path first, its failure would cascade to the parent |
| A5 | **The cascade**: a chain's answers count up to its first failed CREATE; that failure itself counts only when it is the chain's first CREATE or the CLOSE before it succeeded (otherwise it may be the CLOSE's status passed on, and it is asked again too); every path after it is sent again, each in a compound of its own, and its answer goes to its own place | 2.8: Samba fails everything after a failed CREATE with that status, and a related request after a failed one gets the earlier status (finding 6). A path sent alone cannot be cut again, so a failure costs at most one more round trip. 2.13: `mixed/` shows the retried answers |
| A6 | **Up to 16 paths per compound** (`ACCESS_CHAIN`, as `shares`), **at most min(credits / 2, 256) paths in flight** (`PROBE_LIMIT = 256`), credits read when the command starts; the per-compound count is also at most that limit (8 on edge) | 2.9: 256 in flight is where 200 ms stop dominating (2,000 entries in 2.1 s against 25 s at 16); on edge it gives 8 per compound, one compound at a time, as `shares` does there. One bound in paths, not in compounds, so the single-path retries of A5 run 256 wide too |
| A7 | **From mask to commands** (table below) | 2.7: every rule follows what the command sends; each departure from the obvious bit has its measurement |
| A8 | **No `cd` column** | No bit predicts `cd`: its mask needs SYNCHRONIZE, which every mask on a `read only` share lacks while `cd` works there (2.5), and `forbidden/` (mask RA only) refuses it. Trying `cd` changes nothing and costs 2 requests, less than asking (4) |
| A9 | Text: one line per path, `d` or `-`, four fixed columns, the name (5.5) | aligned for a person, one word per command, `-` refused, `?` unknown |
| A10 | `--json`: one `access` object per path with `mask` (integer or null), `attributes` (the CREATE answer's file attributes, integer or null), `error` (kind or null) and the commands of its type as `true`, `false` or `null`: a directory `ls`, `put`, `mkdir`, `rmdir`; a file `get`, **`replace`**, `rm` (5.5) | the raw mask for whoever wants more, named answers for whoever does not. A file's column is `replace`, not `put`, so that one key never means two things (finding 11): `put` is "a new file into this directory", `replace` is "`put -f` over this file". The text keeps the command word `put` in both, with the manual saying which |
| A11 | `-e` lists the directory like `ls` (always asks, sorted by name, refreshes the cache Tab reads), then asks for the directory and every entry; an entry whose probe fails is shown with `?` and its error kind and does not fail the command. Nothing is printed before the last answer and there is no progress line; the documents say so with the measured times (`-e /huge` 20.9 s at the fast endpoint), and Ctrl-C ends it | consistent with `ls` (one more round trip, no stale names); an entry the server will not open is an answer about that entry (as `tree` reports a denied directory without failing). A progress line would be a second output path for one command; printing only at the end is what lets the command run again after a cut (A13), and `ls /huge` and `tree` are silent the same way (finding 12) |
| A12 | Without `-e`, the path's own failure is the command's error: `not_found`, `access_denied` (not even its attributes can be read), `invalid_name`; `usage` without a share; with `-e` also `not_a_directory` (the listing's). So the same path is exit 1 alone and an object with `error` and exit 0 as an entry of `-e`; `docs/scripting.md` and the manual say so (finding 11) | as `ls <path>`: a path that cannot be reached has no columns |
| A13 | `rerun: true` | it prints only after its last answer; a closed connection runs it once more (2.10). Interrupt and silence as for every command (5.6) |
| A14 | **`Remote::stat` asks for `FILE_READ_ATTRIBUTES` only. Decided by the user** (2026-11-19), with every effect of 2.13 shown: a large `put -f` over a file one may replace but not read works (K6); `put` without `-f` over such a file says `already_exists` instead of `access_denied`; `put` onto a directory one may not read says `is_a_directory` on both paths (the small path said `already_exists`, wrongly); a large `put -f` over a file that cannot be replaced uploads everything and is then refused by the rename (7.3 s for 50 MiB at the slow endpoint instead of 1.4 s), with the clearer message of the rename and the temporary file removed | 2.11, 2.13: a check should not ask for more than it needs. Same requests and flights; `cd` keeps `CD_ACCESS` |
| A15 | **No change to the test images**: the tests make their fixtures at run time in their scratch directories with `docker exec` as root (POSIX) and `smbcacls` inside the DC (NT ACLs). **On the DC every object made outside Samba gets a fresh DOS attribute over SMB** (`setmode -rsh`, the fixture's own objects and `Scratch`'s directory), `Scratch` clears the attributes of everything in its directory over SMB before removing it, and no test replaces a DOS read-only file there | every prediction test acts on what it predicts, so it needs fresh objects anyway; `smbcacls` works at run time without privileges (2.6); the DC is already in the gate and the only server with NT ACLs, so no fixture server is added. 2.13: the DC's DOS attributes outlive their file by inode, so without the reset a mask test fails at random and a stale `R` refuses `rm`; a replaced file's record cannot be reached by any path afterwards, hence no replace of an `R` file there (finding 1) |
| A16 | **The prediction tests pin K1 (on POSIX), K2, K3, K4 and K7** as exact disagreements, K6 as an agreement, and the `?` rows (`rodir2/minedir`, `I/sub`, `rodos`) as unknown together with what the real command did | a change of Samba or of a rule shows up as a failing row, and the documents' list of limits cannot drift from what is measured |
| A17 | **No security descriptor in the client** | 2.6: group membership is not visible to the client, `read only` and the DOS attribute and the sticky bit are not in the descriptor, and it would cost a request per object and LSA lookups |
| A18 | No new dependency | the crate's `create_context` codec and the existing transport do everything |
| A19 | **Errors of a chain that are not about one path**: `execute_compound`'s own error, when it is not a lost connection, means the compound never reached the wire (the crate's contract): every path of that chain gets that error (shown `?`; for a single path it is the command's error). A task that ends without a result (a panic: `JoinError`) may have left requests on the wire: the connection is marked dead (`mark_dead`, as `abandon` does) and the call fails with `other`. A lost connection fails the call (A13) | finding 7: the first plan left both open; `access_inner` of `shares` ignores them, which is harmless there and would leave credits behind here |

**A7, the rule.** `m` is the path's mask, `p` the parent's (absent at the share root), "read-only"
the FILE_ATTRIBUTE_READONLY bit (0x1) of a directory's CREATE answer. Each column is allowed
(`ok` below: the command's word), refused (`-`) or unknown (`?`); a column is `?` when a mask it
needs is unknown.

| Column | Directory | File |
|---|---|---|
| 1 | `ls`: `m` has FILE_LIST_DIRECTORY (0x1) | `get`: `m` has FILE_READ_DATA (0x1) |
| 2 | `put`: `m` has FILE_ADD_FILE (0x2) → ok; otherwise `?` when the directory is read-only, else `-` | `put` (JSON `replace`): `m` has no DELETE (0x1_0000) → `-`; `p` has FILE_ADD_FILE → ok; otherwise `?` when the parent is read-only, else `-` |
| 3 | `mkdir`: `m` has FILE_ADD_SUBDIRECTORY (0x4) → ok; otherwise `?` when read-only, else `-` | — |
| 4 | `rmdir`: `m` has no DELETE → `-`; `p` has FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY or FILE_DELETE_CHILD (0x2, 0x4, 0x40) → ok; `p` has none of them → **`?`**; always `-` at the share root | `rm`: `m` has DELETE |

**`rmdir` was decided by the user** (2026-11-19), the third of the options the adversary laid out:
`-` without DELETE, ok with DELETE and a parent that lets you change something, and `?` when the
directory grants DELETE but its parent allows no change, because there POSIX refuses
(`rodir2/minedir`) and NT ACLs allow (`I/sub`), and no mask tells the two apart.

Why each is so:

- `ls`, `get`: FILE_READ_DATA / FILE_LIST_DIRECTORY, **not SYNCHRONIZE**, although both opens ask
  for it: on a `read only` share no mask has S, yet both work (2.5). Price: K4.
- `put` into a directory: `put` writes a temporary file there (create-only, FILE_WRITE_DATA) and
  renames it within the directory; FILE_ADD_FILE is what both need (`A`, `C`, `G`, `rodir`,
  `writeonly`, `sticky`). The temporary file is the session's own, so DELETE on it is not asked
  (measured: `A`, with nothing inherited, takes a `put`); a drop box whose inheritable ACL gives
  the creator no DELETE refuses the rename, and no mask shows it (K7, 2.13).
- `put` over a file (`put -f`): the temporary file needs FILE_ADD_FILE on the parent, the rename
  with replace removes the old file. **FILE_WRITE_DATA on the file is irrelevant**: `rodir/ww.txt`
  has it and is refused, `rootfile.txt` in `S` lacks it and is replaced. DELETE on the target is
  what Windows is expected to check; Samba does not (K1, K2), so the rule may say no where Samba
  says yes, never the other way in what was measured.
- `rm`: DELETE on the file. **Not "or FILE_DELETE_CHILD on the parent"**, as the draft had it: on
  Samba the file's DE already holds what the parent allows (`D/file.txt`), and the OR is wrong for
  the sticky bit (`sticky/rootfile.txt`: the parent has DC, `rm` is refused).
- `rmdir`: DELETE on the directory is not enough on POSIX (2.4: `rodir2/minedir`, `sticky/rwxsub`
  say DE and cannot be removed). When the parent lets the session change nothing the answer is
  `?` (`rodir2/minedir` refused, `I/sub` allowed); K3 remains (sticky: the parent can be changed).
  The parent condition applies to directories only: a file's DE on Samba never comes from its own
  mode (2.4), and on a Windows share where folders grant "Modify" (which has no DELETE_CHILD) and
  files grant DELETE, files stay removable in the answer.
- A directory's DOS read-only attribute (2.13, `rodos`): Samba drops WD, AD and DE from the mask
  but does not enforce the attribute on a directory, so a missing WD or AD there means "unknown",
  for the directory's `put` and `mkdir` and for `put` over its files. Its own `rmdir` stays `-`
  (refused: `cannot be deleted`), `rm` of its files keeps the file's DE (correct in 2.13). A file's
  read-only attribute is not treated so: replacing it is K1 (Samba replaces, Windows is expected
  not to), and its `rm` is refused everywhere.
- The share root has no `rmdir` (rsmb refuses it as `usage`).

## 4. Departures from the design given by the orchestrator

| Draft | This plan | Evidence |
|---|---|---|
| `access [-e] [PATH...]` | one path (A1) | the table's fixed positionals; every other command takes one path |
| CREATE+CLOSE parent and CREATE+CLOSE target in one compound | the same, but the compound is a chain whose answers are cut at the first failure and the rest asked again (A4, A5) | 2.8: Samba cascades a failed CREATE to everything after it |
| `-e` batches like `shares` (up to `ACCESS_CHAIN`, width by credits) | the same chain size; the width is counted in paths, min(credits / 2, 256) (A6) | the retries of A5 are single paths; 2.9 |
| file `put` needs DELETE on the file; `rm` DELETE on the file **or** FILE_DELETE_CHILD on the parent | `put` as the draft, and `?` under a read-only directory; `rm` DELETE only; `rmdir` three answers: `-` without DELETE, ok with DELETE and a parent that lets you change it, `?` with DELETE and a parent that does not (A7, the user's decision) | 2.7: `sticky/rootfile.txt` (the OR is wrong), `rodir2/minedir` and `I/sub` (DE alone is wrong for directories, and the parent decides differently on POSIX and NT ACLs); 2.13 `rodos` |
| columns `ls put mkdir rmdir` / `get put rm` | the same in text, plus `?` for unknown; no `cd` (A8); in JSON a file's `put` is `replace` (A10) | 2.5; finding 11 |
| the probe's desired access to be found | `FILE_READ_ATTRIBUTES` (A2) | 2.3 |
| possibly AND with the tree's MaximalAccess | no (A3) | 2.5 |
| — | `Remote::stat` asks for attributes only (A14) | 2.11 |

## 5. Design

### 5.1 Files, dependencies

`src/session.rs`: the probe, its answer, the chains (`Rights`, `rights_request`,
`maximal_access`, `probe_rights`, `answered`, `settle`, `Session::rights`), and `Remote::stat`'s
request.
`src/commands.rs`: the command (`CmdId::Access`, the table entry, the `access` JSON object, the
rule `allowed`, the line `access_line`, `access_cmd`). No new file, no new dependency, no change to
`src/main.rs`, `src/shell.rs`, `src/transfer.rs`, `src/walk.rs` except what the new `CmdId` arm
needs (none expected: completion reads the table).

### 5.2 The probe (`src/session.rs`)

```rust
use smb2::msg::create_context::{CreateContext, find, pack_contexts, parse_contexts};

/// What the server says this session may do with one path: the MxAc answer of a CREATE
/// (MS-SMB2 2.2.13.2.5, 2.2.14.2.5). Nothing is created or changed to find out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights {
    pub is_dir: bool,
    /// FileAttributes of the CREATE answer (0x10 directory, 0x1 read-only, …).
    pub attributes: u32,
    /// MaximalAccess; None when the answer had no MxAc context, a short one, or a QueryStatus
    /// that is not success.
    pub mask: Option<u32>,
}

/// The create context's name.
const MXAC: &[u8] = b"MxAc";
/// Paths asked about at once, at most (512 requests): where 200 ms stop dominating (Stage 6
/// plan 2.9).
const PROBE_LIMIT: usize = 256;

/// CREATE (open only, file or directory, FILE_READ_ATTRIBUTES: the one access every reachable
/// object grants, so the answer never hides the mask) with an empty MxAc context.
fn rights_request(name: String) -> CreateRequest {
    let mut c = open_request(name, FileAccessMask::FILE_READ_ATTRIBUTES);
    c.create_options = 0;
    c.create_contexts = pack_contexts(&[CreateContext::new(MXAC, Vec::new())]);
    c
}

/// MaximalAccess from a CREATE answer's context chain; None when it is not there or not usable
/// (a malformed chain included: the open itself succeeded).
fn maximal_access(contexts: &[u8]) -> Option<u32> {
    let cs = parse_contexts(contexts).ok()?;
    let d = &find(&cs, MXAC)?.data;
    (d.len() >= 8 && u32_at(d, 0) == 0).then(|| u32_at(d, 4))
}

/// How many answers of a chain count, from (CREATE succeeded, CLOSE succeeded) per path: up to
/// the first failed CREATE, which counts itself only when it is the first or the CLOSE before it
/// succeeded. The server fails every later request of a related chain with the status of the
/// first failure (Stage 6 plan 2.8, A5).
fn answered(pairs: &[(bool, bool)]) -> usize

/// Puts the answered results of the chain `group` (indices into the call's paths) in their
/// places and queues every later index of the chain again, each alone, in the chain's order.
fn settle(
    group: &[usize],
    answers: Vec<Result<Rights>>,
    results: &mut [Option<Result<Rights>>],
    queue: &mut VecDeque<Vec<usize>>,
)

/// One chain: CREATE (with MxAc) + CLOSE per path, all in one compound (2 requests per path,
/// 1 flight). Returns the results of the first `answered` paths, in order; the caller asks again
/// for the rest. A lost connection is the call's error.
async fn probe_rights(
    conn: &Connection,
    tree: &Tree,
    paths: &[Vec<String>],
) -> Result<Vec<Result<Rights>>>
```

`probe_rights`: one `rights_request(tree_path(tree, p))` per path; ops CREATE, CLOSE
(`FileId::SENTINEL`) per path, as `create_close` does for one; `execute_compound`. For each CREATE
answer (every second): `Err(e)` that `classify` calls lost → return `Err`; `Ok(f)` with success →
`Ok(Rights { is_dir: attributes & 0x10 != 0, attributes, mask:
maximal_access(&resp.create_contexts) })`; anything else → `Err(classify(.., &display_path(p)))`
for that path. `answered` (with each CLOSE's status beside its CREATE's) says how many of them are
returned. An answer that does not unpack is `malformed()` for that path. `execute_compound`'s own
error: lost → the call's error; otherwise every path of the chain gets it (A19).

### 5.3 `Session::rights`: chains, width, the cascade

```rust
impl Session {
    /// What this session may do with each path of the selected share, in order. Chains of up to
    /// ACCESS_CHAIN paths, at most min(credits / 2, PROBE_LIMIT) paths in flight (credits when the
    /// call starts, at least 1); the paths after a failure in a chain are asked again, one per
    /// compound. A path the crate would not send as it is (`crate_path`) is `invalid_name` and is
    /// not sent. A lost connection fails the call: nothing has been printed, so the command may
    /// run again.
    pub async fn rights(&mut self, paths: &[Vec<String>]) -> Result<Vec<Result<Rights>>> {
        let share = self.selected()?;
        self.in_flight = true;
        let r = self.rights_inner(&share, paths).await;
        self.in_flight = false;
        r
    }
    async fn rights_inner(&mut self, share: &str, paths: &[Vec<String>])
        -> Result<Vec<Result<Rights>>>;
}
```

`rights_inner`: `let tree = self.tree(share).await?` (reconnects a dead connection, as every
sending method); `limit = (credits / 2).clamp(1, PROBE_LIMIT)`; `per = ACCESS_CHAIN.min(limit)`.
The results start as `Err(invalid_name)` for paths `crate_path` refuses and are not sent; the
others are split, in order, into groups of `per` indices in a `VecDeque<Vec<usize>>`. Loop: while
paths in flight + the next group's length ≤ `limit`, spawn `probe_rights` of the next group on a
`JoinSet` (a clone of the connection and of the `Arc<Tree>`; the task returns its group with its
answers); take the next result and hand it to `settle`, which puts the answered results in their
places and queues every later index of the group alone (the pure part, unit-tested); a lost
connection returns at once (the `JoinSet` is dropped, `in_flight` stays set until `rights` clears
it, and `run_parsed` marks the connection dead; 5.6); a `JoinError` marks the connection dead and
fails the call with `other` (A19). The loop ends when the queue and the set are empty. About 60
lines. No sleep, no timer of its own.

Costs on Samba (2.9): the share root 2 / 1; a path below it 4 / 1; a path whose parent fails 6 /
2; n paths of `-e` with no failure 2(n + 1) requests in ceil((n + 1) / 256) waves (1 flight up to
255 entries on the test server); each failure in a chain sends the rest of that chain again, at
most one more round trip: `-e <S>/listonly` (two entries, the first refused) 5 + 6 + 2 = 13 in 3,
`-e <S>/mixed` (four entries, the second refused) 5 + 10 + 4 = 19 in 3 (2.13).

### 5.4 From mask to commands (`src/commands.rs`)

```rust
/// What `access` shows for one path: Some(true) allowed, Some(false) refused, None unknown.
/// For a file `mkdir` is None and is not shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allowed {
    pub read: Option<bool>,   // ls / get
    pub put: Option<bool>,
    pub mkdir: Option<bool>,
    pub remove: Option<bool>, // rmdir / rm
}

const LIST_OR_READ: u32 = 0x1;  // FILE_LIST_DIRECTORY on a directory, FILE_READ_DATA on a file
const ADD_FILE: u32 = 0x2;      // FILE_ADD_FILE on a directory
const ADD_SUBDIR: u32 = 0x4;    // FILE_ADD_SUBDIRECTORY
const DELETE_CHILD: u32 = 0x40; // FILE_DELETE_CHILD
const DELETE: u32 = 0x1_0000;
const READ_ONLY: u32 = 0x1;     // FILE_ATTRIBUTE_READONLY in the CREATE answer

/// The rule of docs/commands.md, `access` (Stage 6 plan A7). `parent`: the rights of the directory
/// the path is in, None when they are unknown (its probe failed) or at the share root; `root`: the
/// path is the share root.
pub fn allowed(r: Rights, parent: Option<Rights>, root: bool) -> Allowed
```

`allowed` with `r.mask` None: every field None. Otherwise, with `has(m, bit) = m & bit != 0`,
`ro(x) = x.attributes & READ_ONLY != 0`, and "the parent's mask" None when `parent` or its mask
is None:

- Directory: `read = has(m, LIST_OR_READ)`. `put`: `has(m, ADD_FILE)` → true; else None if
  `ro(r)`, else false. `mkdir` the same with ADD_SUBDIR. `remove`: false at the root; false
  without `has(m, DELETE)`; else the parent's mask: has any of ADD_FILE, ADD_SUBDIR,
  DELETE_CHILD → true, has none → **None** (the user's rule), unknown → None.
- File: `read = has(m, LIST_OR_READ)`. `put`: false without `has(m, DELETE)`; else the parent's
  mask: has ADD_FILE → true; lacks it → None if the parent is read-only, else false; unknown →
  None. `mkdir = None` (not shown). `remove = has(m, DELETE)`.

A refusal that one mask decides is not made unknown by the other.

### 5.5 The command (`src/commands.rs`)

```rust
async fn access_cmd(sess: &mut Session, out: &mut Out, a: &Args) -> Result<()>
/// `d  ls  put mkdir rmdir  name`: the mark, four columns of 3, 3, 5 and 5 characters, the name;
/// with `error`, `?` in every column and `  (<kind with spaces>)` after the name.
pub fn access_line(name: &str, is_dir: bool, a: Allowed, error: Option<Kind>) -> String
fn access_obj(path: &[String], is_dir: bool, r: &Result<Rights>, a: Allowed) -> Value
```

Flow. `usage` without a share (`selected`). `let path = sess.resolve(arg.unwrap_or("."))`.

- **Without `-e`**: `paths = [path]` at the share root, else `[parent, path]`; `let r =
  sess.rights(&paths).await?`; the path's own result is the last; its `Err` is the command's
  error (A12). The parent's mask is `r[0]`'s mask when it is `Ok`, else None. One line: the name
  is `display_path(path)` followed by `/` for a directory (the root is `/`).
- **With `-e`**: `let entries = sess.list(&path).await?` (as `ls`: sorted, cached; a file is
  `not_a_directory`); `paths = [path, path/e₁, …, path/eₙ]`; `let r =
  sess.rights(&paths).await?`; the parent mask of every entry is `r[0]`'s; one line per entry in
  the listing's order, the type from the listing, the name `e.name` with `/` after a directory;
  an entry whose result is `Err(e)` is printed with `error: Some(e.kind)`. The command succeeds
  when the listing and the call did.
- Everything is printed after the last answer (`Out::text`, which escapes the name, or
  `Out::obj`), so nothing is printed twice if the command runs again.

**Text** (`access_line`, `format!("{mark}  {c1:<3} {c2:<3} {c3:<5} {c4:<5}  {name}{tail}")`, where
a column is its word when allowed, `-` when refused, `?` when unknown, and the third column is
blank for a file):

```
d  ls  put mkdir rmdir  /upload/rsmb-test-1/
d  ls  put mkdir ?      /upload/
d  ls  -   -     -      /
-  get put       rm     mine.txt
-  -   put       rm     secret.txt
-  get -         -      dosro.txt
d  ls  -   -     rmdir  rodir/
d  -   put mkdir rmdir  writeonly/
d  ls  put mkdir ?      minedir/
d  ls  ?   ?     -      rodos/
-  ?   ?         ?      a.txt  (access denied)
```

**JSON**, one object per path, in the same order:

```
{"type":"access","name":"upload","path":"/upload","is_dir":true,"mask":2032127,"attributes":16,"error":null,"ls":true,"put":true,"mkdir":true,"rmdir":null}
{"type":"access","name":"mine.txt","path":"/upload/s/mine.txt","is_dir":false,"mask":1507743,"attributes":32,"error":null,"get":true,"replace":true,"rm":true}
{"type":"access","name":"a.txt","path":"/upload/s/listonly/a.txt","is_dir":false,"mask":null,"attributes":null,"error":"access_denied","get":null,"replace":null,"rm":null}
```

`name` is the last component (`""` for the share root), `path` from the share root starting with
`/` (as in `entry`). A directory carries `ls`, `put`, `mkdir`, `rmdir`; a file `get`, `replace`,
`rm`; never the other type's keys. Each is `true`, `false` or `null`. (The `attributes` values of
the example are what Samba sends for those objects in the planning fixture; a test compares the
directory bit and the read-only bit only.)

The `JSON_OBJECTS` row: `("access", "name string, path string (from the share root, starting with
/), is_dir bool, mask integer (the MaximalAccess the server computed for this session) or null,
attributes integer (the file attributes of the server's answer: 16 directory, 1 read-only) or
null, error string (an error kind: this entry could not be asked about; access -e only, the same
path alone is the command's error) or null; a directory has ls, put (a new file into it), mkdir,
rmdir; a file get, replace (put -f over it), rm: each true (allowed), false (refused) or null
(unknown)")`.

### 5.6 Errors, exit codes, reconnect, interrupt

- Exit 0 when the command succeeded (with `-e` also when some entries are `?`), 1 for its
  errors, 2 for `usage`. Error kinds: `not_found`, `access_denied`, `not_a_directory` (`-e`),
  `invalid_name`, `usage`, `connection_lost`, `interrupted`; nothing new in `Kind`.
- **A closed connection** in the middle: every pending compound fails at once (2.10), `rights`
  returns `connection_lost`, `run_parsed` marks the connection dead and, the table saying
  `rerun` and nothing having been printed, runs the command once more on a new connection, which
  repeats the listing too. **A silent server**: the deadline, then `connection_lost`, not run again
  (the existing rule).
- **An interrupt** (Ctrl-C, SIGINT, SIGTERM): the command's future is dropped with `in_flight`
  set; `abandon` marks the connection dead; the next command that sends opens a new one. Probes
  left on the wire hold credits until answered, which is why the connection is replaced (as for
  every command). Nothing to clean up: nothing was created.
- **No output before the end** (A11): a large `-e` is silent until its last answer (`-e /huge`
  20.9 s at the fast endpoint, about 80 s computed at 100 ms each way); Ctrl-C ends it. The manual
  and `docs/commands.md` say so.
- **Errors of a whole chain** (A19): the crate's call error that is not a lost connection gives
  every path of the chain that error; a task that ended without a result marks the connection
  dead and fails the command with `other`.
- No `Remote`: `access` creates nothing and needs no registry; it is a `Session` method like
  `shares`, `cd` and `ls` (the navigation commands), which `docs/development.md` allows for
  methods that send and start with `tree()`.
- Cache: `-e`'s listing refreshes the cached listing of the directory (as `ls`); the probes
  change nothing, so nothing is invalidated.

### 5.7 The table entry and `help all`

After `ls` in `COMMANDS` (so `help` lists it among the navigation commands):

```rust
Cmd {
    id: CmdId::Access,
    name: "access",
    syntax: "access [-e] [<path>]",
    summary: "show which commands the server would allow on a path, without trying them",
    details: "Asks the server for your access to the path (default: the working directory) and \
              shows what it allows, as rsmb commands: for a directory ls (list it), put (a new \
              file into it), mkdir (a directory in it), rmdir (remove it); for a file get (and \
              cat), put (put -f over it; 'replace' in --json), rm. '-' is refused, '?' unknown: \
              rmdir is '?' when the directory may be deleted but the one above it lets you \
              change nothing (Samba on Unix permissions refuses, on NT ACLs allows), put and \
              mkdir are '?' in a directory with the DOS read-only attribute (Samba hides the \
              rights but does not enforce it on directories). Nothing is created, \
              changed or tried: one open per path for its attributes, with the server's \
              maximal-access answer (MxAc). With -e, every entry of the directory instead, \
              listed like ls (always asked, refreshes the cache Tab uses). Costs on Samba: 4 \
              requests in 1 round trip (the path and its directory in one compound), 2 at the \
              share root; -e: the listing (5 requests, 1 round trip), then 2 per entry plus 2, \
              16 per compound, 256 at once (1 more round trip up to 255 entries); nothing is \
              printed before the last answer (a directory of 100,000 entries takes about 20 s \
              on a fast link). It is the server's answer for this login: on Samba it includes \
              the share's read only. It does not tell whether a directory is empty, a file is in \
              use, the disk is full, or a name exists, and it can say put where put fails: in a \
              drop box whose inherited permissions do not let you delete your own new file, \
              put writes it and cannot rename it. docs/known-limitations.md lists every case \
              where the server's answer and the command differ. One line per path: 'd' or '-', \
              the four columns, the name. With --json one 'access' object each, with the mask. \
              Errors: not_found, access_denied (not even the attributes can be read), \
              not_a_directory (-e on a file), invalid_name, usage; with -e an entry that cannot \
              be asked about is a '?' line (an object with error) and the command succeeds. Run \
              again once after a lost connection (it prints nothing before its last answer).",
    flags: &[("-e", "every entry of the directory instead of the directory itself")],
    args: &[Arg::RemotePath],
    required: 0,
    examples: &[
        ("access", "what you may do with the working directory"),
        ("access upload", "may you list upload, put a file into it, make a directory in it, remove it?"),
        ("access -e /tree", "the same for every entry of /tree, files and directories"),
    ],
    rerun: true,
},
```

The examples run from the root of `share` and change nothing (`examples_run`; `upload` and
`/tree` exist on `samba` and `samba-private`). `-e` is a flag without a value, its own token, so a
file called `-e` stays reachable as `access -- -e`. `docs/architecture.md` and
`docs/commands.md` ("Quoting and paths": the flags without a value are `-r`, `-f`, `-p`) gain
`-e`.

### 5.8 `docs/commands.md`: the section

A section ``## `access` `` after ``## `ls` ``, with the syntax block, then:

- What it shows: the four columns by type, `-`, `?`, the default path, `-e`.
- **The rule** (the table of A7, with the bit values) and one line per departure from the obvious
  bit, as in A7's list: `put` over a file is not FILE_WRITE_DATA, `rm` is not "or the parent's
  DELETE_CHILD", `rmdir` needs the parent (and is `?` when the parent lets nothing change), `ls`
  and `get` do not require SYNCHRONIZE, a directory's DOS read-only attribute makes `put` and
  `mkdir` unknown.
- **Measured on Samba 4.23.8** (2.4 to 2.7 in short): as `tester` on `share` the root shows `d  ls
  -   -     -`, `upload` `d  ls  put mkdir ?` (its parent, the root, lets nothing change),
  `traverse-only` `- - - -`, `traverse-only/known.txt` `get - -`; on `readonly` every `put`,
  `mkdir`, `rm`, `rmdir` is `-`; on `guest` (anonymous, Guest) `ls` and `get` only.
- **What it does not tell** (2.7, last paragraph), and **where the server's answer and the
  command differ** (K1, K2, K3, K4, K7 in one line each, with the measured case; "on Samba"),
  with the sentence: "An allowed column was wrong in three measured cases: two made on purpose
  (K3, K4) and an ordinary drop box (K7); a refused one in two (K1, K2). Where no mask can decide,
  the column is `?` (`rmdir` under a directory that lets nothing change; `put` and `mkdir` in a
  directory with the DOS read-only attribute)."
- Costs (5.3) and the time at the slow endpoint (2.9: 202 ms; `-e /fanout` about 2.3 s; on a
  server with 16 credits one compound of 8 at a time).
- A lost connection, an interrupt, no output before the end (5.6).
- The output, text and the `access` object, with the lines of 5.5.
- Errors (A12).
- Examples (the three of the table).

`docs_match_the_table` already demands the section with its syntax and examples.

The section "Differences from smbclient-ng" gains: "smbclient-ng's `acls` prints each object's
security descriptor; rsmb has no ACL display. `access` asks the server what it allows and prints
that as commands."

### 5.9 `docs/scripting.md`

- The `access` object in the list of objects, with its fields and the three lines of 5.5.
- A paragraph "Asking before acting": a column says what the server would allow at that moment;
  a directory's `put` means "a new file into it", a file's `replace` "`put -f` over it"; `null`
  means rsmb cannot tell, which a script treats as "try it"; act and check the exit code all the
  same (the answer is not a lock, and `docs/known-limitations.md` names where it differs, the
  drop box K7 among them). A path that cannot be asked about is the command's error (exit 1) when
  given alone, and an object with `error` (exit 0) as an entry of `-e`. A large `-e` prints
  nothing until it has every answer.
- An example:

```
# may I put a file into /upload, and which files of /upload/in may I delete or replace?
rsmb … --json -N -C 'use share' -C 'access /upload' -C 'access -e /upload/in' \
  | jq -c 'select(.type=="access") | if .is_dir then {path, put} else {path, replace, rm} end'
```

### 5.10 `Remote::stat` asks for attributes only (A14)

`Remote::stat` builds its CREATE with `open_request(tree_path(..),
FileAccessMask::FILE_READ_ATTRIBUTES)`, options 0, `FileOpen`, and sends it with `create_close`
(2 requests, 1 flight, as now). `open_close` and `CD_ACCESS` stay for `cd` and `mkdir`. The doc
comment of `stat` says why (`docs/transfers.md`, "The two paths": the check). `transfer_costs` is
unchanged; F87 is the test that fails before the change. **What it changes** (2.13, accepted by
the user), each into `docs/transfers.md`:

- the overwrite table: a large `put` without `-f` over an existing file the user may not read is
  `already_exists` (was `access_denied`); a large `put -f` over it replaces it (was refused);
- the rename's wording table: a target that is a directory the user may not read is now found to
  be a directory, so `put` onto it is `is_a_directory` on both paths (the small path said
  `already_exists`, the large one `access_denied`);
- the failure matrix: a large `put -f` over a file that cannot be replaced (sticky directory,
  someone else's file) is refused by the rename after the upload, not by the check before it: the
  data crosses the link first (50 MiB at the slow endpoint: 7.3 s instead of 1.4 s), the message is
  the rename's (`the server refused to replace it …`), the temporary file is removed, the target
  untouched. Before A14 the check refused it only because it could not open the file at all; a
  replaceable file the user may read was always checked and uploaded the same way.

### 5.11 What does not change

`cd`, `ls`, the listing, the cache's rules, `tree`, `find`, the transfers apart from A14, `shares`,
start-up, the proxy, Kerberos, the test images, `check.sh`, `ctl.sh`, `test.sh`'s steps and its
budgets.

## 6. The test environment

**Nothing in `testenv/` changes** (A15). The fixtures are made by the tests in their scratch
directories (`Scratch`, removed when the test ends) and are written here exactly as the planning
run made them.

### 6.1 The POSIX fixture (`share` on `fast()`: `samba` direct, `samba-private` through the proxy)

Run as root in the container, in the scratch directory (`docker exec -w <scratch> <container> sh
-c <script>`); 0.15 s:

```sh
set -e
f() { echo "$1" > "$1"; chown "$2" "$1"; chmod "$3" "$1"; }
for p in '' rm-; do
  f ${p}mine.txt tester:tester 0644
  f ${p}rootfile.txt root:root 0644
  f ${p}secret.txt root:root 0600
  f ${p}dosro.txt tester:tester 0644
done
head -c 300000 /dev/zero > big.bin; chown root:root big.bin; chmod 0600 big.bin
mkdir rodir rodir/sub; f rodir/f.txt root:root 0644; f rodir/rm-f.txt root:root 0644
f rodir/ww.txt root:root 0666; chown root:root rodir rodir/sub; chmod 0755 rodir rodir/sub
mkdir rodir2 rodir2/minedir; f rodir2/minefile.txt tester:tester 0644
f rodir2/rm-minefile.txt tester:tester 0644
chown tester:tester rodir2/minedir; chown root:root rodir2; chmod 0755 rodir2
mkdir sticky sticky/rootsub sticky/minesub sticky/rwxsub
f sticky/rootfile.txt root:root 0644; f sticky/rm-rootfile.txt root:root 0644
f sticky/mine.txt tester:tester 0644; f sticky/rm-mine.txt tester:tester 0644
chown tester:tester sticky/minesub; chmod 0777 sticky/rwxsub
chown root:root sticky sticky/rootsub sticky/rwxsub; chmod 1777 sticky
mkdir emptyroot emptymine; chown tester:tester emptymine
mkdir listonly; f listonly/a.txt root:root 0644; f listonly/b.txt root:root 0644; chmod 0744 listonly
mkdir writeonly; chmod 0733 writeonly
mkdir rootdir700; chmod 0700 rootdir700
mkdir rodos rodos/sub rodos2; f rodos/f.txt tester:tester 0644; chown tester:tester rodos rodos/sub rodos2
mkdir mixed mixed/d; f mixed/a.txt tester:tester 0644; mkfifo mixed/b.fifo; f mixed/c.txt tester:tester 0644
chown tester:tester mixed mixed/d mixed/b.fifo
smbclient -s /dev/null //127.0.0.1/share -U 'TESTDOM\tester%testpass' \
  -c "cd upload/$(basename "$PWD"); setmode dosro.txt +r; setmode rm-dosro.txt +r; setmode rodos +r; setmode rodos2 +r" >/dev/null
```

The DOS attribute is set over SMB from inside the container (`samba-private` admits 127.0.0.1);
these servers keep it on the file itself, so removing the scratch directory removes it (2.13).
Directories made by root without `chown` are root's, mode 0755 by the umask. `rodos` and `rodos2`
are tester's directories with the DOS read-only attribute (2.13), `mixed` holds a FIFO that Samba
lists and will not open (the failure in the middle of a chain, 2.13), `rootdir700` is the
directory of F87.

### 6.2 The NT ACL fixture (`share` on `dc()`: the DC by address, then by name through the proxy)

Run as root in `rsmb-samba-dc`, in the scratch directory (root's, 1777); 1.9 s. The admin password
is the throwaway value of the DC's image (`docs/test-environment.md`).

```sh
set -e
d=upload/$(basename "$PWD")
mkdir A C D E F G H I J
for x in D/file.txt D/rm-file.txt D/denydel.txt E/file.txt E/rm-file.txt F/file.txt \
         F/rm-file.txt H/nosync.txt I/own.txt I/rm-own.txt; do
  echo "$x" > "$x"; done
mkdir E/empty H/nosyncdir I/sub
chmod -R 0777 A C D E F G H I J
U='-U RSMB\Administrator%Adm1n-testpass'
# A fresh DOS attribute on every object made outside Samba: the DC keeps attributes by inode and
# a reused inode would bring a stale read-only flag with it (2.13).
c=$(find A C D E F G H I J -print | sed 's|.*|setmode "&" -rsh|' | paste -sd ';' -)
smbclient //127.0.0.1/share $U -c "cd $d; $c" >/dev/null
O='OWNER:RSMB\Administrator,GROUP:RSMB\Domain Users,ACL:RSMB\Administrator:ALLOWED/0x0/FULL'
CO='ACL:CREATOR OWNER:ALLOWED/OI|CI|IO/FULL'
t() { smbcacls //127.0.0.1/share "$d/$1" $U --set "$2" >/dev/null; }
t A "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200ab"
t C "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200ad,$CO"
t D "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200e9"
for x in D/file.txt D/rm-file.txt; do t $x "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00120089"; done
t D/denydel.txt "ACL:RSMB\\tester:DENIED/0x0/0x00010000,$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00120089"
t E "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200af,$CO"
for x in E/file.txt E/rm-file.txt; do t $x "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00130089"; done
t E/empty "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200a9"
t F "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200ab,$CO"
for x in F/file.txt F/rm-file.txt; do t $x "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00120089"; done
t G "$O,ACL:RSMB\\Domain Users:ALLOWED/0x0/0x001200ab,$CO"
t H "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200a9"
t H/nosync.txt "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00000081"
t H/nosyncdir "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x00000081"
t I "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200a9"
for x in I/sub I/own.txt I/rm-own.txt; do t $x "$O,ACL:RSMB\\tester:ALLOWED/0x0/FULL"; done
t J "$O,ACL:RSMB\\tester:ALLOWED/0x0/0x001200ab,ACL:RSMB\\tester:ALLOWED/OI|IO/READ"
```

`J` is the drop box of K7 (list and add file; a new file inherits read only, no CREATOR OWNER).
The first plan's `F/dosro.txt` and `F/rm-dosro.txt` are gone: replacing a read-only file on the DC
leaks its attribute record to the next object on that inode, and no path reaches it afterwards
(2.13); K1 stays pinned on POSIX. Nothing in this fixture is read-only, so `Scratch`'s clearing
before removal (7.1) finds nothing to clear unless a test made it so.

## 7. Tests

### 7.1 The harness, `tests/common/mod.rs`

```rust
/// The POSIX fixture of the Stage 6 plan, 6.1, made as root in `s` (on its container).
pub fn posix_fixture(s: &Scratch)
/// The NT ACL fixture of 6.2, made as root on the DC in `s` (`s.container` is the DC's).
pub fn nt_fixture(s: &Scratch)
/// Runs each line in the session and returns, per line, None when it succeeded or the error's
/// kind. Lines run in the order given.
pub fn outcomes(c: &mut Client, lines: &[String]) -> Vec<Option<String>>
/// The `access` objects of a reply by `name`, for `-e` runs.
pub fn access_by_name(r: &Reply) -> BTreeMap<String, Value>
```

Both fixtures run through `docker_sh` with `cd '<server path>' && <script>`, as root (not
`Scratch::sh`, which runs as tester).

**`Scratch` on the DC** (finding 1, 2.13), for every test that uses one there, not only these:
`Scratch::new` gives the new directory a fresh DOS attribute over SMB right after `mkdir`
(`smbclient //127.0.0.1/share -U 'RSMB\Administrator%Adm1n-testpass' -c "cd upload; setmode
<name> -rsh"` inside the DC; about 0.05 s); `Drop` first clears the attributes of everything left
in it over SMB (the `find … | setmode "<path>" -rsh` list of 6.2, in one `smbclient` call) and then
removes it as now. On the Samba servers nothing changes (they keep the attribute on the file). A
test on the DC never replaces a file it made read-only. A test in `tests/fast.rs` names `fast()` and `dc()`, never
an address (the Stage 4 rule). Local files for `put` (`notes.txt`, 6 bytes; `big.bin`, 300,000
bytes) are made in the test's `tmp` directory and reached with `lcd`.

**How a prediction test runs** (F81, F82): make the fixture; in one session (`Client::to(fast(),
…)` or `Client::kerberos(dc(), cache, …)`) `use share`, `cd <scratch>`, then the `access -e` lines
of the test (5.5's JSON) and collect every object by path; check **each mask equals the value in
2.4 / 2.6** (a Samba upgrade that changes an answer fails here, with the table's row in the
message); then `lcd`, run the action lines of 2.7 in their order with `outcomes`; for every row of
the test's table compare the predicted column (from the objects) with the outcome (`None` =
allowed). A row is either **agree** (predicted `true` and ok, or `false` and an error) or **K*n***,
which must disagree exactly as 2.7 says (the message names K*n* and this plan). `listonly/a.txt`
must be `null` with `error: "access_denied"`. The table of rows is written in the test as a
`const` array next to the masks: `(path, column, expected mask, line, expectation)`, where an
expectation is agree, K*n*, or **unknown** (the column must be `null` / `?`; the real outcome is
asserted as 2.7 and 2.13 measured it, so a later change of either shows). Before anything else
F82 asserts that the scratch directory's own mask is 0x001f01ff and its attributes 0x10: a stale
read-only record would show here first (finding 1).

### 7.2 `./test.sh` and the budget

No new step; budgets stay **150 s and 600 s**. Estimate from the planning runs (fixture 0.15 s and
1.9 s, an `access -e` of a fixture directory under 10 ms at the fast endpoint, the action sessions
about 0.5 s; the slow tests run beside the others):

| Step | After Stage 5 | Expected | From |
|---|---|---|---|
| unit tests | 2 s | 2 | six small tests |
| `tests/fast` | 13 | 17 | F81 to F87, about 4 s, beside the others |
| `tests/fast` through the proxy | 15 | 19 | the same through the proxy |
| `tests/slow` | 28 | 37 | S83 is about 17 s (`access -e /many` at 200 ms, twice) beside the others; S85 about 2 s |
| `tests/serial` | 385 | 397 | X81 about 12 s |
| **counted** | **477** | **about 500 of 600** | |

`fast`: 62 s now, about 70 of 150. If the first full run is over 540 s the implementing agent
says so under "Deviations during implementation" before adding anything; S83's directory can be
`/fanout` (2,000 entries) instead of `/many` without losing what it proves.

### 7.3 Unit tests (no server)

| Test | File | Asserts |
|---|---|---|
| `rights_from_masks` | commands | `allowed` on every measured pair of 2.4 to 2.6 and 2.13 (mask, attributes, parent's mask and attributes, root): `S` under `upload` → ls put mkdir rmdir; `upload` under the share root (0x001f01ff under 0x001200a9) → ls put mkdir **?**; the share root (0x001200a9, root) → ls - - -; `traverse-only` → - - - -; `forbidden` (0x80) → - - - -; `known.txt` (0x00120089) → get - -; `mine.txt` and `rootfile.txt` in `S` → get put rm; `secret.txt` → - put rm; `dosro.txt` → get **-** - (K1); `rodir/ww.txt` under `rodir` → get - -; `sticky/rootfile.txt` under `sticky` → get - - (not "or DC"); `rodir2/minedir` under `rodir2` → ls put mkdir **?** (the user's rule); `sticky/rwxsub` under `sticky` → ls put mkdir **rmdir** (K3); `emptyroot` under `S` → ls - - rmdir; `writeonly` → - put mkdir rmdir; `readonly`'s `mine.txt` (0x00040089 under 0x000c00a9) → get - -; NT `A` → ls put - -, `C` → ls - mkdir -, `D/file.txt` under `D` → get - rm, `F/file.txt` under `F` → get **-** - (K2), `H/nosync.txt` (0x81) → **get** - - (K4), `I/sub` under `I` → ls put mkdir **?**, `I/own.txt` under `I` → get - rm; `J` (0x001200ab) → ls put - - (K7: the rule says `put`); `rodos` (0x000c00e9, attributes 0x11, under `S`) → ls **? ?** - (no DE); `rodos/f.txt` (0x0017019f under `rodos`) → get **?** rm; `rodos/sub` (0x001f01ff under `rodos`, which keeps DC) → ls put mkdir rmdir; a file without DE under a read-only parent → put - (not `?`); mask None → all None; a file with DE and the parent unknown → put None, rm true; a file without DE and the parent unknown → put **false** (not None); a directory with DE and the parent unknown → rmdir None; the root with DE → rmdir false. The masks are the measured numbers, written as hex literals |
| `access_lines` | commands | `access_line` for each line of 5.5's text example, byte for byte, the column widths, `?` in one column (`rmdir`, `put`, `mkdir`) and in all with the reason `(access denied)`, `(not found)`, `(other)` |
| `maximal_access_answers` | session | `maximal_access` on chains built with `pack_contexts`: MxAc with QueryStatus 0 and mask 0x001f01ff → Some; QueryStatus 0xC0000022 → None; 4 bytes of data → None; no MxAc but a `QFid` context → None; MxAc after a `QFid` → Some; an empty chain → None; a chain whose `Next` points past its end → None |
| `rights_request_shape` | session | `rights_request("a\\b")`: desired access 0x80, `FileOpen`, create options 0, share access 7, oplock none, name `a\b`; `parse_contexts` of its contexts gives exactly one, named `MxAc`, with no data; packed with the crate's `Pack`, the body's context offset and length are non-zero |
| `chain_answers_up_to_the_first_failure` | session | `answered` on (CREATE ok, CLOSE ok) pairs: `[]` → 0; `[(t,t),(t,t)]` → 2; `[(t,t),(f,f),(t,t)]` → 2; `[(f,f),(t,t)]` → 1; `[(t,t),(t,t),(f,f)]` → 3; **`[(t,f),(f,f)]` → 1** (the second CREATE's failure may be the CLOSE's, finding 6); `[(t,f),(t,t)]` → 2 |
| `chain_retries_keep_their_places` | session | `settle` with the group `[3, 4, 5, 6]` of a call of 8 paths: two answers → results 3 and 4 set, nothing else touched, `[5]` and `[6]` queued in that order behind what was queued before; then `settle([5], [ok])` and `settle([6], [err])` put each at its own index; a group of one with a failure queues nothing (finding 5) |
| `table_is_complete`, `help_all_has_everything`, `every_json_line_has_a_type` | commands, fast | the new command and object pass the existing checks (no change to these tests) |

### 7.4 Integration tests

Every test runs the real binary. "The fixture" is 6.1 or 6.2 in a `Scratch` of its own.

**`tests/fast.rs`** (each run twice: direct, then through the proxy; `dc()` by address, then
`dc.rsmb.test` through port 1080)

| # | Test | How observed | Asserts |
|---|---|---|---|
| F81 | `access_predicts_posix` | `Client::to(fast(), …)`, the POSIX fixture; `access <S>`, `access -e <S>`, `-e <S>/rodir`, `-e <S>/rodir2`, `-e <S>/sticky`, `-e <S>/listonly`, `-e <S>/rodos`, `-e <S>/mixed`; then the 2.7 POSIX lines and those of 2.13 (`put notes.txt rodos/`, `mkdir rodos/new`, `put -f notes.txt rodos/f.txt`, `rmdir rodos/sub`, `rmdir rodos2`, `get mixed/c.txt dl/`) | every mask equals 2.4 and 2.13 (`rodos` 0x000c00e9 with attributes 0x11); every row of 2.7's POSIX table agrees, except **K1** (`dosro.txt` put: predicted false, `put -f` ok) and **K3** (`sticky/rwxsub` rmdir: predicted true, refused), which disagree exactly so; **unknown** rows: `rodir2/minedir` rmdir (`?`, refused), `rodos` put and mkdir and `rodos/f.txt` put (`?`, all ok); `rodos2` rmdir `-` and refused (`other`); **`put -f big.bin big.bin` agrees (predicted true, ok: A14)**; `listonly/a.txt` and `listonly/b.txt` are `null` with `error` `access_denied`, `get listonly/a.txt` fails; in `mixed` `a.txt`, `c.txt` and `d` have the masks of 2.13 under their own names and `b.fifo` is `null` with `error` `other` (the retried answers land in their places, finding 5); exit 0 for every `access` |
| F82 | `access_predicts_nt_acls` | `Client::kerberos(dc(), krb_cache(dir, "k.cc", "-f", "tester", &[dc().spn(true)]), …)`, the NT fixture; first `access <S>` (mask 0x001f01ff, attributes 0x10: no stale record, finding 1); `access -e <S>`, `-e <S>/D`, `-e <S>/E`, `-e <S>/F`, `-e <S>/H`, `-e <S>/I`; then the 2.7 NT lines without the two `dosro` lines, and `put notes.txt J/`, `mkdir J/new` | every mask equals 2.6 (`J` 0x001200ab); every row agrees except **K2** (`F/file.txt`), **K4** (`H/nosync.txt` get, `H/nosyncdir` ls) and **K7** (`J` put: predicted true, refused with a `left_behind` part file), exactly so; **unknown**: `I/sub` rmdir (`?`, ok); `G` (a group ACE) agrees on `put` and `mkdir` |
| F83 | `access_by_share_and_login` | the POSIX fixture; sessions on `fast()` as tester (`readonly`, `encrypted`), as anonymous and as `-u Guest --no-pass` (`guest`) | on `readonly`, `access -e <S>`: every `put`, `mkdir`, `rm`, `rmdir` false, `ls` and `get` as on `share` (`secret.txt` false), the masks of 2.5; `put notes.txt <S>/`, `rm <S>/rm-mine.txt` and `mkdir <S>/x` through `readonly` fail `access_denied`. On `encrypted`, `access -e <S>`: the same masks as on `share`. On `guest` as anonymous and as Guest: `access /` is `d  ls  -   -     -      /`, `access hello.txt` `-  get -         -      /hello.txt`, `put` into `/` fails. As tester on `share`: `access /` the same line |
| F84 | `access_costs` | `Client::tapped(fast())`, the POSIX fixture | `use share` then: `access` (at the root) **2 / 1**; `access upload` **4 / 1**; `access nosuch`: **4 / 1**, kind `not_found`, exit 1; `access nosuch/x`: **6 / 2**, `not_found`; `access -e <S>/listonly` **13 / 3** (5 + 6 + 2: `b.txt` asked again alone); `access -e <S>/mixed` **19 / 3** (5 + 10 + 4: `c.txt` and `d` asked again alone); `access -e /tree`: **19 / 2** (5 + 14), six objects; again: 19 / 2 (no cache); `access -e /`: **43** requests in 2 or 3 flights (19 paths: 2 compounds; whether the second leaves before the first's answer is timing); `access -e /tree/d0/file.txt`: `not_a_directory`, 5 / 1; **then `ls /tree` 5 / 1 and `cd /tree/d0` 2 / 1**, as before this stage; one connection, no TREE_CONNECT after `use` |
| F85 | `access_output` | `Client::to(fast(), …)` with and without `--json`, the POSIX fixture | text lines byte for byte for `access <S>/mine.txt`, `access <S>/rodir`, `access -e <S>/listonly` (`(access denied)`); `-e` lines in the listing's order; JSON: a directory has exactly `ls put mkdir rmdir` and no `get`/`replace`/`rm`, a file exactly `get replace rm` and no `put`, `mask` and `attributes` integers, `error` null; `cd /tree` then `access` = the line of `/tree/`; `access -e a b`: `usage`; `access` before `use`: `usage`; `access -- -e` looks for a path `-e` (`not_found`); a `done` after each |
| F86 | `access_refreshes_the_cache` | pty, `Pty::tapped(fast(), …)` | after `access -e /tree`, Tab on `cd /tree/d` lists d0 to d3 with **0 requests** (the listing was cached, as after `ls`) |
| F87 | `put_over_a_file_without_synchronize` | the POSIX fixture, `Client::tapped(fast())` | every effect of A14 (2.13): `put -f big.bin <S>/big.bin` (300,000 bytes over root's 0600 file): ok, the server's sha256 equals the local one, the check is 2 requests as `transfer_costs` counts (before A14: `access_denied`); `put big.bin <S>/secret.txt` (no `-f`): `already_exists`, the message holds `-f replaces it`, nothing created (before: `access_denied`); `put notes.txt <S>/rootdir700` and `put big.bin <S>/rootdir700`: both `is_a_directory`, the message holds `write '` and `rootdir700/'` (before: `already_exists` and `access_denied`); no `.rsmb-` file left in `<S>` |
| H | `docs_match_the_table` (unchanged test) | `docs/commands.md` | the section ``## `access` `` with its syntax and three examples |

**`tests/slow.rs`** (parallel, shared lock; times only as no-hang bounds)

| # | Test | How observed | Asserts |
|---|---|---|---|
| S81 | `access_slow` | `Client::tapped(Ep::Slow)` | `access upload` 4 / 1; `access -e /tree` 19 / 2; `access -e /fanout`: 2,000 objects, all with a mask, **4,007 requests** (5 + 2 × 2,001), within `NO_HANG` |
| S82 | `access_edge` | `Client::tapped(Ep::Edge)` | `access -e /fanout`: 2,000 objects with a mask; requests = the listing on edge (as `ls /fanout` there, counted in the same session first) + 4,002; never more outstanding than granted (the credit balance helper of `edge_and_encrypted`); within 30 s |
| S83 | `cut_mid_access` | `Client::tapped(Ep::Slow)`, `access -e /many`, `wait_requests("CREATE", 40, …)`, `drop_connections()` | ok; exactly 10,000 `access` objects, each path once; the tap saw exactly 1 new connection and requests on two connections, the listing's QUERY_DIRECTORY on both (it ran again from its start) |
| S84 | `interrupt_mid_access` | as `interrupt_with`: `access -e /many` at the slow endpoint, after 40 CREATEs, `kill -INT` then (a new client) `kill -TERM` | exit 130, kind `interrupted`, within `NO_HANG` |
| S85 | `put_f_refused_by_the_rename` | `Client::tapped(Ep::Slow)`, a `Scratch` with a sticky directory (1777, root's) holding root's 0600 file of 300,000 bytes | `put -f big.bin <dir>/root.bin`: `access_denied`, the message holds `refused to replace it`; the WRITEs went out (the tap counts them); afterwards no `.rsmb-` file in the directory and the file's sha256 unchanged (A14's failure-matrix row; the 50 MiB timing of 2.13 stays a measurement, not a test) |

**`tests/serial.rs`** (exclusive lock; wall times)

| # | Test | How observed | Asserts |
|---|---|---|---|
| X81 | `timing_access` | `tapped_cost`-style runs at the slow endpoint, netem drops checked as the other timing tests do | `access upload` median of 5 **< 260 ms** (measured 202); `access -e /tree` median of 5 **< 500 ms** (2 round trips); `access -e /fanout` median of 3 **< 4 s** (probes 2.1 s + the listing); prints the three medians for `docs/performance.md` |

Where each demand is enforced: the prediction matches the command F81, F82 (both logins' kinds
of rights), F83 (share-level, logins, encryption) · the known disagreements pinned F81, F82,
`rights_from_masks` · the cost and "cd and ls unchanged" F84, S81, S82 · the proxy F81 to F87 in
the proxied run · a cut S83 · an interrupt S84 · the cache F86 · the text and JSON F85,
`access_lines` · the MxAc wire form `rights_request_shape`, `maximal_access_answers` · the
cascade and its retries F84 (`nosuch/x`, `listonly`, `mixed`), F81 (`mixed`'s answers in their
places), `chain_answers_up_to_the_first_failure`, `chain_retries_keep_their_places` · A14 F87,
S85 · the DC's attribute hygiene F82's first assertion.

### 7.5 Not in the gate, and why

A Windows server (none in the environment; section 10). `huge/` with `-e` (20.9 s at the fast
endpoint; the shape is the same as `many/`). Times through the proxy at the slow endpoint
(microsocks' Nagle, measured in 2.8; the counts are asserted in the proxied run). A silent server
during `access` (the existing rule, tested for other commands). The edge server at 100 ms
(computed, 2.9). A second client holding an oplock while `access` opens the file (section 10).
The 50 MiB `put -f` of 2.13 (its outcome is S85, with 300,000 bytes). A call error of a chain that
is not a lost connection, and a task that panics (A19): neither can be made to happen against a
real server; the code paths are a few lines and the review reads them.

## 8. Tasks, in order

Each task ends with its tests green and `./test.sh fast`; the full `./test.sh` after T4 and T6.
Documentation is written with the task that makes it true. A14 comes first, so that every later
test already runs against it (finding 8).

| # | Task | Needs | Builds | Tests added |
|---|---|---|---|---|
| T1 | `Remote::stat` (A14) | — | 5.10; the POSIX fixture of 6.1 and `posix_fixture` (7.1), which F87 needs; `docs/transfers.md` (the three tables of 5.10) | F87 and S85 (written first; F87 fails before the change) |
| T2 | The probe | T1 | 5.2, 5.3 in `src/session.rs`. First by hand: `access`-like calls against `upload`, `forbidden`, `nosuch/x` beside the masks of 2.3 | `maximal_access_answers`, `rights_request_shape`, `chain_answers_up_to_the_first_failure`, `chain_retries_keep_their_places` |
| T3 | The command | T2 | 5.4 to 5.7 in `src/commands.rs`; the `docs/commands.md` section (5.8) so that `docs_match_the_table` passes | `rights_from_masks`, `access_lines`; F84, F85, F86 |
| T4 | The prediction tests | T3 | the rest of the harness of 7.1 (`nt_fixture`, `outcomes`, `Scratch` on the DC); F81 to F83 against 2.7 and 2.13. **The full `./test.sh`**. If a mask or an outcome differs from section 2, find out why before changing a number | F81, F82, F83 |
| T5 | Slow and serial | T4 | S81 to S84, X81 | S81 to S84, X81 |
| T6 | Documents | T5 | section 9; the measured step times and X81's medians into the documents. **The full `./test.sh`** | — |

## 9. Documents

| Document | What changes |
|---|---|
| `docs/commands.md` | the section ``## `access` `` (5.8); "Quoting and paths": `-e` among the flags without a value; "Timeouts and reconnect": `access` among the commands that run again; "Completion": `access` completes remote paths; "Differences from smbclient-ng": `acls` against `access`; the `shares` section's sentence "Per-directory access is not shown" becomes a pointer to `access`; `put`: the changed errors of 5.10 |
| `docs/scripting.md` | the `access` object (with `replace` and `attributes`), "Asking before acting" (with the exit-code difference between a path alone and an entry of `-e`, and no output before the end), the example (5.9) |
| `docs/architecture.md` | a section "`access`": MxAc on a CREATE, the desired access and why, the parent first in one compound, the cascade (and the CLOSE condition of A5) and the retry (`settle`), the width, the errors of a whole chain (A19), the rule's place (`allowed`) and that it is the documented rule of `docs/commands.md`; "Remote": `stat` asks for attributes only; "The command layer": `-e`; "The retry" (the list of navigation commands that may run again, now lines 413 to 416): `access` among them |
| `docs/known-limitations.md` | a section "`access`": what it does not tell (2.7), K1, K2, K3, K4, K7 with their cases, the `?` cases (`rmdir` under a directory that lets nothing change; a directory's DOS read-only attribute), no output before the end of a large `-e`, the answer is a moment's, Samba only (Windows unverified: whether a rename with replace checks DELETE, whether MaximalAccess includes DELETE through the parent's DELETE_CHILD, whether `FILE_READ_ATTRIBUTES` opens what bypass-traverse reaches), the cost of `-e` on large directories and on servers with few credits, `mangled/` (four names `get` cannot open). "Out of scope": ACL display stays out; `access` is not it. The `shares` paragraph (now line 87: "rsmb shows no per-directory access …") points to `access` instead; A14's changed `put` errors where `put` is described |
| `docs/transfers.md` | "The two paths": the check of a large `put` opens with `FILE_READ_ATTRIBUTES`; the overwrite table, the rename's wording table and the failure matrix as 5.10 lists (A14, F87, S85) |
| `docs/performance.md` | the costs of 2.9 and X81's medians; the proxy's Nagle effect on separate compounds (2.8, both values); "Not done" (now line 447: "Per-directory access in `shares` or `ls` …") points to `access` |
| `docs/testing.md` | F81 to F87, S81 to S85, X81, the unit tests; the harness (`posix_fixture`, `nt_fixture`, `outcomes`, `Scratch` on the DC); the measured step times |
| `docs/test-environment.md` | the fixtures the access tests make at run time (6.1, 6.2), that `smbcacls` and the admin password are used inside the DC at run time, that the DC is the only server with NT ACLs; **how the DC keeps DOS attributes** (`xattr_tdb` by inode, the leak through `rm` outside Samba and through a rename with replace, the reset and the clearing `Scratch` does, why no test replaces a read-only file there: 2.13); `mangled/`: the sentence of 2.11 |
| `docs/library-decision.md` | "What rsmb builds on top of `smb2`": the MxAc context through the crate's public `create_context` codec, no patch; the related flag on every request after the first of a compound, and Samba's cascade after a failed CREATE |
| `docs/development.md` | nothing new except: a fixture a test needs only for itself is made at run time in its `Scratch` (`posix_fixture`, `nt_fixture`) |
| `docs/brief.md` | amendment 10: the user's request for this stage, in the user's words as the orchestrator relays them (written by the orchestrating session, not by the implementing agent) |
| `README.md` | the command list and the status line (six stages) |
| `AGENTS.md` | **an index row for `docs/plans/stage-6.md`** (what it answers, in the style of the other plan rows), and the rows of `commands.md` (also "one section per command (19)" becomes 20), `scripting.md`, `architecture.md`, `known-limitations.md`, `testing.md`, `test-environment.md`, `transfers.md` extended with `access` and A14; a principle only once it has proved itself (a candidate: "ask the server for its verdict instead of evaluating its rules in the client", with 2.6 as evidence) |
| `docs/reviews.md` | the review of Stage 6 (step 4 of the loop) |

## 10. Assumptions this plan could not verify

1. **Windows.** Whether a Windows server's MaximalAccess includes DELETE granted by the parent's
   FILE_DELETE_CHILD (Samba's does: `D/file.txt`); whether its rename with replace checks DELETE
   on the replaced file (expected; Samba does not: K1, K2); whether a `FILE_READ_ATTRIBUTES`
   open succeeds on every object a user reaches; whether its MaximalAccess on a share with
   read-only share permissions drops the write bits as Samba's `read only` does. The documents say
   "on Samba".
2. That an attribute-only open breaks no oplock or lease another client holds (Windows and Samba
   treat it as a stat open; no second client was tried).
3. That the probe changes nothing on the server (no write was sent; access times were not
   compared).
4. That the product counts what the probe counted: same compounds, same order; F84 and S81
   confirm.
5. A server that answers without MxAc (every column `?`): none in the environment;
   `maximal_access_answers` covers the parsing.
6. That an ACE granting READ_DATA without SYNCHRONIZE (K4) is rare outside hand-made ACLs.
7. That a server accepts a related CREATE after a CLOSE in one compound and answers it on its own
   merits when everything before it succeeded: only Samba 4.23.8 was tried (finding 6).
8. 256 in flight is the measured point on this Samba at 200 ms; not tuned for other servers.
9. Through microsocks at the slow endpoint a retried batch of single compounds costs one more
   round trip (measured with the probe, not with the product).
10. That every object Samba creates through SMB gets a fresh DOS attribute record on the DC, so
    only objects made outside Samba can inherit a stale one (seen for every `put` and `mkdir` of
    these runs, not proved for every way of creating). A failed CLOSE was never produced, so the
    CLOSE condition of A5 is a rule from the protocol's cascade, not from a measurement.
11. How Windows treats a directory's DOS read-only attribute in its MaximalAccess (A7 shows `?`
    on whatever the server drops for it).

## 11. Adversary findings and what changed

The adversary attacked the first version of this plan with its probes, the release binary of
`0f7bd3f` and a throwaway build of A14. Each finding was reproduced by the planning agent before
the plan was changed (2.13 has the measurements). It also checked and found correct: the masks of
2.3 to 2.6, the cascade of 2.8, K1 to K5 and the other prediction rows it repeated, the chained
`/fanout` (4,002 requests, 443 ms fast, 2,079 ms slow), one compound through the proxy (202 ms),
the crate's desired accesses, `Scratch::new`'s modes, the shares of `samba-private`, F84's
arithmetic, the example paths, `-e` as a free flag and `--`, `cd` and `ls` untouched, and that
rerun, interrupt and `in_flight` follow `ls` and `shares`.

The user answered the plan's two questions on 2026-11-19: **A14 stays in this stage**, with
finding 4's evidence shown and accepted; **`rmdir` has three answers**: `-` without DELETE on the
directory, allowed with DELETE and a parent that lets you change something, `?` (`null`) when the
directory grants DELETE and its parent allows no change (POSIX refuses, NT ACLs allow). Both are
decisions now (A14, A7); section 11 of the first version is gone.

| # | Claim | Evidence (reproduced) | What changed |
|---|---|---|---|
| 1 (blocks) | The NT fixture contaminates the DC: DOS attributes live in `xattr_tdb` by inode; `rm -rf` outside Samba and Samba's own rename with replace leave the record, and the next object made outside Samba on that inode inherits it | 2.13: a `+r` file removed with `rm -f`, the next file on inode 18889957 showed `R`; after `rename new.txt old.txt -f` over an `R` file the next new file took the old inode and showed `R`; the main servers do not leak (attribute 0x80 on the reused inode) | A15: the NT fixture resets the attribute of every object it makes (`setmode -rsh` over SMB, 6.2); `Scratch` on the DC resets its directory at creation and clears everything in it before removal (7.1); the gate never replaces a read-only file on the DC (the two `dosro` objects left the NT fixture; K1 is pinned on POSIX); F82 first asserts the scratch directory's mask and attributes; `docs/test-environment.md` documents the mechanism; section 10's item about the tdb's size is replaced by item 10 |
| 2 | A realistic drop box makes an allowed column wrong | 2.13: `J` (list, add file; inherited READ for the creator, no CREATOR OWNER), mask 0x001200ab: `put notes.txt J/` fails at the rename with `left_behind` | **K7** (unsafe) in 2.7, `J` in the NT fixture, a pinned row of F82, `rights_from_masks`, the help text, `docs/commands.md` and `docs/known-limitations.md`; the sentence about wrong allowed columns now names three (K3, K4, K7); section 10's item 7 (not constructed) removed |
| 3 | A directory with the DOS read-only attribute loses WD, AD, DE in its mask, but Samba does not enforce them | 2.13: `rodos` attributes 0x11, mask 0x000c00e9; `put`, `mkdir` in it, `put -f` over its file, `rmdir` of its subdirectory all work; only `rmdir rodos2` is refused | taken the second way the adversary offered: `Rights` carries the CREATE answer's attributes; A7 shows `?` for `put` and `mkdir` of a read-only directory and for `put` over its files when the mask lacks the bit; `rmdir` of it stays `-`. `rodos`, `rodos2` in the POSIX fixture, unknown rows in F81, cases in `rights_from_masks`; the help text no longer says the answer includes the DOS read-only attribute |
| 4 | A14 changes more of `put` than K6 | 2.13's table: `put` without `-f` over an unreadable file `already_exists` (was `access_denied`); `put` onto a directory one may not read `is_a_directory` on both paths (the small path said `already_exists`, the large `access_denied`); 50 MiB `put -f` over a file that cannot be replaced: 1,430 ms → 7,287 ms, refused by the rename, temporary file removed | shown to the user, accepted; A14 lists every effect; 5.10 names the three tables of `docs/transfers.md`; F87 asserts every changed error; S85 asserts the refusal by the rename with nothing left behind |
| 5 | The cascade's retries are barely tested: `listonly` fails every entry alike, so a wrong index or a skipped retry passes | Samba's listings hide what cannot be opened, with one exception found for the revision: a FIFO is listed and refused (0xC0000279); `mixed` (`a.txt`, `b.fifo`, `c.txt`, `d`) cuts its chain at the third path and the two after it answer alone (2.13, 14 requests in 2 flights) | `settle` is a pure function with its own unit test (`chain_retries_keep_their_places`); `mixed` in the POSIX fixture; F81 asserts `c.txt` and `d` by name; F84 asserts `-e <S>/listonly` 13 / 3 and `-e <S>/mixed` 19 / 3 |
| 6 | A failed CREATE after a failed CLOSE may be the CLOSE's status passed on | not reproduced: no CLOSE of a successful CREATE could be made to fail. The rule follows from the cascade seen in 2.8 (a related request after a failed one gets that status), and adopting it costs nothing | A5 trusts a failed CREATE only when it is the chain's first or the CLOSE before it succeeded; `answered` takes both statuses; unit cases; section 10 item 7 (a related CREATE after a CLOSE, Samba only) and item 10 |
| 7 | `rights_inner` leaves call-level errors open | by reading: `execute_compound`'s outer error and a `JoinError` were unspecified; `access_inner` ignores them | A19: a call error that is not a lost connection gives every path of its chain that error (it never reached the wire); a `JoinError` marks the connection dead and fails with `other` |
| 8 | Task order: F81 asserts A14 in T3, A14 arrives in T4 | by reading the first version's section 8 | A14 is T1 (with the POSIX fixture, F87 and S85); every later task runs against it |
| 9 | Forgotten documents | `docs/known-limitations.md:87` ("rsmb shows no per-directory access"), `docs/performance.md:447` ("Per-directory access in shares or ls … per share only"), `docs/architecture.md:413-416` (the commands that may run again), `AGENTS.md` ("one section per command (19)") read as quoted | all four in section 9 |
| 10 | 2.8's 380 ms is bimodal | repeated: three runs of 7 rounds, 2 or 3 rounds near 243 ms and the rest near 403 ms (medians 403.4, 402.8, 403.1 ms); one compound 201.9 to 202.9 ms | 2.8 gives both values |
| 11 | Scripts: a path alone is exit 1, the same path as an entry of `-e` an object with `error` and exit 0; the JSON key `put` means two things and the example mixed them | by reading A12, 5.5 and the example | a file's JSON key is `replace` (A10); `docs/scripting.md` and the manual state the exit-code difference; the example selects by `is_dir` |
| 12 | No progress on a large `-e` | 2.9: `-e /huge` 20.9 s at the fast endpoint with nothing printed | documented, no progress line (A11): printing only at the end is what lets the command run again after a cut, and `ls` and `tree` behave the same; the manual, `docs/commands.md`, `docs/scripting.md` and `docs/known-limitations.md` give the times |

Nothing was rejected.

## Deviations during implementation

Recorded by the implementing agent on 2026-11-19. Every mask and every outcome of sections 2.4 to
2.7 and 2.13 that the gate checks came out as measured while planning (`access_predicts_posix`:
38 masks and 52 command lines; `access_predicts_nt_acls`: 22 masks and 22 lines, the scratch
directory 0x001f01ff with attributes 0x10; both direct and through the proxy). What differs:

| # | Deviation | Evidence |
|---|---|---|
| W1 | **`access -e <S>/listonly` shows no entry and costs 7 requests in 2 flights**, not 13 in 3 with `a.txt` and `b.txt` as `?` lines (5.3, F81, F84, F85). F84 asserts 7 / 2 with no object and adds `access <S>/listonly/a.txt`: `access_denied`, 4 / 1 (the second CREATE of the chain refused); F81 asserts the same `access_denied` and that `get listonly/a.txt` fails, instead of an object with `error`; F85's byte-for-byte line with an error tail is `-e <S>/mixed` (`-  ?   ?         ?      b.fifo  (other)`) instead of `(access denied)` (the format of that tail stays in `access_lines`). The cascade's retries are still asserted by `mixed` (19 / 3, the answers in their places) | Samba lists nothing in a directory the user may read but not traverse (root's 0744): it cannot look at the entries. `ls upload/plan6probe/listonly` printed nothing with exit 0, `smbclient … -c 'ls upload/plan6probe/listonly/*'` said `NT_STATUS_NO_SUCH_FILE`, and the first run of F84 got `CREATE 2, QUERY_DIRECTORY 3, CLOSE 2`: the listing and the directory's own probe. The plan's 13 / 3 was computed (2.9 says "the first refused"), not measured through the product; 2.7's "`ls listonly` ok" is true, the listing is empty. `docs/commands.md` and `docs/known-limitations.md` say it |
| W2 | F83 on `readonly`: `put` and `mkdir` of `rodos` and `rodos2` are `null`, not `false` | A7's rule for a directory with the DOS read-only attribute (2.13): the mask lacks FILE_ADD_FILE and FILE_ADD_SUBDIRECTORY and the CREATE answer has 0x11, so the column is `?`; F83 was written before `rodos` joined the fixture. Every other `put`, `mkdir`, `replace`, `rm` and `rmdir` is `false` as planned |
| W3 | The JSON lines in the documents are the real ones: keys in alphabetical order, and a plain file's `attributes` on the main servers is 128 (FILE_ATTRIBUTE_NORMAL), not 32 as in 5.5's illustration; the `?` example is the FIFO of a throwaway directory (`error: "other"`) | `serde_json` without `preserve_order` writes keys sorted, as every other object of rsmb; captured from `access /upload` and `access -e` of a directory with `mine.txt` (tester's, 0644) and a FIFO on `samba`, removed afterwards. `access_output` compares keys as sets |
| W4 | An answer that does not unpack is `other`, `<path>: malformed answer from the server`, not `malformed()` | `malformed()`'s message names a directory listing. No test (Samba never sends one) |
| W5 | X81 takes its medians from `Reply::took` of a direct `Client` at the slow endpoint (5, 5 and 3 runs) after a `tapped_cost` of 5 × `access upload` (20 / 5), not the (n − 0) ÷ n process-time method of `per_command` | `access` prints after its last answer; a command's own time from line sent to `done` is what the bound is about, and `-e /fanout` (2 s) does not need the start-up subtracted. Medians on 2026-11-19: 201.6 ms, 404.5 ms, 1,921 ms (bounds 260, 500, 4,000; the plan expected 202, about 400 and 2.1 s plus the listing) |
| W6 | The gate after T2 was not run on its own: T2's unit tests passed (102 then), and `./test.sh fast` ran after T1, after T3 (covering T2) and after T5; the full `./test.sh` after T4 and T6 | T2 adds no command; its code is reached only through T3 |
| W7 | F87 failed before A14 exactly as 5.10 says: its first line, `put -f big.bin big.bin`, was `access_denied` (`/upload/rsmb-test-…/big.bin: access denied`); after the change of `Remote::stat` all of F87 passed | `cargo test --release --test fast put_over_a_file_without_synchronize` before and after the change |
| W8 | **Environment, not the plan:** `iperf3`, a documented host requirement, is no longer installed on this host, so `check.sh` failed its `iperf3 through wan` check. For the runs of this stage only, the `iperf3` binary of the `samba` image (Alpine, musl) with its libraries was copied into a scratch directory outside the repository and run through its musl loader on `PATH`; nothing on the host or in the repository changed. The orchestrator or the user decides how to restore it | `iperf3: command not found`; `pacman -Q iperf3`: not installed; with the copy `check.sh` passes (39.5 Mbit/s over 2 s) |
| W9 | **Environment, not the plan:** `completion_costs_slow` (Stage 1) fails in most full `tests/slow` runs on this host today: the first Tab in `huge/` at the slow endpoint runs into its 5 s bound (BEL) while the other slow tests share the link. It passes alone. **The committed code (`0f7bd3f`, from `git archive` into a scratch directory) fails the same way**, and so does this stage's code without S85 and without S81 to S84; the host had a load average of 7 to 8 throughout (a video call and other sessions). One full `tests/slow` run with this stage's code passed completely | 14 full runs of `tests/slow` with this stage's code on 2026-11-19 (some with tests skipped, below): 13 failed `completion_costs_slow`, 1 passed everything; `0f7bd3f`: failed the same; alone: ok in 5.6 s; `ls many` and `ls huge` at slow with start-up: 5.2 s alone |
| W9b | **Open, reported for the review:** `edge_and_encrypted` (Stage 2) failed twice: its `get -r /fanout` on edge got `connection_lost`, `no complete reply within the response deadline` (the crate's `Error::Timeout`: one request unanswered for the 30 s deadline on a connection that was otherwise answering), and `proxy_failures` (Stage 4) once. The edge failure came only in runs that held S82 (`access_edge`, `access -e /fanout` on the same edge server): 2 of 6 full runs with it, 0 of 9 without it (the T4 gate and its rerun, before S81 to S84 existed; 1 without S85; 1 of `0f7bd3f`; 1 with S81 to S84 skipped; 4 with S82 skipped). It did not reproduce directly: `edge_and_encrypted` passed 3 of 3 beside S82 alone, and 3 of 3 while a loop ran `access -e /fanout` on edge 159 times back to back (`get -r /fanout` 1.9 s each time). The probe asks for no oplock and opens only the directories of `fanout/`; what held a request of the other session for 30 s is not known. S82 stays in `tests/slow` as planned; moving it to `tests/serial` would hide the interaction rather than explain it | the runs listed; the error object of `get -r /fanout` in the gate's log (`connection_lost`, after 1,382 of 2,000 files) |
| W10 | The budget of 7.2, measured: `./test.sh fast` **77 s counted of 150** (expected about 70): `check.sh` 32 s, unit tests 2 s, `tests/fast` 22 s (expected 17), through the proxy 21 s (19). The full run, the sum of its steps because it could not run to its end (W8, W9): **about 506 s of 600** (expected about 500): `check.sh` 32 to 33 s, unit tests 2 to 3 s, `tests/fast` 18 to 19 s, through the proxy 19 s, `tests/slow` 43 s (51 tests, `completion_costs_slow` skipped; expected 37), `tests/serial` 387 s (28 tests; expected 397), tshark not run (3 s after Stage 5). No step over 540 s; S83 keeps `/many` | `./test.sh fast` after T5: `=== all passed (fast): total 79 s, counted 77 s of 150 s budget: within budget`; the full runs' step lines up to `tests/fast through the proxy` (`the proxy logged 184 connections`); `tests/slow` without `completion_costs_slow`: `50 passed … finished in 43.02s`; `tests/serial`: `28 passed … finished in 386.90s`, 387 s with the build check |

### After the review

Recorded by the fix agent on 2026-11-19, from the review's findings 1 and 3 (`docs/reviews.md`,
Stage 6).

| # | Change | Evidence |
|---|---|---|
| W11 | **A15's clearing was not enough: on the DC everything is now removed through Samba.** `setmode -rsh` clears R, S and H but never the directory bit, so a directory made over SMB (F82's `mkdir C/new`, every run) and removed outside Samba left a record with D, and the next file made in the container on that inode listed as a directory. `Scratch`'s `Drop` on the DC now clears the flags and runs `deltree` over SMB as the administrator before `rm -rf`, and `./test.sh`'s stale-directory step does the same for every `upload/rsmb-test-*` on the DC. `nt_fixture` lists its 22 objects over SMB after clearing them and fails, naming the path, when one lists as other than it is. `Scratch::new` on the DC checks that its directory lists as `D (10)` with the ACL Samba derives from mode 1777 and otherwise removes it over SMB and makes it again, for the records a rename with replace leaves (F82's `put -f notes.txt E/file.txt` and `F/file.txt`: two NT-ACL-only records per run). The DC was recreated to drop the records that existed | The orchestrator, as `RSMB\Administrator`: `mkdir n1 n2 n3`, `setmode -rsh` over SMB, `rmdir` outside, eight files outside: `f3`, `f4`, `f5` took the inodes 18891526, 18891558, 18891628 and listed `D 0`; `setmode f3 -rsh` left `D (10)`; `deltree` of the parent failed with `NT_STATUS_NOT_A_DIRECTORY`. The fix agent: with the old DC, F82 failed in `nt_fixture` with "D/file.txt is a file made outside Samba but lists over SMB as `D`"; `deltree` over SMB left no record for any of five objects (python `tdb` in the container), and `deltree` refuses an `R` file with `NT_STATUS_CANNOT_DELETE` (hence the `setmode` first). A file's record planted on the inode the next directory takes (python `tdb`): F82 printed `came up with stale records on the DC; made again` and passed. Stale records (device and inode of no file): 293 on the share's device before, 83 with a DOS attribute, 16 with the directory bit; 0 after recreating the container; after the runs of the fix see `docs/reviews.md` |
| W12 | **The time of `access -e /huge` is the product's, about 22 s at the fast endpoint**, not 2.9's 20.9 s, which is the probes alone (A11, finding 12 and the documents quoted it as the command's) | Through rsmb, process time of `-N --json -C 'use share' -C 'access -e /huge'`: 22,165 and 22,497 ms (load about 1, the orchestrator), 21,496 and 23,918 ms (load about 10, the reviewer). The 80 s at 100 ms each way stays, labelled as computed for the probes alone |
