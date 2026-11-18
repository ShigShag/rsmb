//! The command table, `run_line`, every command, the output sink (text or JSON lines), the help.
//! Adding a command: a `CmdId` variant, a table entry, a function, a `match` arm, a test, a
//! section in docs/commands.md (docs/development.md).

use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::Flow;
use crate::session::{Entry, Error, KIND_NAMES, Kind, Result, Session, display_path, safe};
use crate::shell::tokenize;
use crate::transfer;
use crate::walk;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdId {
    Shares,
    Use,
    Cd,
    Pwd,
    Ls,
    Tree,
    Find,
    Get,
    Put,
    Cat,
    Mkdir,
    Rm,
    Rmdir,
    Lcd,
    Lpwd,
    Lls,
    Refresh,
    Help,
    Exit,
}

/// What a positional argument completes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    Command,
    Share,
    RemoteDir,
    RemotePath,
    LocalPath,
    LocalDir,
}

#[derive(Debug)]
pub struct Cmd {
    pub id: CmdId,
    pub name: &'static str,
    pub syntax: &'static str,
    pub summary: &'static str,
    pub details: &'static str,
    pub flags: &'static [(&'static str, &'static str)],
    pub args: &'static [Arg],
    pub required: usize,
    pub examples: &'static [(&'static str, &'static str)],
    /// May run again from its start after the connection was replaced.
    pub rerun: bool,
}

