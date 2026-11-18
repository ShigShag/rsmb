//! The one traversal routine: a breadth-first walk of a remote tree with up to `L` listings in
//! flight, results taken in the order the listings were queued, a loop check by file id, and caps
//! for servers without file ids. Generic over the function that lists a directory, so the unit
//! tests walk fake trees. Also the pure parts of `tree` and `find` (docs/architecture.md).

use std::cmp::Ordering;
use std::collections::hash_map::Entry as Slot;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;

use tokio::task::JoinSet;

use crate::session::{Entry, Error, Kind, Result, display_path};

/// Listings in flight (or waiting for an earlier one) in a walk; small files in flight in a
/// recursive transfer.
pub const L: usize = 16;
/// Depth limit for directories whose file id is 0.
pub const DEPTH_CAP: usize = 100;
/// How many directories without a file id one walk enters.
pub const NO_ID_CAP: usize = 100_000;

/// One listing without `.` and `..`, and their file ids (0 = none).
pub struct Listing {
    pub entries: Vec<Entry>,
    pub id: u64,
    pub parent: u64,
}

/// A listed directory, its entries sorted by name. The start is depth 0, its entries depth 1.
pub struct Dir {
    pub path: Vec<String>,
    pub depth: usize,
    pub entries: Vec<Entry>,
}

#[derive(Default)]
pub struct Report {
    /// Listing refused with access_denied.
    pub denied: Vec<Vec<String>>,
    /// Not entered: its id belongs to a directory that was listed (where; None: the start's
    /// parent, which the walk does not list).
    pub links: Vec<(Vec<String>, Option<Vec<String>>)>,
    /// Not entered: no file id, and deeper than `DEPTH_CAP` or past `NO_ID_CAP`.
    pub capped: Vec<Vec<String>>,
    /// Any other error for one directory.
    pub failed: Vec<(Vec<String>, Error)>,
    pub listed: usize,
}

/// Walks the tree below `start`. The start's own error is the walk's error. `enter(path, entry)`
/// false skips a directory silently (the caller reports it); with `max_depth` a directory at that
/// depth or deeper is not listed. A lost connection or an interrupt ends the walk once the
/// listings in flight have returned. `on_dir` is called once per listed directory, in the order
/// the listings were queued (a breadth-first walk done one listing at a time, whatever the
/// network does); when it returns false nothing more is started, the listings in flight are
/// awaited and dropped, and the walk returns what it has.
pub async fn walk<F, Fut>(
    list: F,
    start: &[String],
    max_depth: Option<usize>,
    enter: &dyn Fn(&[String], &Entry) -> bool,
    on_dir: &mut dyn FnMut(Dir) -> bool,
) -> Result<Report>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = Result<Listing>> + Send + 'static,
{
    walk_capped(list, start, max_depth, enter, on_dir, NO_ID_CAP).await
}

struct State {
    /// File id → the path queued to be listed under it (None: the start's parent).
    seen: HashMap<u64, Option<Vec<String>>>,
    /// File id → the other paths found to the same directory (with their depth and the number
    /// they were found as), in the order found: listed in turn if the path ahead of them fails,
    /// else reported as `links`.
    dups: BTreeMap<u64, Vec<(usize, Vec<String>, usize)>>,
    found: usize,
    /// Path, depth, file id (0 = none) of what is to be listed.
    queue: VecDeque<(Vec<String>, usize, u64)>,
    report: Report,
    no_id: usize,
    no_id_cap: usize,
    max_depth: Option<usize>,
}

impl State {
    fn take(
        &mut self,
        path: Vec<String>,
        depth: usize,
        mut l: Listing,
        enter: &dyn Fn(&[String], &Entry) -> bool,
        on_dir: &mut dyn FnMut(Dir) -> bool,
    ) -> bool {
        self.report.listed += 1;
        l.entries.sort_by(|a, b| a.name.cmp(&b.name));
        let d = depth + 1;
        for e in l.entries.iter().filter(|e| e.is_dir()) {
            let mut p = path.clone();
            p.push(e.name.clone());
            if !enter(&p, e) || self.max_depth.is_some_and(|m| d >= m) {
                continue;
            }
            if e.name.contains('\u{FFFD}') {
                // A name the server sent with an unpaired UTF-16 surrogate: what we would send
                // back is another name (docs/known-limitations.md).
                let shown = display_path(&p);
                let why = "the server's name has a character that cannot be sent back to it \
                           (an unpaired UTF-16 surrogate); not listed";
                let e = Error::new(Kind::InvalidName, format!("{shown}: {why}"));
                self.report.failed.push((p, e));
                continue;
            }
            if e.file_id == 0 {
                if d > DEPTH_CAP || self.no_id >= self.no_id_cap {
                    self.report.capped.push(p);
                    continue;
                }
                self.no_id += 1;
            } else {
                match self.seen.entry(e.file_id) {
                    Slot::Vacant(v) => {
                        v.insert(Some(p.clone()));
                    }
                    Slot::Occupied(_) => {
                        self.found += 1;
                        let dup = (self.found, p, d);
                        self.dups.entry(e.file_id).or_default().push(dup);
                        continue;
                    }
                }
            }
            self.queue.push_back((p, d, e.file_id));
        }
        on_dir(Dir {
            path,
            depth,
            entries: l.entries,
        })
    }

    /// The listing of a directory queued under `id` failed: the next path found to it is listed
    /// instead, or, when there is none yet, a later one will be.
    fn failed(&mut self, id: u64) {
        if id == 0 {
            return;
        }
        match self.dups.get_mut(&id).filter(|v| !v.is_empty()) {
            Some(v) => {
                let (_, p, d) = v.remove(0);
                self.seen.insert(id, Some(p.clone()));
                self.queue.push_back((p, d, id));
            }
            None => {
                self.seen.remove(&id);
            }
        }
    }

    /// The paths still waiting behind a directory that was listed are not entered.
    fn finish(mut self) -> Report {
        let mut links = Vec::new();
        for (id, paths) in std::mem::take(&mut self.dups) {
            let first = self.seen.get(&id).cloned().flatten();
            links.extend(paths.into_iter().map(|(n, p, _)| (n, p, first.clone())));
        }
        links.sort_by_key(|l| l.0);
        self.report
            .links
            .extend(links.into_iter().map(|(_, p, first)| (p, first)));
        self.report
    }
}

