//! The interactive front end: tokeniser and quoting, completion, prompt, history, the loop.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustyline::completion::{Completer, Pair};
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Context, Editor, Helper};
use tokio::runtime::Runtime;

use crate::commands::{self, Arg, Out};
use crate::session::{Entry, Error, Result, Session, resolve_path, safe};
use crate::{Flow, TERM, run_blocking};

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0b' | '\x0c')
}

/// Splits a line into tokens: ASCII white space separates; `'…'` is literal; in `"…"` only `\"`
/// and `\\` are escapes; outside quotes a backslash takes the next character literally.
pub fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if is_space(c) => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(Error::usage("unterminated ' quote")),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                            cur.push(chars.next().unwrap())
                        }
                        Some(c) => cur.push(c),
                        None => return Err(Error::usage("unterminated \" quote")),
                    }
                }
            }
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => return Err(Error::usage("trailing backslash")),
                }
            }
            c => {
                in_token = true;
                cur.push(c);
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    Ok(out)
}

/// For completion: the finished tokens, the token under the cursor (unquoted) and the byte offset
/// where it starts. Never fails: an open quote runs to the end, a trailing backslash is ignored.
pub fn partial(line: &str) -> (Vec<String>, String, usize) {
    let mut done = Vec::new();
    let mut cur = String::new();
    let mut start = line.len();
    let mut in_token = false;
    let mut it = line.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if !in_token && !is_space(c) {
            in_token = true;
            start = i;
        }
        match c {
            c if is_space(c) => {
                if in_token {
                    done.push(std::mem::take(&mut cur));
                    in_token = false;
                    start = line.len();
                }
            }
            '\'' => {
                for (_, c) in it.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                while let Some((_, c)) = it.next() {
                    match c {
                        '"' => break,
                        '\\' if matches!(it.peek(), Some((_, '"' | '\\'))) => {
                            cur.push(it.next().unwrap().1)
                        }
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                if let Some((_, c)) = it.next() {
                    cur.push(c);
                }
            }
            c => cur.push(c),
        }
    }
    (done, cur, start)
}

/// The canonical form of a token: a backslash before ASCII white space, quotes and backslashes;
/// `''` for the empty string. `tokenize(&quote(x)) == [x]`.
pub fn quote(token: &str) -> String {
    if token.is_empty() {
        return "''".to_string();
    }
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        if is_space(c) || matches!(c, '\'' | '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Candidates for a remote path: `dir_part` as typed (up to and including its last separator),
/// `prefix` the rest. Exact-byte prefix match; directories only for `RemoteDir`.
pub fn candidates(entries: &[Entry], dir_part: &str, prefix: &str, arg: Arg) -> Vec<Pair> {
    entries
        .iter()
        .filter(|e| e.name.starts_with(prefix) && (arg != Arg::RemoteDir || e.is_dir()))
        .map(|e| {
            let (end, mark) = if e.is_dir() { ("/", "/") } else { (" ", "") };
            Pair {
                display: format!("{}{mark}", safe(&e.name)),
                replacement: format!("{}{end}", quote(&format!("{dir_part}{}", e.name))),
            }
        })
        .collect()
}

/// Candidates for a local path: the directory part as typed (with `~`) is read with `read_dir`,
/// names that start with the rest are offered, directories only for `LocalDir`. No request.
pub fn local_candidates(word: &str, arg: Arg) -> Vec<Pair> {
    let cut = word.rfind('/').map_or(0, |i| i + 1);
    let (dir_part, prefix) = word.split_at(cut);
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        crate::transfer::expand(dir_part)
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            // A link to a directory counts as one.
            let is_dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
            (name.starts_with(prefix) && (arg != Arg::LocalDir || is_dir)).then_some((name, is_dir))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(name, is_dir)| {
            let (end, mark) = if is_dir { ("/", "/") } else { (" ", "") };
            Pair {
                display: format!("{}{mark}", safe(&name)),
                replacement: format!("{}{end}", quote(&format!("{dir_part}{name}"))),
            }
        })
        .collect()
}

/// How long the network work of one Tab may take (docs/architecture.md, "Completion").
const COMPLETION_BOUND: Duration = Duration::from_secs(5);

struct ShellHelper {
    sess: Rc<RefCell<Session>>,
    rt: tokio::runtime::Handle,
}

impl ShellHelper {
    /// Runs network work for a completion under the bound; any failure gives no candidates.
    fn bounded<T>(
        &self,
        sess: &mut Session,
        work: impl AsyncFnOnce(&mut Session) -> Result<T>,
    ) -> Option<T> {
        let r = self
            .rt
            .block_on(async { tokio::time::timeout(COMPLETION_BOUND, work(sess)).await });
        match r {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                if e.is_lost() {
                    sess.mark_dead();
                }
                None
            }
            Err(_) => {
                sess.abandon();
                None
            }
        }
    }

    fn complete_line(&self, line: &str) -> (usize, Vec<Pair>) {
        let (done, word, start) = partial(line);
        let Some(first) = done.first() else {
            let c = commands::COMMANDS
                .iter()
                .filter(|c| c.name.starts_with(&word))
                .map(|c| Pair {
                    display: c.name.to_string(),
                    replacement: format!("{} ", c.name),
                })
                .collect();
            return (start, c);
        };
        let Some(cmd) = commands::find(first) else {
            return (start, vec![]);
        };
        let Some(index) = commands::positional_index(cmd, &done[1..]) else {
            return (start, vec![]);
        };
        let Some(&arg) = cmd.args.get(index) else {
            return (start, vec![]);
        };
        if matches!(arg, Arg::LocalPath | Arg::LocalDir) {
            return (start, local_candidates(&word, arg));
        }
        let mut sess = self.sess.borrow_mut();
        let found = match arg {
            Arg::LocalPath | Arg::LocalDir => Vec::new(),
            Arg::Command => commands::COMMANDS
                .iter()
                .map(|c| c.name)
                .chain(["all"])
                .filter(|n| n.starts_with(&word))
                .map(|n| Pair {
                    display: n.to_string(),
                    replacement: format!("{n} "),
                })
                .collect(),
            Arg::Share => {
                let list = match sess.known_shares() {
                    Some(l) => Some(l),
                    None => self.bounded(&mut sess, async |s: &mut Session| s.shares().await),
                };
                list.map(|l| {
                    l.iter()
                        .filter(|s| s.name.starts_with(&word))
                        .map(|s| Pair {
                            display: safe(&s.name).into_owned(),
                            replacement: format!("{} ", quote(&s.name)),
                        })
                        .collect()
                })
                .unwrap_or_default()
            }
            Arg::RemoteDir | Arg::RemotePath => {
                if sess.share().is_none() {
                    return (start, vec![]);
                }
                let cut = word.rfind(['/', '\\']).map_or(0, |i| i + 1);
                let (dir_part, prefix) = word.split_at(cut);
                let dir = resolve_path(sess.cwd(), dir_part);
                let entries =
                    self.bounded(&mut sess, async |s: &mut Session| s.list_cached(&dir).await);
                entries
                    .map(|e| candidates(&e, dir_part, prefix, arg))
                    .unwrap_or_default()
            }
        };
        (start, found)
    }
}

impl Completer for ShellHelper {
    type Candidate = Pair;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(self.complete_line(&line[..pos]))
    }
}
impl Hinter for ShellHelper {
    type Hint = String;
}
impl Highlighter for ShellHelper {}
impl Validator for ShellHelper {}
impl Helper for ShellHelper {}