pub static COMMANDS: &[Cmd] = &[
    Cmd {
        id: CmdId::Shares,
        name: "shares",
        syntax: "shares",
        summary: "list the shares of the server, hidden ones included",
        details: "Asks the server for its share list (srvsvc NetrShareEnumAll on IPC$) and keeps \
                  it for completing 'use', then asks for your access to every share: a \
                  TREE_CONNECT and TREE_DISCONNECT per share, up to 16 shares in one compound, \
                  as many compounds at once as the server's credits allow (one more round trip \
                  on the test server). Costs 7 + 2 x shares requests the first time on a \
                  connection, 6 + 2 x shares after; the probed trees are disconnected. Prints \
                  one line per share: name, type (disk, printer, device, ipc), access, comment. \
                  Access is what the server reports for this login at share level: rw (read and \
                  write), r- (read only), -w (write only), -- (refused), ? (no answer). It says \
                  nothing about single directories or files: a file or folder inside can still \
                  refuse you. Nothing is created on the server. With --json one 'share' object \
                  each, access as \"rw\", \"r\", \"w\", \"none\" or null. If the connection is \
                  lost while the access is asked, the list is still printed, with ? (null) for the \
                  shares not answered, then the error connection_lost (exit 1); the command is not \
                  run again, the next one reconnects.",
        flags: &[],
        args: &[],
        required: 0,
        examples: &[(
            "shares",
            "list every share, hidden ones (ending in $) included, with your access to each",
        )],
        rerun: true,
    },
    Cmd {
        id: CmdId::Use,
        name: "use",
        syntax: "use <share>",
        summary: "select a share; the working directory becomes its root",
        details: "Connects to the share the first time it is used on a connection (1 request), \
                  then costs nothing. A share that requires encryption makes the client encrypt \
                  everything on this connection from then on, for every share. Errors: not_found \
                  (no such share), access_denied.",
        flags: &[],
        args: &[Arg::Share],
        required: 1,
        examples: &[(
            "use share",
            "select the share 'share' and stand at its root",
        )],
        rerun: true,
    },
    Cmd {
        id: CmdId::Cd,
        name: "cd",
        syntax: "cd [<dir>]",
        summary: "change the working directory (default: the share root)",
        details: "Paths use / or \\ as separator; a leading separator starts at the share root; \
                  '.' and '..' work. Going to the root or to a directory above the current one \
                  sends nothing. Any other target is checked on the server with one open and \
                  close in a single round trip (2 requests). The working directory keeps the \
                  spelling as typed. Errors: not_found, not_a_directory, access_denied; the \
                  working directory is unchanged after a failure.",
        flags: &[],
        args: &[Arg::RemoteDir],
        required: 0,
        examples: &[
            ("cd tree/d0", "enter tree/d0 below the working directory"),
            ("cd /", "go to the share root (sends nothing)"),
            ("cd ..", "go one directory up (sends nothing)"),
        ],
        rerun: true,
    },
    Cmd {
        id: CmdId::Pwd,
        name: "pwd",
        syntax: "pwd",
        summary: "print the server, share and working directory",
        details: "Prints //host/share/dir (//host without a share, //host/share/ at the root). \
                  Sends nothing. With --json one 'pwd' object.",
        flags: &[],
        args: &[],
        required: 0,
        examples: &[("pwd", "print where the session stands")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Ls,
        name: "ls",
        syntax: "ls [<dir>]",
        summary: "list a directory (default: the working directory)",
        details: "Always asks the server and refreshes the cached listing that completion uses. \
                  On Samba one round trip (5 requests) for a directory of up to about 15.9 MiB \
                  of entries. One line per entry, sorted by name: 'd' or '-', the size, the \
                  modification time in UTC (YYYY-MM-DD HH:MM:SS, '-' if unknown), the name with \
                  '/' after a directory. With --json one 'entry' object each. Listing a file is \
                  an error (not_a_directory).",
        flags: &[],
        args: &[Arg::RemotePath],
        required: 0,
        examples: &[
            ("ls", "list the working directory"),
            ("ls /sizes", "list the directory sizes below the share root"),
            (
                "ls \"special/dir with space\"",
                "list a directory whose name has spaces",
            ),
        ],
        rerun: true,
    },
    Cmd {
        id: CmdId::Tree,
        name: "tree",
        syntax: "tree [-L <n>] [<dir>]",
        summary: "draw a directory and everything below it as a tree",
        details: "Walks the directory (default: the working directory) and everything below it, \
                  then draws it: the start from the share root, then one line per entry, depth \
                  first, by name within a directory, '/' after a directory, then the line 'N \
                  directories, M files'. -L n stops n levels down (1: the directory's own \
                  entries); a directory at the last level is drawn without its content. Cost on \
                  Samba: one listing (4 requests, 1 round trip) per directory, 16 at a time; a \
                  directory above 1 MiB of entries costs one round trip more per MiB. A directory \
                  that denies listing, one with the file id of a directory already listed (a link \
                  back up, or a second path to one directory: the first in breadth-first order is \
                  listed), one that leads above the start (to its parent, which the walk does not \
                  list) and one that vanished meanwhile are drawn with a note and reported at \
                  the end on stderr; they do not change the exit status. Any other failure of \
                  one directory (also a name the server sent with an unpaired UTF-16 surrogate, \
                  which cannot be sent back) makes the command fail (exit 1) after the drawing. With --json one \
                  'entry' object (with depth) per drawn entry in the same order, then 'denied' and \
                  'skipped' objects and one 'walk_summary'. At most 500000 entries: past that \
                  nothing is drawn and the command fails (use -L or find). The listings feed the \
                  cache that Tab reads. Errors: not_found, not_a_directory, access_denied (the \
                  directory itself), usage; other (past the limit, or a directory that failed). \
                  Run again once after a lost connection (it prints \
                  nothing before the walk has ended).",
        flags: &[(
            "-L <n>",
            "at most n levels below the directory; 1 shows its entries only",
        )],
        args: &[Arg::RemoteDir],
        required: 0,
        examples: &[
            (
                "tree /tree/d0/d0",
                "draw a directory and everything below it",
            ),
            (
                "tree -L 2 /tree",
                "two levels only (one directory in it denies listing: reported, still exit 0)",
            ),
        ],
        rerun: true,
    },
    Cmd {
        id: CmdId::Find,
        name: "find",
        syntax: "find [<dir>] [-name <glob>] [-iname <glob>] [-type f|d] [-size [+|-]<n>[BKMGTP]] \
                 [-maxdepth <n>]",
        summary: "list what is below a directory, filtered by name, type, size and depth",
        details: "Walks the directory (default: the working directory) and everything below it \
                  and prints every entry that passes all the tests given, one path per line from \
                  the share root ('/' after a directory); the directory itself is not printed. \
                  The order is the same on every run: breadth first, each directory's entries by \
                  name, a directory before what is in it; the matches of a directory are printed \
                  as its listing arrives. Flags and the directory may come in any order. Cost on \
                  Samba: one listing (4 requests, 1 round trip) per directory, 16 at a time; the \
                  tests cost nothing. A directory that denies listing, one with the file id of a \
                  directory already listed, one that leads above the start (to its parent, which \
                  the walk does not list) and one that vanished meanwhile are reported at the end \
                  (on stderr; 'denied' and 'skipped' objects with --json) and do not change the \
                  exit status; any other failure of one directory (also a name the server sent \
                  with an unpaired UTF-16 surrogate, which cannot be sent back) makes the command \
                  fail (exit 1) after the rest. With --json one 'entry' object (with depth) per match, then the \
                  reports and one 'walk_summary'; without a walk_summary the result is not whole. \
                  A closed stdout (find ... | head) stops the walk. The listings feed the cache \
                  that Tab reads. Errors: not_found, not_a_directory, access_denied (the directory \
                  itself), usage (a bad flag value). Run again after a lost connection only while \
                  it has printed nothing.",
        flags: &[
            (
                "-name <glob>",
                "the name matches the pattern: * ? [a-z] [!x], \\ escapes",
            ),
            ("-iname <glob>", "the same without regard to letter case"),
            ("-type <f|d>", "files only, or directories only"),
            (
                "-size <[+|-]n[BKMGTP]>",
                "files of more than (+), less than (-) or exactly n units: B or c bytes, K M G T \
                 P powers of 1024",
            ),
            (
                "-maxdepth <n>",
                "at most n levels below the directory; 1 is its entries only",
            ),
        ],
        args: &[Arg::RemoteDir],
        required: 0,
        examples: &[
            (
                "find /tree -name file.txt",
                "every entry called file.txt below /tree",
            ),
            ("find /sizes -size +1M", "files larger than 1 MiB"),
            (
                "find /special -iname '*.TXT' -type f",
                "files ending in .txt in any letter case",
            ),
            (
                "find /tree -maxdepth 2 -type d",
                "directories at most two levels down",
            ),
            (
                "find /cjk -maxdepth 1 -name '简体中文*'",
                "a pattern with non-Latin characters",
            ),
            (
                "find /special -name '[[]abc[]].txt'",
                "a name with a literal [",
            ),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Get,
        name: "get",
        syntax: "get [-r] [-f] <remote> [<local>]",
        summary: "download a file, or a directory with -r",
        details: "Downloads a remote file into the local working directory, into <local> if that \
                  is an existing directory or ends in /, else to the path <local>. The data goes \
                  to a temporary file .rsmb-<hash>.part beside the target, is synced, and is \
                  renamed to the target only when every byte has arrived: an interrupted or \
                  failed get leaves nothing under the target name. An existing local file is not \
                  replaced without -f (0 requests; the interactive shell asks); a directory never. \
                  Cost on Samba: 6 requests plus one READ per 512 KiB (7 in 3 round trips up to \
                  512 KiB). With -r the remote directory and everything below it go to \
                  <local>/<name>/... (<local> must be an existing directory): listings 16 at a \
                  time (4 requests, 1 round trip each), files of at most 256 KiB 16 at a time (3 \
                  requests, 1 round trip each), larger ones one after another; one 'transfer' \
                  line per file, then one 'summary'; anything not transferred (a directory that \
                  denies access, a link back to a directory already visited, a name the local \
                  file system cannot have, an existing target without -f) is one error with its \
                  path and makes the command fail (exit 1) after the rest. Errors: not_found, \
                  is_a_directory (use -r), already_exists, access_denied, invalid_name (a name of \
                  the temporary form), local_io, other. Not run again after a lost connection.",
        flags: &[
            ("-r", "copy a directory and everything below it"),
            ("-f", "replace an existing local file"),
        ],
        args: &[Arg::RemotePath, Arg::LocalPath],
        required: 1,
        examples: &[
            (
                "get /sizes/65537.bin",
                "fetch one file into the local working directory",
            ),
            ("get /sizes/1.bin copy.bin", "fetch it under another name"),
            (
                "get -f /sizes/1.bin notes.txt",
                "replace the local file notes.txt",
            ),
            (
                "get -r /tree/d0/d0",
                "fetch the directory d0 and everything below it into ./d0",
            ),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Put,
        name: "put",
        syntax: "put [-r] [-f] <local> [<remote>]",
        summary: "upload a file, or a directory with -r",
        details: "Uploads a local file into the remote working directory under the name given \
                  (a symbolic link keeps its own name and uploads its target's content), into \
                  <remote> if it ends in / or \\, else to the path <remote> (a trailing / is the only way to say \
                  'into this directory': put a.txt docs onto a directory docs is is_a_directory). \
                  The data goes to a temporary file .rsmb-<hash>.part in the target's directory and \
                  is renamed to the target when the server has confirmed every byte and the flush: \
                  a failed put leaves nothing under the target name, and its temporary file is \
                  deleted (on one new connection if need be) or named in the error. An existing \
                  remote file is not replaced without -f (the interactive shell asks). Cost on \
                  Samba: a file of at most 256 KiB 7 requests in 3 round trips (13 in 6 when \
                  the target exists and -f is not given: the refusal; with -f 7 in 3); larger: a check of the target (2), CREATE and 3 QUERY_INFO, one \
                  WRITE per 512 KiB, FLUSH, CLOSE, the rename (3). With -r the local directory and \
                  everything below it go to <remote>/<name>/... (default: the working directory): \
                  directories created level by level (2 requests each), small files 16 at a \
                  time; symbolic links, special files, names SMB does not allow and names that \
                  differ only in letter case are not uploaded (one error each). Errors: \
                  already_exists, is_a_directory, access_denied, disk_full, invalid_name, \
                  local_io, other. Drops the cached listing of the target's directory. Not run \
                  again after a lost connection.",
        flags: &[
            ("-r", "copy a directory and everything below it"),
            ("-f", "replace an existing remote file"),
        ],
        args: &[Arg::LocalPath, Arg::RemotePath],
        required: 1,
        examples: &[
            ("put notes.txt", "upload into the remote working directory"),
            ("put notes.txt renamed.txt", "upload under another name"),
            (
                "put notes.txt olddir/",
                "upload into the directory olddir (the trailing / says directory)",
            ),
            (
                "put -f notes.txt old.txt",
                "replace the remote file old.txt",
            ),
            (
                "put -r photos",
                "upload the directory photos and everything below it",
            ),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Cat,
        name: "cat",
        syntax: "cat <file>",
        summary: "print a remote file",
        details: "Writes the file's bytes to stdout as they arrive, binary data unchanged, \
                  never the whole file in memory (7 requests on Samba up to 512 KiB). With --json \
                  one 'content' object with the file as text, for a file of at most 1 MiB of \
                  UTF-8; anything else is an error of kind other that says to use get. Errors: \
                  not_found, is_a_directory, access_denied. Not run again after a lost connection \
                  (it has printed while it read).",
        flags: &[],
        args: &[Arg::RemotePath],
        required: 1,
        examples: &[("cat /tree/d0/file.txt", "print a remote file")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Mkdir,
        name: "mkdir",
        syntax: "mkdir [-p] <dir>",
        summary: "create a remote directory",
        details: "Creates one directory (2 requests, 1 round trip); its parent must exist. With -p \
                  every directory of the path below the working directory (or below the part \
                  the path shares with it) is opened or created, from the top, 2 requests a \
                  component, and one that exists is no error. Errors: already_exists (a directory \
                  or a file has the name), not_found (a parent is missing; -p creates it), \
                  not_a_directory (with -p, a file has the name of a component), access_denied, \
                  invalid_name (a character SMB does not allow). Drops the cached listing of the \
                  parent. Not run again after a lost connection.",
        flags: &[("-p", "create missing parents; no error if it exists")],
        args: &[Arg::RemoteDir],
        required: 1,
        examples: &[
            ("mkdir newdir", "create a directory"),
            ("mkdir -p a/b/c", "create a path of directories"),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Rm,
        name: "rm",
        syntax: "rm <file>",
        summary: "delete one remote file",
        details: "Deletes one file (3 requests, 1 round trip). No pattern is expanded, nothing is \
                  recursive, nothing asks. Errors: not_found, is_a_directory (use rmdir), \
                  access_denied, other (in use by another client, or the server will not delete \
                  it). Drops the cached listing of its directory. Not run again after a lost \
                  connection.",
        flags: &[],
        args: &[Arg::RemotePath],
        required: 1,
        examples: &[("rm old.txt", "delete one file")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Rmdir,
        name: "rmdir",
        syntax: "rmdir <dir>",
        summary: "remove an empty remote directory",
        details: "Removes one empty directory (3 requests, 1 round trip). Errors: not_found, \
                  not_a_directory, not_empty, access_denied; the working directory or one above \
                  it is a usage error (cd out of it first). Drops the cached listings of the \
                  parent and of everything at and below the directory. Not run again after a lost \
                  connection.",
        flags: &[],
        args: &[Arg::RemoteDir],
        required: 1,
        examples: &[("rmdir olddir", "remove an empty directory")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Lcd,
        name: "lcd",
        syntax: "lcd [<dir>]",
        summary: "change the local working directory (default: $HOME)",
        details: "Changes the local working directory, which get, put and lls use for relative \
                  paths. A leading ~ is $HOME. Sends nothing and needs no share. Errors: local_io \
                  with the system's text; usage when $HOME is not set.",
        flags: &[],
        args: &[Arg::LocalDir],
        required: 0,
        examples: &[
            ("lcd photos", "change the local working directory"),
            ("lcd", "go to the home directory"),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Lpwd,
        name: "lpwd",
        syntax: "lpwd",
        summary: "print the local working directory",
        details: "Prints the local working directory. Sends nothing and needs no share. With \
                  --json one 'lpwd' object.",
        flags: &[],
        args: &[],
        required: 0,
        examples: &[("lpwd", "print the local working directory")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Lls,
        name: "lls",
        syntax: "lls [<dir>]",
        summary: "list a local directory (default: the local working directory)",
        details: "Lists a local directory in the line format of ls, sorted by name, names that \
                  start with a dot included: 'd' or '-', the size, the modification time in UTC, \
                  the name with '/' after a directory. A symbolic link shows what it points to (a \
                  dangling one as a file of size 0). Sends nothing and needs no share. With \
                  --json one 'local_entry' object each. Errors: local_io.",
        flags: &[],
        args: &[Arg::LocalDir],
        required: 0,
        examples: &[
            ("lls", "list the local working directory"),
            ("lls photos", "list a local directory"),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Refresh,
        name: "refresh",
        syntax: "refresh",
        summary: "forget cached listings and the share list",
        details: "Empties the listing cache and forgets the share list, so the next completion \
                  asks the server again. Sends nothing. 'ls' always asks the server anyway.",
        flags: &[],
        args: &[],
        required: 0,
        examples: &[("refresh", "make the next Tab ask the server")],
        rerun: false,
    },
    Cmd {
        id: CmdId::Help,
        name: "help",
        syntax: "help [<command>|all]",
        summary: "show the commands, one command, or the whole manual",
        details: "'help' lists the commands, 'help <command>' shows one with its examples, \
                  'help all' prints the whole manual (the same as rsmb --help-all). Sends nothing. \
                  With --json the manual is printed as objects.",
        flags: &[],
        args: &[Arg::Command],
        required: 0,
        examples: &[
            ("help", "list the commands"),
            ("help cd", "show the command cd"),
            ("help all", "print the whole manual"),
        ],
        rerun: false,
    },
    Cmd {
        id: CmdId::Exit,
        name: "exit",
        syntax: "exit",
        summary: "end the session",
        details: "Ends the session. The exit code is that of the first failed command of a \
                  non-interactive run with --keep-going, 0 otherwise. Ctrl-D at the prompt does \
                  the same.",
        flags: &[],
        args: &[],
        required: 0,
        examples: &[("exit", "end the session")],
        rerun: false,
    },
];

pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// What follows the command name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Flags without a value, as given (`-r`, `-f`, `-p`).
    pub flags: Vec<String>,
    /// Flags with a value: (`-name`, `*.txt`).
    pub opts: Vec<(String, String)>,
    pub pos: Vec<String>,
    /// The last token is a flag that still wants its value.
    pub open: Option<String>,
}

/// A flag's name, and whether it takes a value (its declaration has a space: `-L <n>`).
fn flag_name(decl: &str) -> (&str, bool) {
    match decl.split_once(' ') {
        Some((name, _)) => (name, true),
        None => (decl, false),
    }
}

/// Splits the tokens after the command name: a token is a flag only if the command declares it
/// and it comes before `--`; the first `--` is dropped. After a flag that takes a value the next
/// token is that value, whatever it is.
pub fn split_args(cmd: &Cmd, tokens: &[String]) -> Args {
    let mut a = Args::default();
    let mut ended = false;
    let mut it = tokens.iter();
    while let Some(t) = it.next() {
        if ended {
            a.pos.push(t.clone());
        } else if t == "--" {
            ended = true;
        } else {
            match cmd
                .flags
                .iter()
                .map(|(f, _)| flag_name(f))
                .find(|(n, _)| n == t)
            {
                Some((n, true)) => match it.next() {
                    Some(v) => a.opts.push((n.to_string(), v.clone())),
                    None => a.open = Some(n.to_string()),
                },
                Some((_, false)) => a.flags.push(t.clone()),
                None => a.pos.push(t.clone()),
            }
        }
    }
    a
}

/// The value of a flag that takes one.
pub fn opt<'a>(a: &'a Args, name: &str) -> Option<&'a str> {
    a.opts
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Index of the positional argument that follows `finished` tokens (for completion); None after
/// a flag that still wants its value.
pub fn positional_index(cmd: &Cmd, finished: &[String]) -> Option<usize> {
    let a = split_args(cmd, finished);
    if a.open.is_some() {
        return None;
    }
    Some(a.pos.len())
}

// ---------------------------------------------------------------------------------------------
// Output

/// stdout, as text or as one JSON object per line. Remembers the first write error.
pub struct Out {
    pub json: bool,
    /// The interactive shell runs the lines (an overwrite may be asked about).
    pub interactive: bool,
    w: Box<dyn Write>,
    failed: Option<std::io::Error>,
    /// The command line being run, for errors about one item of a recursive transfer.
    line: String,
    /// Something of this command has gone to stdout (since `begin`).
    wrote: bool,
}

impl Out {
    pub fn new(json: bool) -> Out {
        Out::to(json, Box::new(BufWriter::new(std::io::stdout())))
    }

    pub fn to(json: bool, w: Box<dyn Write>) -> Out {
        Out {
            json,
            interactive: false,
            w,
            failed: None,
            line: String::new(),
            wrote: false,
        }
    }

    fn write(&mut self, line: &str) {
        self.wrote = true;
        if self.failed.is_none()
            && let Err(e) = writeln!(self.w, "{line}")
        {
            self.failed = Some(e);
        }
    }

    /// One line of text mode that holds names or messages from the server or the disk: control
    /// and bidi characters are shown escaped (`safe`).
    pub fn text(&mut self, line: &str) {
        if !self.json {
            self.write(&safe(line));
        }
    }

    /// Text mode, our own fixed text, possibly several lines (the manual): written as it is.
    pub fn doc(&mut self, text: &str) {
        if !self.json {
            self.write(text);
        }
    }

    pub fn obj(&mut self, v: &Value) {
        if self.json {
            self.write(&v.to_string());
        }
    }

    /// Bytes as they are, no newline (`cat`). A failed write is `local_io`.
    pub fn raw(&mut self, b: &[u8]) -> Result<()> {
        self.wrote = true;
        if self.failed.is_none()
            && let Err(e) = self.w.write_all(b)
        {
            self.failed = Some(e);
        }
        match &self.failed {
            None => Ok(()),
            Some(e) => Err(Error::new(Kind::LocalIo, format!("stdout: {e}"))),
        }
    }

    /// A write to stdout has failed (the reader is gone).
    pub fn broken(&self) -> bool {
        self.failed.is_some()
    }

    pub fn flush(&mut self) -> bool {
        if self.failed.is_none()
            && let Err(e) = self.w.flush()
        {
            self.failed = Some(e);
        }
        self.failed.is_none()
    }

    /// The line `run_line` is about to run.
    pub fn begin(&mut self, line: &str) {
        self.line = line.to_string();
        self.wrote = false;
    }

    /// Something of the current command has gone to stdout.
    pub fn written(&self) -> bool {
        self.wrote
    }

    /// A note about the running command: in text mode on stderr after `rsmb: ` (stdout flushed
    /// first); nothing with `--json`, where an object carries it.
    pub fn diag(&mut self, line: &str) {
        if !self.json {
            self.flush();
            let _ = writeln!(std::io::stderr(), "rsmb: {}", safe(line));
        }
    }

    pub fn error(&mut self, e: &Error, command: Option<&str>) {
        if self.json {
            let mut v = json!({"type": "error", "kind": e.kind.name(), "message": e.message,
                               "command": command});
            if let Some(p) = &e.path {
                v["path"] = json!(p);
            }
            if !e.left.is_empty() {
                v["left_behind"] = json!(e.left);
            }
            self.obj(&v);
            self.flush();
        } else {
            self.flush();
            let _ = writeln!(std::io::stderr(), "rsmb: {}", safe(&e.message));
        }
    }

    /// An error about one item inside a running command (a recursive transfer goes on).
    pub fn item_error(&mut self, e: &Error) {
        let line = self.line.clone();
        self.error(e, Some(&line));
    }

    /// Last line of a command (JSON only); flushes. False when stdout could not be written.
    pub fn done(&mut self, command: &str, ok: bool) -> bool {
        self.obj(&json!({"type": "done", "command": command, "ok": ok}));
        self.flush()
    }
}

/// An error outside any command (start-up), and a broken stdout, on stderr.
pub fn report(out: &mut Out, e: &Error) {
    out.error(e, None);
    out.flush();
}

// ---------------------------------------------------------------------------------------------
// Running a line

/// Runs one line: skip blank and `#` lines, tokenise, look up, check arguments, dispatch with
/// the retry after a reconnect, print the error and `done`. With `force` the command runs as if
/// `-f` had been given (the answer to the overwrite prompt). In the interactive shell a single
/// `get` or `put` refused with `already_exists` prints nothing and returns `Flow::Confirm`.
pub async fn run_line(sess: &mut Session, out: &mut Out, line: &str, force: bool) -> Flow {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Flow::Ok;
    }
    out.begin(line);
    let r = match parse(line) {
        Ok((cmd, mut a)) => {
            if force && cmd.flags.iter().any(|(f, _)| *f == "-f") && !has(&a.flags, "-f") {
                a.flags.push("-f".to_string());
            }
            let r = run_parsed(sess, out, cmd, &a).await;
            if let Err(e) = &r
                && out.interactive
                && !force
                && e.kind == Kind::AlreadyExists
                && e.path.is_none()
                && matches!(cmd.id, CmdId::Get | CmdId::Put)
                && !has(&a.flags, "-r")
                && !has(&a.flags, "-f")
            {
                return Flow::Confirm(e.message.clone());
            }
            r
        }
        Err(e) => Err(e),
    };
    let (flow, ok) = match &r {
        Ok(true) => (Flow::Exit, true),
        Ok(false) => (Flow::Ok, true),
        Err(e) => {
            out.error(e, Some(line));
            (Flow::Failed(e.exit_code(false)), false)
        }
    };
    if !out.done(line, ok) {
        let e = Error::new(Kind::LocalIo, "cannot write to stdout");
        let _ = writeln!(std::io::stderr(), "rsmb: {}", e.message);
        return Flow::Failed(1);
    }
    flow
}

pub fn has(flags: &[String], f: &str) -> bool {
    flags.iter().any(|x| x == f)
}

/// The command and what follows it, or a usage error.
fn parse(line: &str) -> Result<(&'static Cmd, Args)> {
    let tokens = tokenize(line)?;
    let name = &tokens[0];
    let cmd =
        find(name).ok_or_else(|| Error::usage(format!("unknown command '{name}'; see 'help'")))?;
    Ok((cmd, check_args(cmd, &tokens[1..])?))
}

/// `split_args`, then the usage errors: a flag without its value, a flag with a value given
/// twice, too few or too many positionals.
fn check_args(cmd: &Cmd, tokens: &[String]) -> Result<Args> {
    let a = split_args(cmd, tokens);
    if let Some(f) = &a.open {
        return Err(Error::usage(format!(
            "{f} needs a value; usage: {}",
            cmd.syntax
        )));
    }
    for (i, (n, _)) in a.opts.iter().enumerate() {
        if a.opts[..i].iter().any(|(m, _)| m == n) {
            return Err(Error::usage(format!("{n} given twice")));
        }
    }
    if a.pos.len() < cmd.required || a.pos.len() > cmd.args.len() {
        return Err(Error::usage(format!("usage: {}", cmd.syntax)));
    }
    Ok(a)
}

async fn run_parsed(sess: &mut Session, out: &mut Out, cmd: &Cmd, a: &Args) -> Result<bool> {
    // A command that starts on a dead connection reconnects first; if that fails (or the new
    // connection fails again) it is not tried a second time.
    let was_dead = sess.is_dead();
    let generation = sess.generation();
    let mut r = dispatch(cmd, sess, out, a).await;
    if let Err(e) = &mut r
        && e.is_lost()
    {
        // Not the connection a failed upload's cleanup has just made.
        if !sess.replaced_by_cleanup_since(generation) {
            sess.mark_dead();
        }
        // find has printed nothing yet: nothing can be printed twice (docs/commands.md).
        // Nothing that has printed is run again (find, and shares with the access probe lost).
        let rerun = (cmd.rerun || cmd.id == CmdId::Find) && !out.written();
        if !was_dead && e.kind == (Kind::ConnectionLost { silent: false }) && rerun {
            r = dispatch(cmd, sess, out, a).await;
            if let Err(e) = &r
                && e.is_lost()
            {
                sess.mark_dead();
            }
        } else if let Some(tail) = not_run_again(cmd, out.written(), &out.line) {
            e.message.push_str(&tail);
        }
    }
    r
}

/// What an error says about a command that is not run again after a lost connection. A `find`
/// that printed nothing has nothing incomplete, and changed nothing.
fn not_run_again(cmd: &Cmd, printed: bool, line: &str) -> Option<String> {
    if cmd.id == CmdId::Find {
        printed.then(|| format!("; '{line}' was not run again: the results above are incomplete"))
    } else if !cmd.rerun {
        Some(format!(
            "; '{line}' was not run again and may or may not have taken effect"
        ))
    } else {
        None
    }
}

async fn dispatch(cmd: &Cmd, sess: &mut Session, out: &mut Out, a: &Args) -> Result<bool> {
    let (flags, args) = (&a.flags, &a.pos);
    let arg = args.first().map(String::as_str);
    match cmd.id {
        CmdId::Shares => shares(sess, out).await?,
        CmdId::Use => sess.use_share(arg.unwrap_or_default()).await?,
        CmdId::Cd => sess.cd(arg.unwrap_or("/")).await?,
        CmdId::Pwd => pwd(sess, out),
        CmdId::Ls => ls(sess, out, arg).await?,
        CmdId::Tree => tree_cmd(sess, out, a).await?,
        CmdId::Find => find_cmd(sess, out, a).await?,
        CmdId::Get => transfer::get(sess, out, flags, args).await?,
        CmdId::Put => transfer::put(sess, out, flags, args).await?,
        CmdId::Cat => transfer::cat(sess, out, arg.unwrap_or_default()).await?,
        CmdId::Mkdir => mkdir(sess, has(flags, "-p"), arg.unwrap_or_default()).await?,
        CmdId::Rm => rm(sess, arg.unwrap_or_default()).await?,
        CmdId::Rmdir => rmdir(sess, arg.unwrap_or_default()).await?,
        CmdId::Lcd => lcd(arg)?,
        CmdId::Lpwd => lpwd(out)?,
        CmdId::Lls => lls(out, arg)?,
        CmdId::Refresh => sess.refresh(),
        CmdId::Help => help(out, arg)?,
        CmdId::Exit => return Ok(true),
    }
    Ok(false)
}

/// A remote path below the share root (the root itself is a usage error for `what`).
fn below_root(sess: &Session, arg: &str, what: &str) -> Result<Vec<String>> {
    sess.share()
        .ok_or_else(|| Error::usage("no share selected: use <share> first"))?;
    let p = sess.resolve(arg);
    if p.is_empty() {
        return Err(Error::usage(format!("{what}: not the share root")));
    }
    Ok(p)
}

async fn mkdir(sess: &mut Session, parents: bool, arg: &str) -> Result<()> {
    let path = below_root(sess, arg, "mkdir")?;
    // With -p: from the first component below what the path shares with the working directory
    // (`cd` checked those), so `mkdir -p a/b/c` costs 3 round trips wherever it is typed.
    let known = path
        .iter()
        .zip(sess.cwd())
        .take_while(|(a, b)| a == b)
        .count()
        .min(path.len() - 1);
    let first = if parents { known + 1 } else { path.len() };
    for i in first..=path.len() {
        sess.invalidate(&path[..i - 1], false);
    }
    let remote = sess.remote().await?;
    let r = async {
        if parents {
            for i in first..=path.len() {
                remote.mkdir(&path[..i], true).await?;
            }
            return Ok(());
        }
        remote.mkdir(&path, false).await.map_err(|e| {
            if e.kind == Kind::NotFound {
                Error::new(
                    Kind::NotFound,
                    format!(
                        "{}: a parent directory does not exist; mkdir -p creates it",
                        display_path(&path)
                    ),
                )
            } else {
                e
            }
        })
    }
    .await;
    sess.finish(remote, r).await
}

async fn rm(sess: &mut Session, arg: &str) -> Result<()> {
    let path = below_root(sess, arg, "rm")?;
    sess.invalidate(&path[..path.len() - 1], false);
    let remote = sess.remote().await?;
    let r = remote.remove(&path, false).await.map_err(|e| {
        if e.kind == Kind::IsADirectory {
            Error::new(
                Kind::IsADirectory,
                format!(
                    "{}: is a directory; rmdir removes an empty one",
                    display_path(&path)
                ),
            )
        } else {
            e
        }
    });
    sess.finish(remote, r).await
}

async fn rmdir(sess: &mut Session, arg: &str) -> Result<()> {
    let path = below_root(sess, arg, "rmdir")?;
    // The server folds case, so does the cache key; so does this guard.
    let cwd = sess.cwd();
    if path.len() <= cwd.len()
        && path
            .iter()
            .zip(cwd)
            .all(|(a, b)| a.to_lowercase() == b.to_lowercase())
    {
        return Err(Error::usage(format!(
            "rmdir {}: it is the working directory or one above it; cd out of it first",
            display_path(&path)
        )));
    }
    sess.invalidate(&path[..path.len() - 1], false);
    sess.invalidate(&path, true);
    let remote = sess.remote().await?;
    let r = remote.remove(&path, true).await;
    sess.finish(remote, r).await
}

fn local_err(what: &Path, e: std::io::Error) -> Error {
    Error::new(Kind::LocalIo, format!("{}: {e}", what.display()))
}

fn lcd(arg: Option<&str>) -> Result<()> {
    let dir = match arg {
        Some(a) => transfer::expand(a),
        None => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(std::path::PathBuf::from)
            .ok_or_else(|| Error::usage("lcd: $HOME is not set"))?,
    };
    std::env::set_current_dir(&dir).map_err(|e| local_err(&dir, e))
}

fn lpwd(out: &mut Out) -> Result<()> {
    let d = std::env::current_dir().map_err(|e| local_err(Path::new("."), e))?;
    let shown = d.to_string_lossy().into_owned();
    out.text(&shown);
    out.obj(&json!({"type": "lpwd", "path": shown}));
    Ok(())
}

/// SystemTime as FILETIME (100 ns since 1601), 0 when unknown.
pub fn to_filetime(t: std::io::Result<SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| {
            (d.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(d.subsec_nanos() / 100)
        })
}

fn lls(out: &mut Out, arg: Option<&str>) -> Result<()> {
    let dir = transfer::expand(arg.unwrap_or("."));
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| local_err(&dir, e))? {
        let e = e.map_err(|e| local_err(&dir, e))?;
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        // A symbolic link shows what it points to; a dangling one is a file of size 0.
        let (is_dir, size, mtime) = match std::fs::metadata(&path) {
            Ok(m) => (
                m.is_dir(),
                if m.is_dir() { 0 } else { m.len() },
                to_filetime(m.modified()),
            ),
            Err(_) => (false, 0, 0),
        };
        let abs = std::path::absolute(&path).unwrap_or(path);
        entries.push((name, abs, is_dir, size, mtime));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, abs, is_dir, size, mtime) in entries {
        if out.json {
            out.obj(
                &json!({"type": "local_entry", "name": name, "path": abs.to_string_lossy(),
                             "is_dir": is_dir, "size": size, "mtime": filetime(mtime, true)}),
            );
        } else {
            let e = Entry {
                name,
                size,
                mtime,
                attributes: if is_dir { 0x10 } else { 0x20 },
                file_id: 0,
            };
            out.text(&ls_line(&e));
        }
    }
    Ok(())
}

async fn shares(sess: &mut Session, out: &mut Out) -> Result<()> {
    let list = sess.shares().await?;
    let names: Vec<String> = list.iter().map(|s| s.name.clone()).collect();
    let (access, lost) = sess.access(&names).await?;
    for (s, a) in list.iter().zip(access) {
        if out.json {
            out.obj(
                &json!({"type": "share", "name": s.name, "share_type": s.type_name(),
                             "hidden": s.hidden(), "comment": s.comment, "access": a}),
            );
        } else {
            let col = match a {
                Some("rw") => "rw",
                Some("r") => "r-",
                Some("w") => "-w",
                Some(_) => "--",
                None => "?",
            };
            let line = format!("{:<20} {:<8} {col:<4} {}", s.name, s.type_name(), s.comment);
            out.text(line.trim_end());
        }
    }
    // The list is whole, the access column is not: the loss is the command's error (connection_lost,
    // exit 1), and the next command reconnects; nothing printed is printed again (run_parsed).
    match lost {
        Some(mut e) => {
            e.message
                .push_str("; the access of the shares shown as ? / null is unknown");
            Err(e)
        }
        None => Ok(()),
    }
}

fn pwd(sess: &Session, out: &mut Out) {
    if out.json {
        out.obj(
            &json!({"type": "pwd", "host": sess.host(), "share": sess.share(),
                         "path": display_path(sess.cwd())}),
        );
    } else {
        out.text(&sess.location());
    }
}

async fn ls(sess: &mut Session, out: &mut Out, arg: Option<&str>) -> Result<()> {
    let dir = sess.resolve(arg.unwrap_or("."));
    let entries = sess.list(&dir).await?;
    if out.json {
        for e in entries.iter() {
            out.obj(&entry_obj(&dir, e, None));
        }
    } else {
        for e in entries.iter() {
            out.text(&ls_line(e));
        }
    }
    Ok(())
}

/// The path of an entry of `dir`, from the share root.
pub fn entry_path(dir: &[String], e: &Entry) -> String {
    let sep = if dir.is_empty() { "" } else { "/" };
    format!("{}{sep}{}", display_path(dir), e.name)
}

/// The `entry` object of `ls`, `tree` and `find` (these two add `depth`).
pub fn entry_obj(dir: &[String], e: &Entry, depth: Option<usize>) -> Value {
    let mut v = json!({"type": "entry", "name": e.name, "path": entry_path(dir, e),
                       "is_dir": e.is_dir(), "size": e.size,
                       "mtime": filetime(e.mtime, true), "file_id": e.file_id.to_string()});
    if let Some(d) = depth {
        v["depth"] = json!(d);
    }
    v
}

/// `d` or `-`, the size in 13 columns, the time in UTC, the name (`/` after a directory).
pub fn ls_line(e: &Entry) -> String {
    let (mark, slash) = if e.is_dir() { ('d', "/") } else { ('-', "") };
    let time = filetime(e.mtime, false).unwrap_or_else(|| "-".to_string());
    format!("{mark}{:>13}  {time:<19}  {}{slash}", e.size, e.name)
}

/// A FILETIME as UTC text: `YYYY-MM-DD HH:MM:SS`, or RFC 3339 with `rfc3339`; None for 0.
pub fn filetime(ft: u64, rfc3339: bool) -> Option<String> {
    if ft == 0 {
        return None;
    }
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (H. Hinnant), days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (hh, mm, ss) = (rem / 3600, rem % 3600 / 60, rem % 60);
    Some(if rfc3339 {
        format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
    })
}

// ---------------------------------------------------------------------------------------------
// tree and find

/// What `each` gets for every listed directory: the sink, the progress line (clear it before
/// printing), the directory, its depth, its entries (sorted, without `.` and `..`). It says
/// whether the walk goes on.
type Each<'a> =
    dyn FnMut(&mut Out, &mut transfer::Progress, &[String], usize, &Arc<Vec<Entry>>) -> bool + 'a;

/// `remote()`, the walk with the progress line and the cache feed, `finish()`. Returns the
/// walk's report, the entries the listings held, and the time.
async fn run_walk(
    sess: &mut Session,
    out: &mut Out,
    label: &str,
    start: &[String],
    max_depth: Option<usize>,
    each: &mut Each<'_>,
) -> Result<(walk::Report, usize, Duration)> {
    let t0 = Instant::now();
    let remote = sess.remote().await?;
    let mut prog = transfer::Progress::new(out.json, label);
    let mut entries = 0;
    let mut dirs = 0;
    // The folded cache keys this walk has stored or given up (Cache::feed).
    let mut fed = HashMap::new();
    let r = {
        let r = remote.clone();
        let list = move |d: Vec<String>| {
            let r = r.clone();
            async move { r.list(&d).await }
        };
        let mut on_dir = |d: walk::Dir| {
            let list = Arc::new(d.entries);
            entries += list.len();
            dirs += 1;
            prog.listing(dirs);
            sess.feed(&mut fed, &d.path, list.clone());
            each(out, &mut prog, &d.path, d.depth, &list)
        };
        walk::walk(list, start, max_depth, &|_, _| true, &mut on_dir).await
    };
    prog.clear();
    let report = sess.finish(remote, r).await?;
    Ok((report, entries, t0.elapsed()))
}

/// The end of a walk that ran to its end (docs/commands.md): the denied and skipped directories,
/// the other failures, the summary; an error when the walk is incomplete.
#[allow(clippy::too_many_arguments)]
fn end_of_walk(
    out: &mut Out,
    command: &str,
    start: &[String],
    report: &walk::Report,
    end: (Vec<walk::EndLine>, Vec<Error>, bool),
    entries: usize,
    matched: usize,
    took: Duration,
) -> Result<()> {
    let (lines, failed, incomplete) = end;
    let path = display_path(start);
    let mut skipped = 0;
    for l in &lines {
        out.diag(&l.text);
        if l.kind == "denied" {
            out.obj(&json!({"type": "denied", "path": l.path}));
        } else {
            skipped += 1;
            out.obj(
                &json!({"type": "skipped", "path": l.path, "reason": l.reason,
                            "same_as": l.same_as}),
            );
        }
    }
    for e in &failed {
        out.item_error(e);
    }
    out.obj(
        &json!({"type": "walk_summary", "command": command, "path": path,
                    "directories": report.listed, "entries": entries, "matched": matched,
                    "denied": report.denied.len(), "skipped": skipped, "failed": failed.len(),
                    "seconds": transfer::seconds(took)}),
    );
    out.flush();
    if incomplete {
        let n = failed.len() + report.capped.len();
        return Err(Error::other(format!(
            "{command} {path}: incomplete: {n} directories could not be listed"
        )));
    }
    Ok(())
}

/// `find [<dir>] [-name <glob>] [-iname <glob>] [-type f|d] [-size [+|-]<n>[BKMGTP]] [-maxdepth <n>]`
async fn find_cmd(sess: &mut Session, out: &mut Out, a: &Args) -> Result<()> {
    sess.share()
        .ok_or_else(|| Error::usage("no share selected: use <share> first"))?;
    let (filter, depth) = walk::Filter::parse(&a.opts)?;
    let start = sess.resolve(a.pos.first().map_or(".", String::as_str));
    let label = format!("find {}", display_path(&start));
    let mut matched = 0;
    let (report, entries, took) = run_walk(
        sess,
        out,
        &label,
        &start,
        depth,
        &mut |out, prog, dir, depth, list| {
            let hits: Vec<&Entry> = list.iter().filter(|e| filter.matches(e)).collect();
            if !hits.is_empty() {
                prog.clear();
                for e in hits {
                    matched += 1;
                    let slash = if e.is_dir() { "/" } else { "" };
                    out.text(&format!("{}{slash}", entry_path(dir, e)));
                    out.obj(&entry_obj(dir, e, Some(depth + 1)));
                }
                out.flush();
            }
            // A closed stdout (find ... | head) stops the walk.
            !out.broken()
        },
    )
    .await?;
    if out.broken() {
        return Ok(());
    }
    let end = walk::end_report(&report);
    end_of_walk(out, "find", &start, &report, end, entries, matched, took)
}

/// `tree [-L <n>] [<dir>]`
async fn tree_cmd(sess: &mut Session, out: &mut Out, a: &Args) -> Result<()> {
    sess.share()
        .ok_or_else(|| Error::usage("no share selected: use <share> first"))?;
    let depth = opt(a, "-L")
        .map(|v| walk::parse_depth("-L", v))
        .transpose()?;
    let start = sess.resolve(a.pos.first().map_or(".", String::as_str));
    let path = display_path(&start);
    let mut nodes = TreeNodes::new(walk::TREE_MAX);
    let (report, entries, took) = run_walk(
        sess,
        out,
        &format!("tree {path}"),
        &start,
        depth,
        &mut |_, _, dir, _, list| nodes.add(dir, list),
    )
    .await?;
    draw_tree_output(out, &start, &report, nodes, entries, took)
}

/// The listings `tree` holds until it draws, up to `limit` entries.
struct TreeNodes {
    map: HashMap<Vec<String>, Arc<Vec<Entry>>>,
    count: usize,
    limit: usize,
}

impl TreeNodes {
    fn new(limit: usize) -> TreeNodes {
        TreeNodes {
            map: HashMap::new(),
            count: 0,
            limit,
        }
    }

    /// Whether the walk goes on: false once there are more entries than the limit.
    fn add(&mut self, dir: &[String], list: &Arc<Vec<Entry>>) -> bool {
        self.count += list.len();
        if self.count > self.limit {
            return false;
        }
        self.map.insert(dir.to_vec(), list.clone());
        true
    }
}

/// What `tree` prints after its walk: past the limit an error and nothing drawn, else the tree
/// and the end of the walk.
fn draw_tree_output(
    out: &mut Out,
    start: &[String],
    report: &walk::Report,
    nodes: TreeNodes,
    entries: usize,
    took: Duration,
) -> Result<()> {
    let path = display_path(start);
    if nodes.count > nodes.limit {
        return Err(Error::other(format!(
            "tree {path}: more than {} entries; limit the depth with -L, or use find",
            nodes.limit
        )));
    }
    let nodes = nodes.map;
    let end = walk::end_report(report);
    let mut notes: HashMap<Vec<String>, String> = HashMap::new();
    for p in &report.denied {
        notes.insert(p.clone(), "access denied".into());
    }
    for (p, first) in &report.links {
        notes.insert(
            p.clone(),
            match first {
                Some(f) => format!("already listed as {}", display_path(f)),
                None => walk::ABOVE.to_string(),
            },
        );
    }
    for p in &report.capped {
        notes.insert(p.clone(), "no file id".into());
    }
    for (p, e) in &report.failed {
        let why = if e.kind == Kind::NotFound {
            "gone"
        } else {
            e.kind.name()
        };
        notes.insert(p.clone(), why.replace('_', " "));
    }
    out.text(&path);
    let mut matched = 0;
    let (d, f) = walk::draw_tree(start, &nodes, &notes, &mut |depth, dir, e, text| {
        matched += 1;
        out.text(text);
        out.obj(&entry_obj(dir, e, Some(depth)));
    });
    out.text("");
    out.text(&format!("{d} directories, {f} files"));
    end_of_walk(out, "tree", start, report, end, entries, matched, took)
}

// ---------------------------------------------------------------------------------------------
// Help

/// Every object type rsmb prints with `--json`, with its fields.
pub const JSON_OBJECTS: &[(&str, &str)] = &[
    (
        "share",
        "name string, share_type \"disk\"|\"printer\"|\"device\"|\"ipc\"|\"unknown\", hidden bool \
         (name ends in $ or the type has bit 31), comment string, access \"rw\"|\"r\"|\"w\"|\"none\" \
         (the share-level access the server reports for this login) or null (no answer)",
    ),
    (
        "entry",
        "name string, path string (from the share root, starting with /), is_dir bool, size \
         integer, mtime string (RFC 3339, UTC) or null, file_id string of decimal digits; from \
         tree and find also depth integer (1 = directly in the start directory)",
    ),
    (
        "denied",
        "path string: a directory of a walk whose listing the server refused; what is in it is \
         unknown (tree, find)",
    ),
    (
        "skipped",
        "path string, reason \"already_listed\"|\"outside\"|\"gone\"|\"no_file_id\", same_as string \
         or null (for already_listed: the path under which the directory with the same file id \
         was listed; outside: it leads to the start's parent directory, which the walk does not \
         list): a directory a walk did not enter (tree, find)",
    ),
    (
        "walk_summary",
        "command \"tree\"|\"find\", path string (the start), directories integer (listings \
         that succeeded, the start included), entries integer (in those listings), matched \
         integer (entry objects printed), denied, skipped, failed integers, seconds number; once, \
         only after a walk that ran to its end",
    ),
    (
        "pwd",
        "host string, share string or null, path string (/ or /a/b)",
    ),
    (
        "error",
        "kind string (one of the error kinds), message string, command string or null (start-up); \
         path string (only for an error about one item of a recursive transfer: the remote path \
         for get, the local path for put); left_behind array of strings (only when temporary \
         files could not be removed: remote ones as /dir/.rsmb-....part on the selected share, \
         local ones as absolute paths)",
    ),
    (
        "transfer",
        "direction \"get\"|\"put\", remote string (from the share root, starting with /), local \
         string (absolute path), bytes integer, seconds number; one per file that arrived",
    ),
    (
        "summary",
        "direction \"get\"|\"put\", files integer (arrived), bytes integer, failed integer (not \
         transferred: files, directories and other entries), directories integer, seconds \
         number; once at the end of a recursive transfer, also after a fatal error, not after an \
         interrupt",
    ),
    (
        "content",
        "path string, size integer, text string; cat with --json",
    ),
    (
        "local_entry",
        "name string, path string (absolute), is_dir bool, size integer, mtime string (RFC \
         3339, UTC) or null; lls",
    ),
    ("lpwd", "path string (the local working directory)"),
    (
        "done",
        "command string (the line as given, trimmed), ok bool; the last line of every command",
    ),
    (
        "command",
        "name, syntax, summary, details strings; flags array of {flag, help}; examples array of \
         {line, does}",
    ),
    ("option", "flag string (as -H, --host <HOST>), help string"),
    ("exit_code", "code integer, meaning string"),
    ("topic", "name string, text string"),
    (
        "json_object",
        "name string (a value of type), fields string",
    ),
];

pub const EXIT_CODES: &[(u8, &str)] = &[
    (
        0,
        "every command succeeded (or the shell ended with exit or Ctrl-D)",
    ),
    (
        1,
        "a command failed (the first failure stops a non-interactive run unless -k)",
    ),
    (
        2,
        "usage: a bad flag, a bad command line, an unknown command, no share selected, no \
         password without a terminal",
    ),
    (
        3,
        "connecting or logging in failed at start-up, a proxy failure included",
    ),
    (
        130,
        "interrupted: SIGINT or SIGTERM dropped a running command or the start-up connect; \
         SIGTERM also ends the interactive shell (Ctrl-C there only returns to the prompt)",
    ),
];

const ABOUT: &str = "rsmb is an SMB2/3 client shell built for high-latency links: one TCP \
connection, one tree connect per share, cd in one round trip, a directory listing in one round \
trip, cached listings for Tab completion. It is used interactively or driven by a program.";

fn topic_commands_given() -> String {
    "Commands come from -S <file> (one per line) first, then every -C <line> in order. Then, if \
stdin is a terminal and -N is absent, the interactive shell starts; if stdin is not a terminal \
and neither -C nor -S was given, commands are read from stdin, one per line, until end of input; \
otherwise rsmb exits. With -C or -S stdin is never read. Blank lines and lines whose first \
non-blank character is # are skipped. A non-interactive run stops at the first failing command \
with its exit code; with -k (--keep-going) it goes on and exits with the code of the first \
failure. If stdout cannot be written (the reader closed the pipe) the run ends at once with exit \
code 1, also with -k. rsmb never prompts when stdin is not a terminal: a missing password is a \
usage error (give -p or --no-pass). With --json the line editor never starts: lines typed at a \
terminal are read one by one like lines from a pipe, under the same rules (the first failing \
command ends the run unless -k; Ctrl-C during a command ends the run with 130 after the cleanup, \
Ctrl-C between commands exits 130; nothing asks to overwrite)."
        .to_string()
}

fn topic_quoting() -> String {
    "Tokens are separated by ASCII white space. '...' is taken literally. In \"...\" only \\\" and \
\\\\ are escapes. Outside quotes a backslash takes the next character literally, so a backslash \
as path separator must be quoted or doubled (cd 'tree\\d0', cd tree\\\\d0); / needs nothing. A \
non-ASCII space such as U+3000 is part of a name. No variables, no globbing. Paths: / or \\ \
separate; a leading one starts at the share root, otherwise the path is relative to the working \
directory; . and .. work. A token that starts with - is an argument, not a flag, unless the \
command declares that flag; -- ends the flags. In text output a control character (C0, DEL, C1) or \
bidi control (U+202A-202E, U+2066-2069, U+200E, U+200F, U+061C) in a name or message from the \
server is shown escaped as \\u{9b}; --json and the names sent back to the server and used for local \
files keep the exact name. Tab completes command names, share names after \
use, and remote paths (directories only after cd, mkdir, rmdir, tree and find; directories and \
files after ls, get, cat, rm and as put's second argument). As get's second argument, put's first \
and after lcd (directories only) and lls it completes local paths, with no request. Names that \
hold control or bidi characters are shown escaped as \\u{9b} in the candidate list."
        .to_string()
}

fn topic_transfers() -> String {
    "get and put write the data under a temporary name in the directory of the final name, \
.rsmb-<16 hex digits>.part (a hash of the final name), and rename it to the final name only when \
every byte has arrived and been checked: under a final name there is only ever a complete file or \
the file that was there before. A file goes as one compound request when it is at most 256 KiB \
(the small path; 64 KiB on a server with 64 KiB I/O) in a recursive transfer or a single put, and \
as a stream of 512 KiB requests otherwise. Nothing is overwritten without -f, and -f replaces only \
what was there when the transfer started (a target that appears meanwhile is not replaced; on \
the server a replacing rename is the server's decision). -f never replaces a directory. In the \
interactive shell a single get or put onto an existing target asks 'Overwrite? [y/N]'; nothing \
else ever asks. For put a trailing / says 'into this directory': put a.txt docs/ writes \
docs/a.txt, put a.txt docs onto a directory docs is is_a_directory. Names of the temporary form \
are never transferred (invalid_name): a leftover is a partial file; cat and rm work on it. After \
a failure the temporary file is removed: locally always, on the server on the same connection \
within 2 s or on one new connection; what could not be removed is named in the error and in \
left_behind. After kill -9 a temporary file stays; the next transfer of the same name removes \
it. After a silent link the server keeps a leftover open for the dead session until it notices \
the connection is gone: until then rm and a new put of that name fail with 'in use'. Nothing is \
resumed or retried: a failed transfer starts again from byte 0. get -r <dir> [<local dir>] and \
put -r <dir> [<remote dir>] copy the directory and everything below it into the second \
argument (default: the working directory), keeping the structure; a recursive transfer goes on \
past items it cannot transfer (one error each, with path), prints one summary, and exits 1 if \
anything was not transferred; after an interrupt it prints no summary. Times, attributes and \
permissions are not kept. Symbolic links are not created, and not followed below a local \
directory."
        .to_string()
}

fn topic_timeouts() -> String {
    "--timeout <seconds> (default 30) is the response deadline. A server that has gone silent \
fails the command after between --timeout and max(--timeout, 15 s), with kind connection_lost; \
the command is not tried again, and the next command that sends opens a new connection. One \
reply that keeps arriving must be complete within 6 x --timeout (180 s by default): on a very \
slow link raise --timeout. Connecting has its own 10 s limit, through a proxy too, which \
--timeout does not change. A connection that was closed is \
replaced by the next command that sends; a reading command that lost its connection midway runs \
once more on a new one. A reconnect that fails is reported once, with its own kind (connect, \
auth), and not retried. Commands that send nothing (pwd, help, refresh, exit, cd to the root or \
to a directory above) never touch the network. After Ctrl-C during a command that was on the \
wire, the next command that sends opens a new connection. Tab in a directory that is not cached \
gives up after 5 s (the editor beeps) and the connection is replaced; Ctrl-C cannot interrupt a \
Tab. There is no liveness probe: an idle session sends nothing. In a transfer the deadline is per \
request, not per transfer: each READ or WRITE of at most 512 KiB, each small file and each walk \
listing is one reply that must be complete within 6 x --timeout, with at most 4 MiB queued in \
front of it. The commands that change something or write output while they read (get, put, cat, \
mkdir, rm, rmdir) are not run again after a lost connection; the error says so. A failed put may \
make one new connection inside the command, only to delete its temporary files (not after a \
silent link). After an interrupted transfer, or one that failed locally once data was being read, the next \
command reconnects; a refusal before any data was read keeps the connection."
        .to_string()
}

fn topic_examples() -> String {
    "Text: rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM -C 'use share' -C 'ls /sizes'\n\
JSON: printf 'use share\\ncd tree\\nls\\n' | rsmb -H 10.213.45.10 -u tester -p testpass -d TESTDOM \
--json --keep-going\n\
Each command's output ends with a {\"type\":\"done\",...} line, so a program can write one line \
to stdin and read until done.\n\
The examples of get, put, mkdir, rm and rmdir are written for a remote working directory you may \
write to that holds a file old.txt and an empty directory olddir, and a local working directory \
that holds notes.txt and a directory photos; those of lcd and lls for the same local directory.\n\
Through a SOCKS5 proxy: rsmb --proxy socks5://127.0.0.1:1080 -H fileserver.example -u alice -d \
CORP -N -C 'use docs' -C 'ls'"
        .to_string()
}

fn topic_proxy() -> String {
    "--proxy socks5://[user:pass@]host:port sends every connection to the server through a SOCKS5 \
proxy (RFC 1928, with a user name and password by RFC 1929). socks5h:// means the same, and one \
trailing / is accepted; any other scheme (socks4, socks4a, http, https, upper case) is a usage \
error, exit 2, whose message never repeats the URL. Percent-encode @ : / % and other special \
characters in user and password (%40 %3A %2F %25); each is 1 to 255 bytes after decoding. An \
IPv6 proxy goes in brackets: socks5://[::1]:1080. The proxy resolves the server's name: -H goes \
to it as written, a name as a name and an IPv4 or IPv6 address (without brackets) as an \
address, so this host never looks the server's name up; with --proxy -H is at most 255 bytes. \
The proxy's own name is resolved on this host. Every connection, the first and every \
reconnect, goes through the proxy and nothing else is dialled: a proxy that cannot be reached \
is an error, never a reason to connect directly. No environment variable (ALL_PROXY and the \
like) is read. The credentials are visible in the process list, like -p, and never printed. A \
failure at the proxy is kind connect (exit 3 at start-up; a reconnect that fails is 'reconnect \
failed: ...', exit 1) with a message that starts 'proxy <host>:<port>: ', in three shapes: the \
proxy cannot be reached (Connection refused; no answer within 10 s); the proxy refuses the \
login (user name or password refused; wants a user name and password); the proxy cannot reach \
the server (cannot reach <server>:<port>: host unreachable, connection refused, or a general \
failure, which some proxies give for an unknown name). When the TCP connection to the proxy \
stood but the handshake did not finish within 10 s, the message says the proxy is silent, is \
not a SOCKS5 proxy, or cannot reach the server: rsmb cannot tell which. Connecting has 10 s in \
all, through the proxy too; --timeout does not change it. A refused SMB login through the proxy \
is kind auth, as direct. Cost: the TCP connect to the proxy and 2 round trips to it (3 with a \
login), once per connection; every command sends the same requests in the same round trips as \
direct. A proxy that holds back small segments (Nagle's algorithm on its socket to the server, \
as microsocks does) delays what has many requests in flight: walks, many small files, large \
transfers."
        .to_string()
}

fn topic_tree_and_find() -> String {
    format!(
        "tree and find walk a directory and everything below it, with at most 16 listings in \
flight on the one connection. Depth: the directory itself is depth 0 and is never printed; its \
entries are depth 1; tree -L n and find -maxdepth n print depths 1 to n (0 is a usage error). \
Paths are printed from the share root, whatever the argument and the working directory. Order: \
the same on every run; find prints breadth first (a directory before what is in it, each \
directory's entries by name), tree depth first by name. Patterns (-name, -iname) match the \
last component of a path, as Unicode characters: * any run, also none (a leading dot is not \
special); ? one character; [abc] [a-z] one of a set, [!a] or [^a] none of it, ] first in a \
class and - first or last are members, a backwards range [z-a] holds nothing; \\ makes the \
next character literal, also inside a class; a [ with no closing ] is itself; {{a,b}} is \
literal; classes like [:digit:] are refused (write [0-9]); a / in a pattern is refused. \
-iname takes two characters as equal when their one-character lower-case or upper-case forms \
are equal (so ß is not SS, and İ is not i); names are never normalised (an NFC pattern does \
not match an NFD name). The tokeniser does no globbing, so -name *.txt works unquoted; outside \
quotes it takes a backslash for itself: write '\\[abc\\].txt' or [[]abc[]].txt. -size \
[+|-]n[BKMGTP]: in bytes, strictly, no rounding: +n more than n units, -n less than n units, n \
exactly; B or c bytes, K M G T P powers of 1024 (upper or lower case); b is refused (512-byte \
blocks in Unix find); a directory never matches -size. At the end: every directory that \
denied listing ('denied'), was not entered because a directory with its file id was already \
listed, because it leads above the start or because it vanished ('skipped'), and every other failure (an 'error' with path), \
then one 'walk_summary'. Denied and skipped directories do not change the exit status; any \
other failure, or a walk past its limit on a server without file ids, makes the command fail \
(exit 1) after the rest. After an interrupt, a lost connection, the tree limit or a closed \
stdout no walk_summary follows: without it the result is not whole. tree collects at most \
{} entries before it draws. find prints as listings arrive and holds the queue of \
directories still to list, and every directory it entered or skipped, with its file id and \
path; what it lists also goes to the cache that Tab reads, up to the cache's own cap (two \
directories whose names differ only in case, and what is below them, are cached by neither); a \
walk never reads the cache.",
        walk::TREE_MAX
    )
}

fn topics() -> Vec<(&'static str, String)> {
    vec![
        ("about", ABOUT.to_string()),
        ("giving commands", topic_commands_given()),
        ("quoting and paths", topic_quoting()),
        ("transfers", topic_transfers()),
        ("timeouts and reconnect", topic_timeouts()),
        ("proxy", topic_proxy()),
        ("tree and find", topic_tree_and_find()),
        ("examples", topic_examples()),
    ]
}

fn command_text(c: &Cmd, out: &mut Out) {
    out.doc(&format!("{}  -  {}", c.syntax, c.summary));
    out.doc(&format!("    {}", c.details));
    for (f, h) in c.flags {
        out.doc(&format!("    {f}  {h}"));
    }
    for (line, does) in c.examples {
        out.doc(&format!("    example: {line}    # {does}"));
    }
}

fn command_obj(c: &Cmd) -> Value {
    json!({"type": "command", "name": c.name, "syntax": c.syntax, "summary": c.summary,
           "details": c.details,
           "flags": c.flags.iter().map(|(f, h)| json!({"flag": f, "help": h})).collect::<Vec<_>>(),
           "examples": c.examples.iter().map(|(l, d)| json!({"line": l, "does": d})).collect::<Vec<_>>()})
}

fn help(out: &mut Out, arg: Option<&str>) -> Result<()> {
    match arg {
        None => {
            for c in COMMANDS {
                out.doc(&format!("{:<10} {}", c.name, c.summary));
                out.obj(&command_obj(c));
            }
            out.doc("'help <command>' shows one command, 'help all' the whole manual.");
        }
        Some("all") => help_all(out),
        Some(name) => {
            let c = find(name)
                .ok_or_else(|| Error::usage(format!("unknown command '{name}'; see 'help'")))?;
            command_text(c, out);
            out.obj(&command_obj(c));
        }
    }
    Ok(())
}

/// The whole manual: the same for `help all` and `rsmb --help-all`.
pub fn help_all(out: &mut Out) {
    let topics = topics();
    let options = crate::options();
    // 1. what rsmb is
    out.doc("RSMB(1)\n");
    out.doc(&topics[0].1);
    out.obj(&json!({"type": "topic", "name": topics[0].0, "text": topics[0].1}));
    // 2. synopsis and start-up flags, from clap's metadata
    out.doc("\nSYNOPSIS");
    out.doc("    rsmb -H <host> [-P <port>] [-u <user>] [-d <domain>] [-p <password> | --no-pass]");
    out.doc("         [--proxy socks5://[user:pass@]host:port] [--timeout <seconds>]");
    out.doc("         [-S <file>] [-C <command>]... [-N] [-k] [--json]");
    out.doc("    rsmb --help-all [--json]");
    out.doc("\nSTART-UP FLAGS");
    for (flag, help) in &options {
        out.doc(&format!("    {flag:<32} {help}"));
        out.obj(&json!({"type": "option", "flag": flag, "help": help}));
    }
    // 3, 4
    for (name, text) in &topics[1..3] {
        out.doc(&format!("\n{}", name.to_uppercase()));
        out.doc(text);
        out.obj(&json!({"type": "topic", "name": name, "text": text}));
    }
    // 5. every command
    out.doc("\nCOMMANDS");
    for c in COMMANDS {
        out.doc("");
        command_text(c, out);
        out.obj(&command_obj(c));
    }
    // 6. --json
    out.doc("\nJSON OUTPUT (--json)");
    out.doc(
        "Every stdout line is one JSON object with a 'type'; nothing else goes to stdout and \
         stderr stays empty. A failed command prints 'error' then 'done' with ok false; a failed \
         start-up prints one 'error' and no 'done'; skipped lines print nothing. A flag error is one \
         'error' of kind usage (exit 2). --json never starts the line editor: with a terminal on \
         stdin lines are read from it one by one, under the rules of a non-interactive run (the \
         first failing command ends the run unless -k; Ctrl-C during a command ends it with 130).",
    );
    for (name, fields) in JSON_OBJECTS {
        out.doc(&format!("    {name:<12} {fields}"));
        out.obj(&json!({"type": "json_object", "name": name, "fields": fields}));
    }
    out.doc(&format!("    error kinds: {}", KIND_NAMES.join(", ")));
    // 7. exit codes
    out.doc("\nEXIT CODES");
    for (code, meaning) in EXIT_CODES {
        out.doc(&format!("    {code:<4} {meaning}"));
        out.obj(&json!({"type": "exit_code", "code": code, "meaning": meaning}));
    }
    // 8, 9
    for (name, text) in &topics[3..] {
        out.doc(&format!("\n{}", name.to_uppercase()));
        out.doc(text);
        out.obj(&json!({"type": "topic", "name": name, "text": text}));
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn filetime_cases() {
        assert_eq!(filetime(0, true), None);
        assert_eq!(
            filetime(116_444_736_000_000_000, true).unwrap(),
            "1970-01-01T00:00:00Z"
        );
        // 2024-02-29 12:00:00 UTC = unix 1709208000
        let ft = |unix: u64| (unix + 11_644_473_600) * 10_000_000;
        assert_eq!(
            filetime(ft(1_709_208_000), true).unwrap(),
            "2024-02-29T12:00:00Z"
        );
        assert_eq!(
            filetime(ft(1_709_208_000), false).unwrap(),
            "2024-02-29 12:00:00"
        );
        // 2026-11-17 21:33:05 UTC = unix 1794951185
        assert_eq!(
            filetime(ft(1_794_951_185), true).unwrap(),
            "2026-11-17T21:33:05Z"
        );
        assert_eq!(
            filetime(ft(1_794_951_185) + 9_999_999, false).unwrap(),
            "2026-11-17 21:33:05"
        );
        assert_eq!(filetime(1, true).unwrap(), "1601-01-01T00:00:00Z");
        assert_eq!(
            filetime(116_444_735_990_000_000, true).unwrap(),
            "1969-12-31T23:59:59Z"
        );
        assert!(filetime(u64::MAX, true).is_some());
    }

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn listed(names: &[&str]) -> Arc<Vec<Entry>> {
        Arc::new(
            names
                .iter()
                .map(|n| Entry {
                    name: n.to_string(),
                    size: 1,
                    mtime: 0,
                    attributes: 0x20,
                    file_id: 1,
                })
                .collect(),
        )
    }

    #[test]
    fn tree_past_its_limit_draws_nothing() {
        let start = vec!["t".to_string()];
        for json in [false, true] {
            let mut nodes = TreeNodes::new(5);
            assert!(nodes.add(&start, &listed(&["a", "b", "c"])));
            assert!(
                !nodes.add(&["t".into(), "d".into()], &listed(&["x", "y", "z"])),
                "6 entries: the walk is told to stop"
            );
            let sink = Sink::default();
            let mut out = Out::to(json, Box::new(sink.clone()));
            let e = draw_tree_output(
                &mut out,
                &start,
                &walk::Report::default(),
                nodes,
                6,
                Duration::ZERO,
            )
            .unwrap_err();
            out.flush();
            assert_eq!(e.kind, Kind::Other);
            assert!(
                e.message.contains("-L") && e.message.contains("find"),
                "{}",
                e.message
            );
            assert!(
                sink.0.lock().unwrap().is_empty(),
                "nothing drawn, no walk_summary: {:?}",
                String::from_utf8_lossy(&sink.0.lock().unwrap())
            );
        }
        // At the limit exactly, it draws.
        let mut nodes = TreeNodes::new(3);
        assert!(nodes.add(&start, &listed(&["a", "b", "c"])));
        let sink = Sink::default();
        let mut out = Out::to(false, Box::new(sink.clone()));
        draw_tree_output(
            &mut out,
            &start,
            &walk::Report::default(),
            nodes,
            3,
            Duration::ZERO,
        )
        .unwrap();
        out.flush();
        let text = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
        assert!(text.contains("0 directories, 3 files"), "{text}");
    }

    #[test]
    fn lost_connection_tails() {
        let cmd = |n: &str| find(n).unwrap();
        let line = "find /x";
        assert_eq!(not_run_again(cmd("find"), false, line), None);
        let t = not_run_again(cmd("find"), true, line).unwrap();
        assert!(t.contains("results above are incomplete"), "{t}");
        let t = not_run_again(cmd("put"), false, "put a b").unwrap();
        assert!(t.contains("may or may not have taken effect"), "{t}");
        assert_eq!(not_run_again(cmd("ls"), true, "ls"), None);
    }

    #[test]
    fn ls_line_format() {
        let e = |name: &str, size, attributes| Entry {
            name: name.into(),
            size,
            mtime: 116_444_736_000_000_000,
            attributes,
            file_id: 1,
        };
        assert_eq!(
            ls_line(&e("a.txt", 5, 0x20)),
            "-            5  1970-01-01 00:00:00  a.txt"
        );
        assert_eq!(
            ls_line(&e("dir", 0, 0x10)),
            "d            0  1970-01-01 00:00:00  dir/"
        );
        assert_eq!(
            ls_line(&e("over4g.bin", 4_617_089_843, 0x20)),
            "-   4617089843  1970-01-01 00:00:00  over4g.bin"
        );
        let mut x = e("t", 1, 0);
        x.mtime = 0;
        assert_eq!(ls_line(&x), "-            1  -                    t");
    }

    #[test]
    fn table_is_complete() {
        let mut names = std::collections::HashSet::new();
        for c in COMMANDS {
            assert!(names.insert(c.name), "duplicate {}", c.name);
            assert!(!c.summary.is_empty() && !c.details.is_empty(), "{}", c.name);
            assert!(
                c.syntax.split(' ').next() == Some(c.name),
                "{}: {}",
                c.name,
                c.syntax
            );
            assert!(!c.examples.is_empty(), "{} has no example", c.name);
            for (line, does) in c.examples {
                assert_eq!(tokenize(line).unwrap()[0], c.name, "{line}");
                assert!(!does.is_empty());
            }
            assert!(c.required <= c.args.len(), "{}", c.name);
        }
    }

    /// Captures what an `Out` writes.
    #[derive(Clone, Default)]
    struct Capture(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub fn rendered(json: bool) -> String {
        let cap = Capture::default();
        let mut out = Out::to(json, Box::new(cap.clone()));
        help_all(&mut out);
        assert!(out.flush());
        String::from_utf8(cap.0.borrow().clone()).unwrap()
    }

    #[test]
    fn help_all_has_everything() {
        let s = rendered(false);
        for c in COMMANDS {
            assert!(s.contains(c.syntax), "{}", c.syntax);
            for (l, _) in c.examples {
                assert!(s.contains(l), "{l}");
            }
        }
        for (name, text) in topics() {
            assert!(s.contains(&text), "topic {name}");
        }
        assert_eq!(topics().len(), 8, "parts 1, 3, 4, 8 to 12 are topics");
        assert!(topic_proxy().contains("never a reason to connect directly"));
        assert!(s.contains(
            "rsmb --proxy socks5://127.0.0.1:1080 -H fileserver.example -u alice -d CORP -N -C \
             'use docs' -C 'ls'"
        ));
        assert!(
            !s.contains("prompt goes to stdout"),
            "the old JSON paragraph"
        );
        assert!(s.contains("--json never starts the line editor"));
        assert!(s.contains("under the rules of a non-interactive run"));
        assert!(
            topic_timeouts().contains("Connecting has its own 10 s limit, through a proxy too")
        );
        assert!(topic_commands_given().contains("With --json the line editor never starts"));
        for part in [
            "SYNOPSIS",
            "START-UP FLAGS",
            "COMMANDS",
            "JSON OUTPUT",
            "EXIT CODES",
            "GIVING COMMANDS",
            "QUOTING AND PATHS",
            "TRANSFERS",
            "TIMEOUTS AND RECONNECT",
            "PROXY",
            "TREE AND FIND",
            "EXAMPLES",
        ] {
            assert!(s.contains(part), "{part}");
        }
        for code in ["0 ", "1 ", "2 ", "3 ", "130 "] {
            assert!(s.contains(&format!("    {code}")), "exit code {code}");
        }
        for t in [
            "share",
            "entry",
            "pwd",
            "error",
            "done",
            "command",
            "option",
            "exit_code",
            "topic",
            "json_object",
            "transfer",
            "summary",
            "content",
            "local_entry",
            "lpwd",
            "denied",
            "skipped",
            "walk_summary",
        ] {
            assert!(JSON_OBJECTS.iter().any(|(n, _)| *n == t), "{t}");
            assert!(s.contains(&format!("    {t} ")), "json type {t}");
        }
        for k in KIND_NAMES {
            assert!(s.contains(k), "{k}");
        }
        for part in [
            "-S",
            "-C",
            "-N",
            "-k",
            "stdin",
            "#",
            "--timeout",
            "15 s",
            "6 x --timeout",
            "Ctrl-C",
            "5 s",
            "--json",
            "--keep-going",
            ".rsmb-",
            "Overwrite? [y/N]",
            "left_behind",
            "only what was there when the transfer started",
            "never transferred",
            "in use",
            "Nothing is resumed",
            "no summary",
            "trailing /",
            "holds a file old.txt and an empty directory olddir",
            "not_empty",
            "per request",
            "-maxdepth n",
            "[:digit:] are refused",
            "b is refused",
            "never matches -size",
            "without it the result is not whole",
            "the same on every run",
            "500000 entries",
            "depth",
        ] {
            assert!(s.contains(part), "{part}");
        }
        // JSON: every line an object whose type is listed
        for line in rendered(true).lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            let t = v["type"].as_str().unwrap();
            assert!(JSON_OBJECTS.iter().any(|(n, _)| *n == t), "{line}");
        }
    }

    #[test]
    fn arguments_and_flags() {
        let cd = find("cd").unwrap();
        let toks = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(split_args(cd, &toks(&["-rf"])).pos, ["-rf"]);
        assert_eq!(
            split_args(find("ls").unwrap(), &toks(&["--help.txt"])).pos,
            ["--help.txt"]
        );
        assert_eq!(split_args(cd, &toks(&["--", "-rf"])).pos, ["-rf"]);
        assert_eq!(split_args(cd, &toks(&["--", "--"])).pos, ["--"]);
        let fake = Cmd {
            flags: &[("-r", "recursive")],
            ..*cd
        };
        let a = split_args(&fake, &toks(&["-r", "x", "--", "-r"]));
        assert_eq!((a.flags, a.pos), (toks(&["-r"]), toks(&["x", "-r"])));
        assert_eq!(parse("cd a b").unwrap_err().kind, Kind::Usage);
        assert_eq!(parse("use").unwrap_err().kind, Kind::Usage);
        assert_eq!(parse("nosuch").unwrap_err().kind, Kind::Usage);
        assert_eq!(parse("cd -- -rf").unwrap().1.pos, ["-rf"]);
        assert_eq!(parse("ls --help.txt").unwrap().1.pos, ["--help.txt"]);
    }

    #[test]
    fn flags_of_get_and_mkdir() {
        let p = |l: &str| {
            let (c, a) = parse(l).unwrap();
            (c.name, a.flags, a.pos)
        };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(p("mkdir -p x"), ("mkdir", s(&["-p"]), s(&["x"])));
        assert_eq!(p("mkdir -- -p"), ("mkdir", s(&[]), s(&["-p"])));
        assert_eq!(
            p("get -r -f a b"),
            ("get", s(&["-r", "-f"]), s(&["a", "b"]))
        );
        assert_eq!(p("get -- -r"), ("get", s(&[]), s(&["-r"])));
        assert_eq!(
            p("get -rf"),
            ("get", s(&[]), s(&["-rf"])),
            "-rf is not a flag"
        );
        assert_eq!(p("put -f x"), ("put", s(&["-f"]), s(&["x"])));
        assert_eq!(parse("mkdir").unwrap_err().kind, Kind::Usage);
        assert_eq!(parse("rm a b").unwrap_err().kind, Kind::Usage);
    }

    #[test]
    fn flags_with_values() {
        let (fd, tr) = (find("find").unwrap(), find("tree").unwrap());
        let toks = |l: &str| tokenize(l).unwrap();
        let ok = |c: &Cmd, l: &str| check_args(c, &toks(l)).unwrap();
        let usage = |c: &Cmd, l: &str| check_args(c, &toks(l)).unwrap_err();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let o = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        let a = ok(fd, "-name x /d");
        assert_eq!(a, ok(fd, "/d -name x"));
        assert_eq!(
            (a.opts.clone(), a.pos.clone()),
            (o(&[("-name", "x")]), s(&["/d"]))
        );
        assert_eq!(opt(&a, "-name"), Some("x"));
        assert_eq!(opt(&a, "-type"), None);
        let e = usage(fd, "-name");
        assert_eq!(e.kind, Kind::Usage);
        assert!(e.message.contains("-name needs a value"), "{}", e.message);
        assert_eq!(ok(fd, "-name -type").opts, o(&[("-name", "-type")]));
        let a = ok(fd, "-name --");
        assert_eq!((a.opts, a.pos), (o(&[("-name", "--")]), s(&[])));
        let a = ok(fd, "-- -name");
        assert_eq!((a.opts, a.pos), (o(&[]), s(&["-name"])));
        let e = usage(fd, "-name a -name b");
        assert!(e.message.contains("-name given twice"), "{}", e.message);
        let a = ok(tr, "-L 2 d");
        assert_eq!((a.opts, a.pos), (o(&[("-L", "2")]), s(&["d"])));
        assert_eq!(usage(tr, "-L").kind, Kind::Usage);
        assert_eq!(positional_index(fd, &toks("-name")), None);
        assert_eq!(positional_index(fd, &toks("-name x")), Some(0));
        assert_eq!(positional_index(fd, &toks("-name x /d")), Some(1));
        assert_eq!(parse("find -name").unwrap_err().kind, Kind::Usage);
        assert_eq!(parse("find /a /b").unwrap_err().kind, Kind::Usage);
        assert_eq!(
            parse("find -name x /d").unwrap().1,
            parse("find /d -name x").unwrap().1
        );
        let (c, a) = parse("get -r -f a b").unwrap();
        assert_eq!(
            (c.name, a.flags, a.pos),
            ("get", s(&["-r", "-f"]), s(&["a", "b"]))
        );
        let (c, a) = parse("mkdir -p x").unwrap();
        assert_eq!((c.name, a.flags, a.pos), ("mkdir", s(&["-p"]), s(&["x"])));
    }

    #[test]
    fn out_written() {
        for json in [false, true] {
            let cap = Capture::default();
            let mut out = Out::to(json, Box::new(cap.clone()));
            out.begin("x");
            assert!(!out.written());
            out.text("a");
            assert_eq!(out.written(), !json, "json {json}: text");
            out.begin("y");
            assert!(!out.written());
            out.obj(&json!({"type": "pwd"}));
            assert_eq!(out.written(), json, "json {json}: obj");
            out.begin("z");
            out.raw(b"bytes").unwrap();
            assert!(out.written(), "json {json}: raw");
            out.begin("w");
            assert!(!out.written());
            out.diag("to stderr");
            assert!(!out.written(), "json {json}: diag goes to stderr");
        }
    }
}