async fn walk_capped<F, Fut>(
    list: F,
    start: &[String],
    max_depth: Option<usize>,
    enter: &dyn Fn(&[String], &Entry) -> bool,
    on_dir: &mut dyn FnMut(Dir) -> bool,
    no_id_cap: usize,
) -> Result<Report>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = Result<Listing>> + Send + 'static,
{
    let first = list(start.to_vec()).await?;
    let mut st = State {
        seen: HashMap::new(),
        dups: BTreeMap::new(),
        found: 0,
        queue: VecDeque::new(),
        report: Report::default(),
        no_id: 0,
        no_id_cap,
        max_depth,
    };
    if first.id != 0 {
        st.seen.insert(first.id, Some(start.to_vec()));
    }
    // Below the share root only: the root's `..` (0 on Samba) is no directory of the share.
    if first.parent != 0 && !start.is_empty() {
        st.seen.entry(first.parent).or_insert(None);
    }
    let mut go = st.take(start.to_vec(), 0, first, enter, on_dir);
    let mut set = JoinSet::new();
    let mut fatal: Option<Error> = None;
    // Results that arrived before an earlier one, by sequence number.
    let mut ready: BTreeMap<u64, (Vec<String>, usize, u64, Result<Listing>)> = BTreeMap::new();
    let (mut spawned, mut taken) = (0u64, 0u64);
    loop {
        while fatal.is_none() && go && spawned - taken < L as u64 {
            let Some((p, d, id)) = st.queue.pop_front() else {
                break;
            };
            let work = list(p.clone());
            let seq = spawned;
            set.spawn(async move { (seq, p, d, id, work.await) });
            spawned += 1;
        }
        let Some(joined) = set.join_next().await else {
            break;
        };
        let (seq, p, d, id, r) = joined.map_err(|e| Error::other(format!("walk: {e}")))?;
        if fatal.is_some() || !go {
            continue;
        }
        match r {
            Err(e) if e.is_lost() || e.kind == Kind::Interrupted => {
                fatal = Some(e);
                continue;
            }
            r => {
                ready.insert(seq, (p, d, id, r));
            }
        }
        while go && let Some((p, d, id, r)) = ready.remove(&taken) {
            taken += 1;
            match r {
                Ok(l) => go = st.take(p, d, l, enter, on_dir),
                Err(e) => {
                    if e.kind == Kind::AccessDenied {
                        st.report.denied.push(p);
                    } else {
                        st.report.failed.push((p, e));
                    }
                    st.failed(id);
                }
            }
        }
    }
    match fatal {
        Some(e) => Err(e),
        None => Ok(st.finish()),
    }
}

// ---------------------------------------------------------------------------------------------
// tree and find: the pure parts (docs/commands.md, "tree" and "find")

/// Entries `tree` collects before it gives up.
pub const TREE_MAX: usize = 500_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

/// A glob over the last component of a path: `*`, `?`, `[...]` with ranges and `!` or `^`, `\`
/// escapes; on Unicode scalar values; `fold` for `-iname`.
#[derive(Debug)]
pub struct Glob {
    toks: Vec<Tok>,
    fold: bool,
}

/// The one-character form a case mapping gives, None when it gives several (`ß` upper is `SS`).
fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// With `fold`: the same character, or equal one-character lower-case forms, or equal
/// one-character upper-case forms.
fn same(a: char, b: char, fold: bool) -> bool {
    if a == b {
        return true;
    }
    if !fold {
        return false;
    }
    let lower = |c: char| single(c.to_lowercase());
    let upper = |c: char| single(c.to_uppercase());
    (lower(a).is_some() && lower(a) == lower(b)) || (upper(a).is_some() && upper(a) == upper(b))
}