/// `$RSMB_HISTORY` (empty: none), else `$XDG_STATE_HOME/rsmb/history`, else
/// `$HOME/.local/state/rsmb/history`.
fn history_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RSMB_HISTORY") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("rsmb/history"))
}

/// The terminal's attributes before the first prompt. rustyline holds the terminal in raw mode
/// while it reads a line; a SIGTERM exits from the signal task and must give it back.
static COOKED: std::sync::OnceLock<libc::termios> = std::sync::OnceLock::new();

fn save_terminal() {
    // SAFETY: tcgetattr fills the struct it is given; zeroed termios is a valid value to fill.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(0, &mut t) } == 0 {
        let _ = COOKED.set(t);
    }
}

/// Puts the terminal back as `run` found it; nothing if `run` has not started.
pub fn restore_terminal() {
    if let Some(t) = COOKED.get() {
        // SAFETY: `t` is a termios from tcgetattr.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, t) };
    }
}

/// The interactive loop. Returns the exit code.
pub fn run(rt: &Runtime, sess: Session, out: &mut Out) -> u8 {
    let sess = Rc::new(RefCell::new(sess));
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .completion_prompt_limit(100)
        .max_history_size(1000)
        .and_then(|b| b.history_ignore_dups(true))
        .map(|b| b.auto_add_history(true).build());
    let editor = config.and_then(Editor::<ShellHelper, DefaultHistory>::with_config);
    let mut ed = match editor {
        Ok(e) => e,
        Err(e) => {
            commands::report(
                out,
                &Error::new(crate::session::Kind::LocalIo, format!("terminal: {e}")),
            );
            return 1;
        }
    };
    ed.set_helper(Some(ShellHelper {
        sess: sess.clone(),
        rt: rt.handle().clone(),
    }));
    let history = history_path();
    if let Some(h) = &history {
        let _ = ed.load_history(h);
        if let Some(dir) = h.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    save_terminal();
    out.interactive = true;
    loop {
        let prompt = format!("{}> ", safe(&sess.borrow().location()));
        match ed.readline(&prompt) {
            Ok(line) => {
                // Each line is saved as it is entered: a SIGTERM must not lose the session.
                if let Some(h) = &history {
                    let _ = ed.append_history(h);
                }
                let mut flow = run_blocking(rt, &mut sess.borrow_mut(), out, &line, false);
                if let Flow::Confirm(message) = flow {
                    // History off for the answer; Ctrl-C and Ctrl-D mean no.
                    ed.set_auto_add_history(false);
                    let answer = ed.readline(&format!("{} Overwrite? [y/N] ", safe(&message)));
                    ed.set_auto_add_history(true);
                    flow = match answer {
                        Ok(a) if a.trim_start().starts_with(['y', 'Y']) => {
                            run_blocking(rt, &mut sess.borrow_mut(), out, &line, true)
                        }
                        _ => {
                            let line = line.trim();
                            let e = Error::new(crate::session::Kind::AlreadyExists, message);
                            out.error(&e, Some(line));
                            out.done(line, false);
                            Flow::Failed(1)
                        }
                    };
                }
                match flow {
                    Flow::Exit => break 0,
                    Flow::Failed(130) if TERM.load(Ordering::SeqCst) => break 130,
                    _ => {}
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break 0,
            Err(e) => {
                commands::report(
                    out,
                    &Error::new(crate::session::Kind::LocalIo, format!("terminal: {e}")),
                );
                break 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(var: &str) -> Vec<String> {
        let dir = env!("CARGO_MANIFEST_DIR");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {dir}/testenv/samba/names.sh; printf '%s\\n' \"${var}\""
            ))
            .output()
            .unwrap();
        assert!(out.status.success());
        let s = String::from_utf8(out.stdout).unwrap();
        let v: Vec<String> = s
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        assert!(v.len() >= 9, "{var}: {v:?}");
        v
    }

    fn t(line: &str) -> Vec<String> {
        tokenize(line).unwrap()
    }

    #[test]
    fn tokenize_rules() {
        assert_eq!(t(" a \t b\r\n"), ["a", "b"]);
        assert_eq!(t("'a  b\\'"), ["a  b\\"]);
        assert_eq!(t(r#""a \" \\ \x""#), [r#"a " \ \x"#]);
        assert_eq!(t(r"a\ b \' \\"), ["a b", "'", "\\"]);
        assert_eq!(t("a' 'b\"c\"d"), ["a bcd"]);
        assert_eq!(t("'' x"), ["", "x"]);
        assert_eq!(t("$HOME *.txt #x"), ["$HOME", "*.txt", "#x"]);
        assert_eq!(t("cd tree\\\\d0"), ["cd", "tree\\d0"]);
        assert_eq!(tokenize("'open").unwrap_err().kind.name(), "usage");
        assert_eq!(tokenize("\"open").unwrap_err().kind.name(), "usage");
        assert_eq!(tokenize("trailing\\").unwrap_err().kind.name(), "usage");
    }

    #[test]
    fn ideographic_space_stays() {
        assert_eq!(t("cd 東京\u{3000}大阪"), ["cd", "東京\u{3000}大阪"]);
    }

    #[test]
    fn fixture_names_round_trip() {
        for n in names("CJK_NAMES").into_iter().chain(names("SPECIAL_NAMES")) {
            assert_eq!(
                t(&format!("ls {}", quote(&n))),
                ["ls".to_string(), n.clone()],
                "{n:?}"
            );
        }
        assert_eq!(quote(""), "''");
        assert_eq!(t(&quote("")), [""]);
    }

    #[test]
    fn partial_survives_any_cut() {
        for n in names("CJK_NAMES").into_iter().chain(names("SPECIAL_NAMES")) {
            let line = format!("cd {}", quote(&n));
            for (i, _) in line.char_indices().chain([(line.len(), ' ')]) {
                let (done, word, start) = partial(&line[..i]);
                assert!(start <= i);
                if i > 3 {
                    assert_eq!(done, ["cd"], "{line:?} cut at {i}");
                    assert!(n.starts_with(&word), "{n:?} cut at {i}: {word:?}");
                }
            }
            let (done, word, start) = partial(&line);
            assert_eq!((done, word, start), (vec!["cd".to_string()], n.clone(), 3));
        }
        assert_eq!(partial("cd "), (vec!["cd".to_string()], String::new(), 3));
        assert_eq!(
            partial("cd 'a b"),
            (vec!["cd".to_string()], "a b".to_string(), 3)
        );
        assert_eq!(
            partial("cd a\\"),
            (vec!["cd".to_string()], "a".to_string(), 3)
        );
    }

    fn entry(name: &str, dir: bool) -> Entry {
        Entry {
            name: name.to_string(),
            size: 0,
            mtime: 0,
            attributes: if dir { 0x10 } else { 0x20 },
            file_id: 0,
        }
    }

    #[test]
    fn candidates_from_entries() {
        let nfc = "\u{d55c}\u{ae00}";
        let nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        let es = [
            entry("dir with space", true),
            entry("dir.txt", false),
            entry("Dir2", true),
            entry(nfc, true),
            entry(nfd, true),
        ];
        let c = candidates(&es, "", "dir", Arg::RemoteDir);
        assert_eq!(c.len(), 1, "directories only, exact case");
        assert_eq!(c[0].replacement, "dir\\ with\\ space/");
        assert_eq!(c[0].display, "dir with space/");
        let c = candidates(&es, "a b/", "dir", Arg::RemotePath);
        let r: Vec<&str> = c.iter().map(|p| p.replacement.as_str()).collect();
        assert_eq!(r, ["a\\ b/dir\\ with\\ space/", "a\\ b/dir.txt "]);
        assert_eq!(c[1].display, "dir.txt");
        for p in &c {
            let back = t(&format!("ls {}", p.replacement));
            assert!(back[1].starts_with("a b/dir"), "{back:?}");
        }
        let c = candidates(&es, "", &nfc[..3], Arg::RemoteDir);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].display, format!("{nfc}/"));
        let c = candidates(&es, "", &nfd[..3], Arg::RemoteDir);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].display, format!("{nfd}/"));
    }

    #[test]
    fn local_candidates() {
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-tmp")
            .join(format!("{}-local-candidates", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("photos dir")).unwrap();
        std::fs::create_dir_all(d.join("pdir")).unwrap();
        std::fs::write(d.join("p file.txt"), b"x").unwrap();
        std::fs::write(d.join("other.txt"), b"x").unwrap();
        let base = format!("{}/", d.display());
        let reps = |word: &str, arg| {
            super::local_candidates(word, arg)
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>()
        };
        let q = |s: &str| quote(&format!("{base}{s}"));
        assert_eq!(
            reps(&format!("{base}p"), Arg::LocalPath),
            [
                format!("{} ", q("p file.txt")),
                format!("{}/", q("pdir")),
                format!("{}/", q("photos dir"))
            ]
        );
        assert_eq!(
            reps(&format!("{base}p"), Arg::LocalDir),
            [format!("{}/", q("pdir")), format!("{}/", q("photos dir"))],
            "directories only"
        );
        let c = super::local_candidates(&format!("{base}ph"), Arg::LocalDir);
        assert_eq!(c[0].display, "photos dir/");
        assert!(
            c[0].replacement.contains("photos\\ dir/"),
            "{}",
            c[0].replacement
        );
        assert!(reps(&format!("{base}zz"), Arg::LocalPath).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// After a flag that takes a value Tab offers nothing (and sends nothing); after its value
    /// the directory argument completes again.
    #[test]
    fn no_candidates_for_a_flag_value() {
        for line in [
            "find -name ",
            "find /tree -iname ",
            "tree -L ",
            "find -maxdepth ",
        ] {
            let (done, _, _) = partial(line);
            let cmd = commands::find(&done[0]).unwrap();
            assert_eq!(
                commands::positional_index(cmd, &done[1..]),
                None,
                "{line:?}"
            );
        }
        for line in ["find -name x ", "tree -L 2 ", "find "] {
            let (done, _, _) = partial(line);
            let cmd = commands::find(&done[0]).unwrap();
            let i = commands::positional_index(cmd, &done[1..]).unwrap();
            assert_eq!(cmd.args.get(i), Some(&Arg::RemoteDir), "{line:?}");
        }
    }
}