/// `[...]` starting after its `[`: the class and the index after its `]`; None when no `]`
/// closes it (the `[` is then a literal).
fn class(c: &[char], mut j: usize) -> Result<Option<(Tok, usize)>> {
    let mut negated = false;
    if j < c.len() && (c[j] == '!' || c[j] == '^') {
        negated = true;
        j += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    // One member, `\` taking the next character literally.
    let member = |j: usize| -> (char, usize) {
        if c[j] == '\\' && j + 1 < c.len() {
            (c[j + 1], j + 2)
        } else {
            (c[j], j + 1)
        }
    };
    loop {
        if j >= c.len() {
            return Ok(None);
        }
        if c[j] == ']' && !first {
            return Ok(Some((Tok::Class { negated, ranges }, j + 1)));
        }
        if c[j] == '[' && c.get(j + 1) == Some(&':') {
            return Err(Error::usage(
                "character classes like [:digit:] are not supported; write the range, as [0-9]",
            ));
        }
        first = false;
        let (lo, next) = member(j);
        if next + 1 < c.len() && c[next] == '-' && c[next + 1] != ']' {
            let (hi, after) = member(next + 1);
            ranges.push((lo, hi));
            j = after;
        } else {
            ranges.push((lo, lo));
            j = next;
        }
    }
}

impl Glob {
    /// A usage error for a `/` in the pattern and for `[:` inside a class.
    pub fn new(pattern: &str, fold: bool) -> Result<Glob> {
        if pattern.contains('/') {
            return Err(Error::usage(
                "the pattern matches the last component of a path; it cannot hold /",
            ));
        }
        let c: Vec<char> = pattern.chars().collect();
        let mut toks = Vec::new();
        let mut i = 0;
        while i < c.len() {
            match c[i] {
                '*' => {
                    if toks.last() != Some(&Tok::Star) {
                        toks.push(Tok::Star);
                    }
                    i += 1;
                }
                '?' => {
                    toks.push(Tok::Any);
                    i += 1;
                }
                '\\' if i + 1 < c.len() => {
                    toks.push(Tok::Lit(c[i + 1]));
                    i += 2;
                }
                '[' => match class(&c, i + 1)? {
                    Some((t, next)) => {
                        toks.push(t);
                        i = next;
                    }
                    None => {
                        toks.push(Tok::Lit('['));
                        i += 1;
                    }
                },
                x => {
                    toks.push(Tok::Lit(x));
                    i += 1;
                }
            }
        }
        Ok(Glob { toks, fold })
    }

    fn one(&self, t: &Tok, ch: char) -> bool {
        match t {
            Tok::Lit(l) => same(*l, ch, self.fold),
            Tok::Any => true,
            Tok::Star => false,
            Tok::Class { negated, ranges } => {
                let mut forms = vec![ch];
                if self.fold {
                    forms.extend(single(ch.to_lowercase()));
                    forms.extend(single(ch.to_uppercase()));
                }
                let member = forms
                    .iter()
                    .any(|f| ranges.iter().any(|(lo, hi)| lo <= f && f <= hi));
                member != *negated
            }
        }
    }

    /// The whole name matches. One backtrack point (the last `*`): no pattern is slow.
    pub fn matches(&self, name: &str) -> bool {
        let n: Vec<char> = name.chars().collect();
        let p = &self.toks;
        let (mut pi, mut ni) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        while ni < n.len() {
            if pi < p.len() && p[pi] == Tok::Star {
                star = Some((pi, ni));
                pi += 1;
            } else if pi < p.len() && self.one(&p[pi], n[ni]) {
                pi += 1;
                ni += 1;
            } else if let Some((sp, sn)) = star {
                pi = sp + 1;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == Tok::Star {
            pi += 1;
        }
        pi == p.len()
    }
}

const SIZE_FORM: &str = "write [+|-]<n>[BKMGTP], for example +1M (B or c: bytes; K M G T P: \
                         powers of 1024)";

/// `[+|-]N[BKMGTP]`: how a size must compare, and with how many bytes.
pub fn parse_size(s: &str) -> Result<(Ordering, u64)> {
    let bad = |why: &str| Error::usage(format!("-size {s}: {why}"));
    let (ord, rest) = if let Some(r) = s.strip_prefix('+') {
        (Ordering::Greater, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (Ordering::Less, r)
    } else {
        (Ordering::Equal, s)
    };
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (num, unit) = rest.split_at(end);
    if num.is_empty() {
        return Err(bad(SIZE_FORM));
    }
    let mult: u64 = match unit {
        "" | "B" | "c" => 1,
        "K" | "k" => 1 << 10,
        "M" | "m" => 1 << 20,
        "G" | "g" => 1 << 30,
        "T" | "t" => 1 << 40,
        "P" | "p" => 1 << 50,
        "b" => {
            return Err(bad(
                "b means 512-byte blocks in find; write B or c for bytes",
            ));
        }
        _ => return Err(bad(SIZE_FORM)),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .map(|n| (ord, n))
        .ok_or_else(|| bad("too large"))
}

/// `-maxdepth` and `-L`: a decimal integer of at least 1.
pub fn parse_depth(flag: &str, value: &str) -> Result<usize> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 && value.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
        _ => Err(Error::usage(format!(
            "{flag} {value}: a whole number of at least 1 (1 is the directory's own entries)"
        ))),
    }
}

/// The tests of `find`; every one given must hold.
#[derive(Debug, Default)]
pub struct Filter {
    name: Option<Glob>,
    iname: Option<Glob>,
    dir: Option<bool>,
    size: Option<(Ordering, u64)>,
}

impl Filter {
    /// From the flags with a value; also returns `-maxdepth`. Usage errors for a bad value.
    pub fn parse(opts: &[(String, String)]) -> Result<(Filter, Option<usize>)> {
        let mut f = Filter::default();
        let mut depth = None;
        for (flag, value) in opts {
            let glob = |fold| {
                Glob::new(value, fold)
                    .map_err(|e| Error::usage(format!("{flag} '{value}': {}", e.message)))
            };
            match flag.as_str() {
                "-name" => f.name = Some(glob(false)?),
                "-iname" => f.iname = Some(glob(true)?),
                "-type" => {
                    f.dir = Some(match value.as_str() {
                        "f" => false,
                        "d" => true,
                        _ => {
                            return Err(Error::usage(format!(
                                "-type {value}: f (files) or d (directories)"
                            )));
                        }
                    })
                }
                "-size" => f.size = Some(parse_size(value)?),
                "-maxdepth" => depth = Some(parse_depth(flag, value)?),
                _ => return Err(Error::usage(format!("{flag}: not a test of find"))),
            }
        }
        Ok((f, depth))
    }

    pub fn matches(&self, e: &Entry) -> bool {
        self.name.as_ref().is_none_or(|g| g.matches(&e.name))
            && self.iname.as_ref().is_none_or(|g| g.matches(&e.name))
            && self.dir.is_none_or(|d| d == e.is_dir())
            && self
                .size
                .is_none_or(|(ord, n)| !e.is_dir() && e.size.cmp(&n) == ord)
    }
}

/// `line(depth, dir, entry, text)` of `draw_tree`.
pub type Line<'a> = dyn FnMut(usize, &[String], &Entry, &str) + 'a;

/// The lines of a tree below `start`, depth first, entries in the order of each listing.
/// `nodes`: the listed directories by path. `notes`: why a directory was not entered.
/// `line(depth, dir, entry, text)` gets every entry, the directory it is in and its drawn line.
/// Returns (directories, files) drawn.
pub fn draw_tree(
    start: &[String],
    nodes: &HashMap<Vec<String>, Arc<Vec<Entry>>>,
    notes: &HashMap<Vec<String>, String>,
    line: &mut Line<'_>,
) -> (usize, usize) {
    let mut counts = (0, 0);
    draw(
        start,
        1,
        &mut String::new(),
        nodes,
        notes,
        line,
        &mut counts,
    );
    counts
}

fn draw(
    dir: &[String],
    depth: usize,
    prefix: &mut String,
    nodes: &HashMap<Vec<String>, Arc<Vec<Entry>>>,
    notes: &HashMap<Vec<String>, String>,
    line: &mut Line<'_>,
    counts: &mut (usize, usize),
) {
    let Some(entries) = nodes.get(dir) else {
        return;
    };
    for (i, e) in entries.iter().enumerate() {
        let last = i + 1 == entries.len();
        let mut p = dir.to_vec();
        p.push(e.name.clone());
        let mut text = format!("{prefix}{}{}", if last { "└── " } else { "├── " }, e.name);
        if e.is_dir() {
            text.push('/');
            counts.0 += 1;
        } else {
            counts.1 += 1;
        }
        if let Some(n) = notes.get(&p) {
            text.push_str(&format!(" [{n}]"));
        }
        line(depth, dir, e, &text);
        if e.is_dir() {
            let len = prefix.len();
            prefix.push_str(if last { "    " } else { "│   " });
            draw(&p, depth + 1, prefix, nodes, notes, line, counts);
            prefix.truncate(len);
        }
    }
}

/// Why a directory with the file id of the start's parent is not entered: the walk lists no
/// such directory, so it names none.
pub const ABOVE: &str = "leads above the start, to a directory this walk does not list";

/// One line of the end of a walk: a directory that was not listed, and why.
#[derive(Debug, PartialEq, Eq)]
pub struct EndLine {
    /// "denied" or "skipped".
    pub kind: &'static str,
    pub path: String,
    /// "" (denied), "already_listed", "outside", "gone", "no_file_id".
    pub reason: &'static str,
    pub same_as: Option<String>,
    /// The text line, without `rsmb: `.
    pub text: String,
}

/// What to say after a walk: the denied and the skipped directories, each sorted by path; the
/// other failures (with their path), sorted by path; whether the walk is incomplete.
pub fn end_report(r: &Report) -> (Vec<EndLine>, Vec<Error>, bool) {
    let mut denied: Vec<EndLine> = r
        .denied
        .iter()
        .map(|p| {
            let path = display_path(p);
            EndLine {
                kind: "denied",
                text: format!("{path}: access denied; not listed"),
                path,
                reason: "",
                same_as: None,
            }
        })
        .collect();
    let mut skipped = Vec::new();
    for (p, first) in &r.links {
        let path = display_path(p);
        skipped.push(match first {
            Some(f) => {
                let same = display_path(f);
                EndLine {
                    kind: "skipped",
                    text: format!("{path}: not entered: already listed as {same}"),
                    path,
                    reason: "already_listed",
                    same_as: Some(same),
                }
            }
            None => EndLine {
                kind: "skipped",
                text: format!("{path}: not entered: {ABOVE}"),
                path,
                reason: "outside",
                same_as: None,
            },
        });
    }
    for p in &r.capped {
        let path = display_path(p);
        skipped.push(EndLine {
            kind: "skipped",
            text: format!(
                "{path}: not entered: the server gives no file ids and the walk is at its limit \
                 (depth {DEPTH_CAP}, {NO_ID_CAP} directories)"
            ),
            path,
            reason: "no_file_id",
            same_as: None,
        });
    }
    let mut failed = Vec::new();
    for (p, e) in &r.failed {
        let path = display_path(p);
        if e.kind == Kind::NotFound {
            skipped.push(EndLine {
                kind: "skipped",
                text: format!("{path}: gone before it was listed"),
                path,
                reason: "gone",
                same_as: None,
            });
        } else {
            failed.push(e.clone().at(path));
        }
    }
    denied.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    failed.sort_by(|a, b| a.path.cmp(&b.path));
    let incomplete = !failed.is_empty() || !r.capped.is_empty();
    denied.extend(skipped);
    (denied, failed, incomplete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn dir(name: &str, id: u64) -> Entry {
        Entry {
            name: name.into(),
            size: 0,
            mtime: 0,
            attributes: 0x10,
            file_id: id,
        }
    }

    fn file(name: &str) -> Entry {
        Entry {
            name: name.into(),
            size: 1,
            mtime: 0,
            attributes: 0x20,
            file_id: 7,
        }
    }

    type Tree = Arc<HashMap<Vec<String>, std::result::Result<Listing, Error>>>;

    fn clone_listing(l: &Listing) -> Listing {
        Listing {
            entries: l.entries.clone(),
            id: l.id,
            parent: l.parent,
        }
    }

    fn lister(
        tree: Tree,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    ) -> impl Fn(Vec<String>) -> std::pin::Pin<Box<dyn Future<Output = Result<Listing>> + Send>>
    {
        move |p: Vec<String>| {
            let tree = tree.clone();
            let calls = calls.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(p.clone());
                match tree.get(&p) {
                    Some(Ok(l)) => Ok(clone_listing(l)),
                    Some(Err(e)) => Err(e.clone()),
                    None => Err(Error::new(Kind::NotFound, "no such fake dir")),
                }
            })
        }
    }

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn all(_: &[String], _: &Entry) -> bool {
        true
    }

    #[test]
    fn walk_visits_each_directory_once() {
        // /loops (id 10, parent 1): a (id 11) with b (id 12), self (10), up (1), and a link
        // inside b back to a (11).
        let mut t = HashMap::new();
        t.insert(
            v(&["loops"]),
            Ok(Listing {
                entries: vec![
                    dir("a", 11),
                    dir("self", 10),
                    dir("up", 1),
                    file("file.txt"),
                ],
                id: 10,
                parent: 1,
            }),
        );
        t.insert(
            v(&["loops", "a"]),
            Ok(Listing {
                entries: vec![dir("b", 12)],
                id: 11,
                parent: 10,
            }),
        );
        t.insert(
            v(&["loops", "a", "b"]),
            Ok(Listing {
                entries: vec![dir("back", 11)],
                id: 12,
                parent: 11,
            }),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut seen = Vec::new();
        let r = rt()
            .block_on(walk(
                lister(Arc::new(t), calls.clone()),
                &v(&["loops"]),
                None,
                &all,
                &mut |d| {
                    seen.push(d.path);
                    true
                },
            ))
            .unwrap();
        let mut c = calls.lock().unwrap().clone();
        c.sort();
        assert_eq!(
            c,
            [v(&["loops"]), v(&["loops", "a"]), v(&["loops", "a", "b"])]
        );
        assert_eq!(r.listed, 3);
        assert_eq!(seen.len(), 3);
        let links = r.links.to_vec();
        assert!(links.contains(&(v(&["loops", "self"]), Some(v(&["loops"])))));
        assert!(links.contains(&(v(&["loops", "up"]), None)));
        assert!(links.contains(&(v(&["loops", "a", "b", "back"]), Some(v(&["loops", "a"])))));
        assert_eq!(links.len(), 3);
    }

    /// Every directory has one subdirectory `d`, every id is 0: a tree that never ends.
    fn endless(
        width: usize,
    ) -> impl Fn(Vec<String>) -> std::pin::Pin<Box<dyn Future<Output = Result<Listing>> + Send>>
    {
        move |_p: Vec<String>| {
            Box::pin(async move {
                Ok(Listing {
                    entries: (0..width).map(|i| dir(&format!("d{i}"), 0)).collect(),
                    id: 0,
                    parent: 0,
                })
            })
        }
    }

    #[test]
    fn walk_depth_cap_without_ids() {
        let mut deepest = 0;
        let r = rt()
            .block_on(walk(endless(1), &[], None, &all, &mut |d| {
                deepest = deepest.max(d.depth);
                true
            }))
            .unwrap();
        assert_eq!(deepest, DEPTH_CAP);
        assert_eq!(r.listed, DEPTH_CAP + 1);
        assert_eq!(r.capped.len(), 1);
        assert_eq!(r.capped[0].len(), DEPTH_CAP + 1);
    }

    #[test]
    fn walk_breadth_cap_without_ids() {
        let mut deepest = 0;
        let r = rt()
            .block_on(walk_capped(
                endless(2),
                &[],
                None,
                &all,
                &mut |d| {
                    deepest = deepest.max(d.depth);
                    true
                },
                1_000,
            ))
            .unwrap();
        assert_eq!(
            r.listed, 1_001,
            "the start and 1,000 directories without an id"
        );
        assert!(
            deepest < 20,
            "ended at depth {deepest}, well before {DEPTH_CAP}"
        );
        assert!(!r.capped.is_empty());
    }

    #[test]
    fn walk_goes_on_past_denied() {
        let mut t = HashMap::new();
        t.insert(
            v(&["t"]),
            Ok(Listing {
                entries: vec![dir("denied", 2), dir("gone", 3), dir("ok", 4)],
                id: 1,
                parent: 0,
            }),
        );
        t.insert(
            v(&["t", "denied"]),
            Err(Error::new(Kind::AccessDenied, "/t/denied: access denied")),
        );
        t.insert(
            v(&["t", "ok"]),
            Ok(Listing {
                entries: vec![dir("deeper", 5)],
                id: 4,
                parent: 1,
            }),
        );
        t.insert(
            v(&["t", "ok", "deeper"]),
            Ok(Listing {
                entries: vec![file("x")],
                id: 5,
                parent: 4,
            }),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let r = rt()
            .block_on(walk(
                lister(Arc::new(t), calls.clone()),
                &v(&["t"]),
                None,
                &all,
                &mut |_| true,
            ))
            .unwrap();
        assert_eq!(r.denied, [v(&["t", "denied"])]);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].0, v(&["t", "gone"]));
        assert_eq!(r.failed[0].1.kind, Kind::NotFound);
        assert_eq!(r.listed, 3, "t, t/ok, t/ok/deeper");
    }

    #[test]
    fn walk_width() {
        let now = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let (n2, m2, t2) = (now.clone(), most.clone(), total.clone());
        let list = move |p: Vec<String>| {
            let (now, most, total) = (n2.clone(), m2.clone(), t2.clone());
            async move {
                let c = now.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(c, Ordering::SeqCst);
                total.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                now.fetch_sub(1, Ordering::SeqCst);
                let entries = if p.is_empty() {
                    (0..200).map(|i| dir(&format!("d{i}"), 100 + i)).collect()
                } else {
                    vec![]
                };
                Ok(Listing {
                    entries,
                    id: if p.is_empty() { 1 } else { 0 },
                    parent: 0,
                })
            }
        };
        let r = rt()
            .block_on(walk(list, &[], None, &all, &mut |_| true))
            .unwrap();
        assert_eq!(r.listed, 201);
        assert_eq!(total.load(Ordering::SeqCst), 201);
        assert_eq!(
            most.load(Ordering::SeqCst),
            L,
            "never more than {L} at once"
        );
    }

    #[test]
    fn walk_fatal_error_waits() {
        let started = Arc::new(Mutex::new(Vec::<String>::new()));
        let finished = Arc::new(Mutex::new(Vec::<String>::new()));
        let (s2, f2) = (started.clone(), finished.clone());
        let list = move |p: Vec<String>| {
            let (started, finished) = (s2.clone(), f2.clone());
            async move {
                let name = p.join("/");
                started.lock().unwrap().push(name.clone());
                if p.is_empty() {
                    return Ok(Listing {
                        entries: (0..40).map(|i| dir(&format!("d{i:02}"), 10 + i)).collect(),
                        id: 1,
                        parent: 0,
                    });
                }
                if name == "d00" {
                    return Err(Error::new(
                        Kind::ConnectionLost { silent: false },
                        "connection lost: test",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                finished.lock().unwrap().push(name);
                Ok(Listing {
                    entries: vec![],
                    id: 0,
                    parent: 0,
                })
            }
        };
        let e = rt()
            .block_on(walk(list, &[], None, &all, &mut |_| true))
            .err()
            .unwrap();
        assert!(e.is_lost());
        let s = started.lock().unwrap().clone();
        let f = finished.lock().unwrap().clone();
        assert_eq!(s.len(), 1 + L, "none started after the error: {s:?}");
        assert_eq!(
            f.len(),
            L - 1,
            "every listing in flight returned first: {f:?}"
        );
    }

    #[test]
    fn walk_max_depth() {
        let listed = |max: usize| {
            let mut got = Vec::new();
            rt().block_on(walk(endless(2), &[], Some(max), &all, &mut |d| {
                got.push(d.path);
                true
            }))
            .unwrap();
            got.sort();
            got
        };
        assert_eq!(listed(1), [v(&[])], "max_depth 1: the start only");
        assert_eq!(
            listed(2),
            [v(&[]), v(&["d0"]), v(&["d1"])],
            "max_depth 2: the start and its subdirectories"
        );
    }

    /// A fake tree: path → (listing, delay in ms); every list call is counted before it starts.
    type Timed = Arc<HashMap<Vec<String>, (Listing, u64)>>;

    fn timed_lister(
        tree: Timed,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    ) -> impl Fn(Vec<String>) -> std::pin::Pin<Box<dyn Future<Output = Result<Listing>> + Send>>
    {
        move |p: Vec<String>| {
            calls.lock().unwrap().push(p.clone());
            let tree = tree.clone();
            Box::pin(async move {
                let (l, ms) = tree.get(&p).expect("a fake directory");
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                Ok(clone_listing(l))
            })
        }
    }

    #[test]
    fn walk_stops_when_told() {
        // A root with 40 directories of 10 each: at the third on_dir the walk is told to stop.
        let mut t = HashMap::new();
        t.insert(
            v(&[]),
            (
                Listing {
                    entries: (0..40).map(|i| dir(&format!("d{i:02}"), 10 + i)).collect(),
                    id: 1,
                    parent: 0,
                },
                0,
            ),
        );
        for i in 0..40u64 {
            t.insert(
                v(&[&format!("d{i:02}")]),
                (
                    Listing {
                        entries: (0..10)
                            .map(|j| dir(&format!("e{j}"), 1000 + i * 10 + j))
                            .collect(),
                        id: 10 + i,
                        parent: 1,
                    },
                    5,
                ),
            );
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicUsize::new(0));
        let (c2, f2) = (calls.clone(), finished.clone());
        let inner = timed_lister(Arc::new(t), calls.clone());
        let list = move |p: Vec<String>| {
            let fut = inner(p);
            let f2 = f2.clone();
            async move {
                let r = fut.await;
                f2.fetch_add(1, Ordering::SeqCst);
                r
            }
        };
        let mut n = 0;
        let mut started_at_stop = 0;
        let r = rt().block_on(walk(list, &[], None, &all, &mut |_| {
            n += 1;
            if n == 3 {
                started_at_stop = c2.lock().unwrap().len();
                return false;
            }
            true
        }));
        let r = r.expect("a stopped walk is Ok");
        assert_eq!(n, 3, "on_dir is not called after it said stop");
        assert_eq!(r.listed, 3);
        let started = calls.lock().unwrap().len();
        assert_eq!(
            started, started_at_stop,
            "no listing started after the stop"
        );
        assert_eq!(
            finished.load(Ordering::SeqCst),
            started,
            "every listing in flight returned"
        );
    }

    /// The listing order of a breadth-first walk done one listing at a time.
    fn bfs(tree: &HashMap<Vec<String>, (Listing, u64)>, start: Vec<String>) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut q = VecDeque::from([start]);
        while let Some(p) = q.pop_front() {
            let mut names: Vec<&Entry> = tree[&p].0.entries.iter().filter(|e| e.is_dir()).collect();
            names.sort_by(|a, b| a.name.cmp(&b.name));
            for e in names {
                let mut c = p.clone();
                c.push(e.name.clone());
                q.push_back(c);
            }
            out.push(p);
        }
        out
    }

    #[test]
    fn walk_takes_results_in_queue_order() {
        // Three levels: 20 directories of 3; a listing takes longer the earlier it is queued.
        let mut t = HashMap::new();
        let mut id = 100;
        let level = |names: Vec<String>, id: &mut u64| -> Vec<Entry> {
            names
                .iter()
                .map(|n| {
                    *id += 1;
                    dir(n, *id)
                })
                .collect()
        };
        let top: Vec<String> = (0..20).map(|i| format!("a{i:02}")).collect();
        let entries = level(top.clone(), &mut id);
        t.insert(
            v(&[]),
            (
                Listing {
                    entries,
                    id: 1,
                    parent: 0,
                },
                0,
            ),
        );
        let mut order = 0u64;
        for a in &top {
            let kids: Vec<String> = ["b", "m", "z"].iter().map(|k| k.to_string()).collect();
            let entries = level(kids.clone(), &mut id);
            order += 1;
            t.insert(
                v(&[a]),
                (
                    Listing {
                        entries,
                        id: 2000 + order,
                        parent: 1,
                    },
                    (90 - order) / 4,
                ),
            );
            for k in &kids {
                order += 1;
                t.insert(
                    v(&[a, k]),
                    (
                        Listing {
                            entries: vec![file("f")],
                            id: 3000 + order,
                            parent: 0,
                        },
                        (90 - order) / 4,
                    ),
                );
            }
        }
        let want = bfs(&t, v(&[]));
        let t = Arc::new(t);
        for run in 0..20 {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let c2 = calls.clone();
            let mut got = Vec::new();
            let mut most = 0;
            rt().block_on(walk(
                timed_lister(t.clone(), calls.clone()),
                &[],
                None,
                &all,
                &mut |d| {
                    got.push(d.path);
                    // listings started (not the start) minus results taken (not the start)
                    let ahead = c2.lock().unwrap().len().saturating_sub(got.len());
                    most = most.max(ahead);
                    true
                },
            ))
            .unwrap();
            assert_eq!(got, want, "run {run}: on_dir in breadth-first order");
            assert!(most <= L, "run {run}: {most} listings in flight or waiting");
            let started = calls.lock().unwrap().len();
            assert_eq!(started, want.len());
        }
        // links/: a/to-c and b/c are one directory (id 5); a's listing is slower than b's.
        let mut t = HashMap::new();
        t.insert(
            v(&["links"]),
            (
                Listing {
                    entries: vec![dir("a", 2), dir("b", 3)],
                    id: 1,
                    parent: 9,
                },
                0,
            ),
        );
        t.insert(
            v(&["links", "a"]),
            (
                Listing {
                    entries: vec![dir("to-c", 5)],
                    id: 2,
                    parent: 1,
                },
                30,
            ),
        );
        t.insert(
            v(&["links", "b"]),
            (
                Listing {
                    entries: vec![dir("c", 5)],
                    id: 3,
                    parent: 1,
                },
                1,
            ),
        );
        t.insert(
            v(&["links", "a", "to-c"]),
            (
                Listing {
                    entries: vec![file("file.txt")],
                    id: 5,
                    parent: 3,
                },
                1,
            ),
        );
        let t = Arc::new(t);
        for run in 0..20 {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let mut got = Vec::new();
            let r = rt()
                .block_on(walk(
                    timed_lister(t.clone(), calls),
                    &v(&["links"]),
                    None,
                    &all,
                    &mut |d| {
                        got.push(d.path);
                        true
                    },
                ))
                .unwrap();
            assert_eq!(
                got,
                [
                    v(&["links"]),
                    v(&["links", "a"]),
                    v(&["links", "b"]),
                    v(&["links", "a", "to-c"])
                ],
                "run {run}"
            );
            assert_eq!(
                r.links,
                [(v(&["links", "b", "c"]), Some(v(&["links", "a", "to-c"])))],
                "run {run}"
            );
        }
    }

    #[test]
    fn walk_root_parent_is_not_seeded() {
        // The share root's `..` has the id 5, and so has a directory in it.
        let mut t = HashMap::new();
        t.insert(
            v(&[]),
            Ok(Listing {
                entries: vec![dir("x", 5)],
                id: 1,
                parent: 5,
            }),
        );
        t.insert(
            v(&["x"]),
            Ok(Listing {
                entries: vec![dir("y", 5)],
                id: 7,
                parent: 5,
            }),
        );
        t.insert(
            v(&["x", "y"]),
            Ok(Listing {
                entries: vec![],
                id: 5,
                parent: 7,
            }),
        );
        let t = Arc::new(t);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let r = rt()
            .block_on(walk(lister(t.clone(), calls), &[], None, &all, &mut |_| {
                true
            }))
            .unwrap();
        assert_eq!(
            r.listed, 2,
            "the root and x (whose id 5 is the root's `..`); x/y has x's id"
        );
        assert!(r.links.iter().all(|(_, f)| f.is_some()), "{:?}", r.links);
        // Below the root the parent is seeded: the same id is reported with None.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let r = rt()
            .block_on(walk(lister(t, calls), &v(&["x"]), None, &all, &mut |_| {
                true
            }))
            .unwrap();
        assert_eq!(r.listed, 1);
        assert_eq!(r.links, [(v(&["x", "y"]), None)]);
    }

    #[test]
    fn walk_lists_the_second_path_when_the_first_fails() {
        // L/b/c (id 30) is also reachable as L/<link>. The link sorts before b (so it is queued
        // and fails before b/c is found) or after it (b/c is found while the link's listing is
        // still waiting); either way b/c must be listed when the link's listing failed.
        for link in ["a-to-c", "zz-to-c"] {
            for kind in [Kind::NotFound, Kind::AccessDenied, Kind::Other] {
                let mut t = HashMap::new();
                t.insert(
                    v(&["L"]),
                    Ok(Listing {
                        entries: vec![dir("b", 20), dir(link, 30), dir("zz-last", 40)],
                        id: 10,
                        parent: 1,
                    }),
                );
                t.insert(
                    v(&["L", "b"]),
                    Ok(Listing {
                        entries: vec![dir("c", 30)],
                        id: 20,
                        parent: 10,
                    }),
                );
                t.insert(
                    v(&["L", "b", "c"]),
                    Ok(Listing {
                        entries: vec![file("file.txt")],
                        id: 30,
                        parent: 20,
                    }),
                );
                t.insert(
                    v(&["L", "zz-last"]),
                    Ok(Listing {
                        entries: vec![],
                        id: 40,
                        parent: 10,
                    }),
                );
                t.insert(v(&["L", link]), Err(Error::new(kind, "the link failed")));
                let calls = Arc::new(Mutex::new(Vec::new()));
                let mut got = Vec::new();
                let r = rt()
                    .block_on(walk(
                        lister(Arc::new(t), calls),
                        &v(&["L"]),
                        None,
                        &all,
                        &mut |d| {
                            got.push(d.path);
                            true
                        },
                    ))
                    .unwrap();
                let ctx = format!("{link} {kind:?}");
                assert!(got.contains(&v(&["L", "b", "c"])), "{ctx}: listed {got:?}");
                assert!(r.links.is_empty(), "{ctx}: {:?}", r.links);
                assert_eq!(r.denied.len() + r.failed.len(), 1, "{ctx}");
            }
        }
    }

    #[test]
    fn walk_does_not_enter_a_name_it_could_not_decode() {
        let bad = "a\u{FFFD}b";
        let mut t = HashMap::new();
        t.insert(
            v(&["t"]),
            Ok(Listing {
                entries: vec![dir(bad, 5), dir("ok", 6)],
                id: 1,
                parent: 0,
            }),
        );
        t.insert(
            v(&["t", "ok"]),
            Ok(Listing {
                entries: vec![],
                id: 6,
                parent: 1,
            }),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let r = rt()
            .block_on(walk(
                lister(Arc::new(t), calls.clone()),
                &v(&["t"]),
                None,
                &all,
                &mut |_| true,
            ))
            .unwrap();
        assert!(
            !calls.lock().unwrap().contains(&v(&["t", bad])),
            "the altered name was sent to the server"
        );
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].0, v(&["t", bad]));
        assert_eq!(r.failed[0].1.kind, Kind::InvalidName);
        let (lines, failed, incomplete) = end_report(&r);
        assert!(lines.is_empty() && failed.len() == 1 && incomplete);
    }

    // --- tree and find: the pure parts ---

    fn g(p: &str) -> Glob {
        Glob::new(p, false).unwrap()
    }
    fn gi(p: &str) -> Glob {
        Glob::new(p, true).unwrap()
    }

    /// A variable of testenv/samba/names.sh, one name per line.
    fn fixture_names(var: &str) -> Vec<String> {
        let dir = env!("CARGO_MANIFEST_DIR");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {dir}/testenv/samba/names.sh; printf '%s\\n' \"${var}\""
            ))
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        assert!(v.len() >= 9, "{var}: {v:?}");
        v
    }

    #[test]
    fn glob_basics() {
        assert!(g("file.txt").matches("file.txt"));
        assert!(!g("file.txt").matches("file.txtx"));
        assert!(!g("file.txt").matches("xfile.txt"));
        assert!(g("*").matches(""));
        assert!(g("*").matches("anything at all"));
        assert!(g("*").matches(".hidden"), "a leading dot is not special");
        assert!(g("*.txt").matches(".txt"));
        assert!(g("?").matches("a"));
        assert!(!g("?").matches(""));
        assert!(!g("?").matches("ab"));
        assert!(g("a*b*c").matches("abc"));
        assert!(g("a*b*c").matches("aXXbYYc"));
        assert!(g("a*b*c").matches("abcbc"));
        assert!(!g("a*b*c").matches("aXXbYY"));
        assert!(g("**").matches("x"));
        assert!(g("a**b").matches("aZZb"));
        assert!(
            !g("abcdef").matches("abc"),
            "a pattern longer than the name"
        );
        assert!(g("").matches(""));
        assert!(!g("").matches("x"));
    }

    #[test]
    fn glob_classes() {
        let m = |p: &str, n: &str| g(p).matches(n);
        assert!(m("[abc]", "b") && !m("[abc]", "d"));
        assert!(m("[a-c]", "b") && !m("[a-c]", "d"));
        assert!(m("[!a]", "b") && !m("[!a]", "a"));
        assert!(m("[^a]", "b") && !m("[^a]", "a"));
        assert!(m("[]a]", "]") && m("[]a]", "a") && !m("[]a]", "b"));
        assert!(m("[!]]", "a") && !m("[!]]", "]"));
        assert!(m("[a-]", "-") && m("[a-]", "a") && !m("[a-]", "b"));
        assert!(m("[-a]", "-") && m("[-a]", "a"));
        assert!(m("[[]", "["));
        assert!(m("\\[", "[") && !m("\\[", "\\["));
        assert!(m("\\*", "*") && !m("\\*", "x"));
        assert!(m("\\\\", "\\"));
        assert!(m("a\\", "a\\"), "a lone \\ at the end is a backslash");
        assert!(m("[\\]]", "]"));
        for n in ["\\", "[", "a", "\\]"] {
            assert!(!m("[\\]]", n), "[\\]] matches only ], not {n:?}");
        }
        assert!(
            !m("[z-a]", "a") && !m("[z-a]", "z"),
            "a reversed range holds nothing"
        );
        assert!(m("[", "[") && !m("[", "a"));
        assert!(m("[abc", "[abc"));
        assert!(m("*[*", "x[y") && !m("*[*", "xy"));
        assert!(m("{a,b}.txt", "{a,b}.txt") && !m("{a,b}.txt", "a.txt"));
        for p in ["[[:digit:]]*", "[x[:alpha:]]"] {
            let e = Glob::new(p, false).unwrap_err();
            assert_eq!(e.kind, Kind::Usage, "{p}");
            assert!(e.message.contains("[:digit:]"), "{p}: {}", e.message);
        }
        let e = Glob::new("a/b", false).unwrap_err();
        assert_eq!(e.kind, Kind::Usage);
        assert!(e.message.contains("last component"), "{}", e.message);
    }

    #[test]
    fn glob_unicode() {
        for n in ["简", "𠮷", "😀"] {
            assert!(g("?").matches(n), "? matches {n} (one character)");
            assert!(!g("??").matches(n));
        }
        let (nfc, nfd) = ("caf\u{e9}.txt", "cafe\u{301}.txt");
        assert!(g("caf?.txt").matches(nfc) && !g("caf?.txt").matches(nfd));
        assert!(g("caf??.txt").matches(nfd) && !g("caf??.txt").matches(nfc));
        let ko_nfc = "\u{d55c}\u{ae00}";
        let ko_nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        assert!(
            g(ko_nfc).matches(ko_nfc) && !g(ko_nfc).matches(ko_nfd),
            "no normalisation"
        );
        assert!(g("東京\u{3000}大阪*").matches("東京\u{3000}大阪_0001.txt"));
        assert!(!g("東京\u{3000}大阪*").matches("東京 大阪"));
        assert!(g("[简繁]*").matches("简体中文") && g("[简繁]*").matches("繁體中文"));
        assert!(!g("[简繁]*").matches("한글"));
        assert!(g("[一-龥]*").matches("東京") && !g("[一-龥]*").matches("한글"));
        let names: Vec<String> = fixture_names("CJK_NAMES")
            .into_iter()
            .chain(fixture_names("SPECIAL_NAMES"))
            .collect();
        for n in &names {
            let escaped: String = n.chars().flat_map(|c| ['\\', c]).collect();
            assert!(g(&escaped).matches(n), "{n:?} matches itself escaped");
            assert!(g("*").matches(n), "{n:?} matches *");
            for other in &names {
                if other != n {
                    assert!(
                        !g(&escaped).matches(other),
                        "{n:?} escaped matches {other:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn glob_fold() {
        let pairs: &[(&str, &str, bool)] = &[
            ("CASE.TXT", "case.txt", true),
            ("ｆｕｌｌ*", "ＦＵＬＬ_0001.txt", true),
            ("привет.txt", "Привет.txt", true),
            ("GRÜßE.TXT", "Grüße.txt", true),
            ("GRÜSSE.TXT", "Grüße.txt", false),
            ("CAF\u{c9}.TXT", "caf\u{e9}.txt", true),
            ("CAF\u{c9}.TXT", "cafe\u{301}.txt", false),
            ("[A-C]ase.txt", "case.txt", true),
            ("INDEX", "ındex", true),
            ("INDEX", "index", true),
            ("ındex", "INDEX", true),
            ("Σ", "σ", true),
            ("Σ", "ς", true),
            ("σ", "ς", true),
            ("istanbul", "İstanbul", false),
            ("İSTANBUL", "İstanbul", true),
        ];
        for &(p, n, want) in pairs {
            assert_eq!(gi(p).matches(n), want, "-iname {p} against {n}");
            if !p.contains(['*', '[']) && !n.contains(['*', '[']) {
                assert_eq!(gi(n).matches(p), want, "symmetric: -iname {n} against {p}");
            }
            assert!(
                !g(p).matches(n),
                "-name {p} against {n}: no folding without -iname"
            );
        }
        assert!(gi("[ｆ]*").matches("ＦＵＬＬ"));
    }

    #[test]
    fn glob_is_linear() {
        let name = "a".repeat(5_000);
        let t0 = std::time::Instant::now();
        assert!(!g("a*a*a*a*a*a*a*a*b").matches(&name));
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    }

    #[test]
    fn size_parse() {
        use std::cmp::Ordering::*;
        let ok = |s: &str| parse_size(s).unwrap();
        assert_eq!(ok("0"), (Equal, 0));
        assert_eq!(ok("10"), (Equal, 10));
        assert_eq!(ok("10B"), (Equal, 10));
        assert_eq!(ok("10c"), (Equal, 10));
        assert_eq!(ok("1K"), (Equal, 1_024));
        assert_eq!(ok("1k"), (Equal, 1_024));
        assert_eq!(ok("1M"), (Equal, 1 << 20));
        assert_eq!(ok("1m"), (Equal, 1 << 20));
        assert_eq!(ok("1G"), (Equal, 1 << 30));
        assert_eq!(ok("1T"), (Equal, 1 << 40));
        assert_eq!(ok("1P"), (Equal, 1 << 50));
        assert_eq!(ok("+1M"), (Greater, 1 << 20));
        assert_eq!(ok("-1M"), (Less, 1 << 20));
        for bad in [
            "",
            "+",
            "K",
            "1X",
            "1.5M",
            "1KB",
            "--1",
            "99999999P",
            "10C",
            "10b",
        ] {
            let e = parse_size(bad).unwrap_err();
            assert_eq!(e.kind, Kind::Usage, "{bad:?}");
        }
        assert!(parse_size("10b").unwrap_err().message.contains("512"));
    }

    fn sized(name: &str, size: u64) -> Entry {
        Entry {
            name: name.into(),
            size,
            mtime: 0,
            attributes: 0x20,
            file_id: 9,
        }
    }

    fn filter(opts: &[(&str, &str)]) -> Result<(Filter, Option<usize>)> {
        let o: Vec<(String, String)> = opts
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        Filter::parse(&o)
    }

    #[test]
    fn filter_rules() {
        let f = |o: &[(&str, &str)]| filter(o).unwrap().0;
        let m = 1_048_576u64;
        let (plus, minus, eq) = (
            f(&[("-size", "+1M")]),
            f(&[("-size", "-1M")]),
            f(&[("-size", "1M")]),
        );
        for (size, p, l, e) in [
            (m - 1, false, true, false),
            (m, false, false, true),
            (m + 1, true, false, false),
        ] {
            let x = sized("x", size);
            assert_eq!(
                (plus.matches(&x), minus.matches(&x), eq.matches(&x)),
                (p, l, e),
                "{size}"
            );
        }
        let d = dir("d", 4);
        for o in [
            ("-size", "-1K"),
            ("-size", "0"),
            ("-size", "+0"),
            ("-size", "-1"),
        ] {
            assert!(
                !f(&[o]).matches(&d),
                "a directory never matches -size {}",
                o.1
            );
        }
        assert!(!f(&[("-size", "-1K"), ("-type", "d")]).matches(&d));
        assert!(f(&[("-type", "d")]).matches(&d) && !f(&[("-type", "d")]).matches(&sized("x", 1)));
        assert!(f(&[("-type", "f")]).matches(&sized("x", 1)) && !f(&[("-type", "f")]).matches(&d));
        let all3 = f(&[("-name", "*.txt"), ("-iname", "A*"), ("-size", "+10")]);
        assert!(all3.matches(&sized("a.txt", 11)));
        assert!(!all3.matches(&sized("a.txt", 10)));
        assert!(!all3.matches(&sized("A.TXT", 11)), "-name is exact");
        assert!(!all3.matches(&sized("b.txt", 11)));
        let none = f(&[]);
        assert!(
            none.matches(&d) && none.matches(&sized("x", 0)),
            "no test matches everything"
        );
        assert_eq!(filter(&[("-maxdepth", "3")]).unwrap().1, Some(3));
        for bad in [
            ("-type", "x"),
            ("-maxdepth", "0"),
            ("-maxdepth", "x"),
            ("-maxdepth", "-1"),
            ("-name", "a/b"),
            ("-iname", "[[:alpha:]]"),
        ] {
            assert_eq!(filter(&[bad]).unwrap_err().kind, Kind::Usage, "{bad:?}");
        }
    }

    #[test]
    fn tree_drawing() {
        let a = |v: Vec<Entry>| Arc::new(v);
        let start = v(&["t"]);
        let mut nodes = HashMap::new();
        nodes.insert(
            start.clone(),
            a(vec![
                dir("a", 2),
                dir("empty", 3),
                dir("forbidden", 4),
                file("x y.txt"),
                dir("简体中文", 5),
            ]),
        );
        nodes.insert(v(&["t", "a"]), a(vec![dir("b", 6), file("f1")]));
        nodes.insert(v(&["t", "a", "b"]), a(vec![file("deep")]));
        nodes.insert(v(&["t", "empty"]), a(vec![]));
        nodes.insert(v(&["t", "简体中文"]), a(vec![file("leaf")]));
        let mut notes = HashMap::new();
        notes.insert(v(&["t", "forbidden"]), "access denied".to_string());
        let mut lines = Vec::new();
        let mut depths = Vec::new();
        let counts = draw_tree(&start, &nodes, &notes, &mut |d, p, e, text| {
            lines.push(text.to_string());
            depths.push((d, display_path(p), e.name.clone()));
        });
        let want = "\
├── a/
│   ├── b/
│   │   └── deep
│   └── f1
├── empty/
├── forbidden/ [access denied]
├── x y.txt
└── 简体中文/
    └── leaf";
        assert_eq!(lines.join("\n"), want);
        assert_eq!(counts, (5, 4), "(directories, files)");
        assert_eq!(depths[0], (1, "/t".to_string(), "a".to_string()));
        assert_eq!(depths[2], (3, "/t/a/b".to_string(), "deep".to_string()));
    }

    #[test]
    fn end_report_rules() {
        let mut r = Report {
            denied: vec![v(&["s", "t", "z"]), v(&["s", "t", "b"])],
            links: vec![
                (v(&["s", "t", "y", "link"]), Some(v(&["s", "t", "a"]))),
                (v(&["s", "t", "up"]), None),
            ],
            capped: vec![v(&["s", "t", "deep"])],
            failed: vec![
                (
                    v(&["s", "t", "gone"]),
                    Error::new(Kind::NotFound, "/s/t/gone: not found"),
                ),
                (v(&["s", "t", "bad"]), Error::other("/s/t/bad: something")),
            ],
            listed: 5,
        };
        let (lines, failed, incomplete) = end_report(&r);
        let got: Vec<(&str, &str, &str, Option<&str>)> = lines
            .iter()
            .map(|l| (l.kind, l.path.as_str(), l.reason, l.same_as.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("denied", "/s/t/b", "", None),
                ("denied", "/s/t/z", "", None),
                ("skipped", "/s/t/deep", "no_file_id", None),
                ("skipped", "/s/t/gone", "gone", None),
                ("skipped", "/s/t/up", "outside", None),
                ("skipped", "/s/t/y/link", "already_listed", Some("/s/t/a")),
            ]
        );
        assert_eq!(lines[0].text, "/s/t/b: access denied; not listed");
        assert_eq!(
            lines[4].text,
            "/s/t/up: not entered: leads above the start, to a directory this walk does not list"
        );
        assert_eq!(lines[3].text, "/s/t/gone: gone before it was listed");
        assert!(
            lines.iter().all(|l| !l.text.contains("a link")),
            "no claim about which of two paths is a link"
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].path.as_deref(), Some("/s/t/bad"));
        assert!(incomplete);
        r.capped.clear();
        r.failed.retain(|(_, e)| e.kind == Kind::NotFound);
        let (_, failed, incomplete) = end_report(&r);
        assert!(failed.is_empty() && !incomplete);
    }
}
