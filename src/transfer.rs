//! `get`, `put`, `cat`: temporary names, the local temporary file, the target rules, the recursive
//! transfers, the local walk, the progress line. Every request goes through `session::Remote`.
//! docs/transfers.md states the rules; docs/architecture.md how they are built.

use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use tokio::task::JoinSet;

use crate::commands::{Out, has};
use crate::session::{
    Entry, Error, Kind, Part, Remote, Result, Session, display_path, resolve_path, valid_name,
};
use crate::walk::{self, L};

/// The largest file `cat --json` prints.
pub const CAT_JSON_MAX: u64 = 1_048_576;

// ---------------------------------------------------------------------------------------------
// Names

/// FNV-1a, 64 bits: the same value in every build (`std`'s hasher does not promise that).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The temporary name of a final name (the name, not the path): `.rsmb-<16 hex>.part`, 27 bytes.
pub fn temp_name(final_name: &str) -> String {
    format!(".rsmb-{:016x}.part", fnv1a(final_name.as_bytes()))
}

/// Exactly `.rsmb-`, 16 of `[0-9a-f]`, `.part`.
pub fn is_temp_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 27
        && b.starts_with(b".rsmb-")
        && b.ends_with(b".part")
        && b[6..22]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

fn temp_refused(shown: &str) -> Error {
    Error::new(
        Kind::InvalidName,
        format!(
            "{shown}: a temporary file of rsmb; not transferred. If it is a leftover, delete it"
        ),
    )
}

/// A name a local file may have. Local paths are built only by pushing checked components onto
/// the target directory, so nothing a server sends can name a path outside it.
pub fn local_component(name: &str) -> Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(Error::new(
            Kind::InvalidName,
            format!("'{name}': not a name a local file can have"),
        ));
    }
    Ok(name)
}

/// A leading `~` or `~/` is `$HOME`.
pub fn expand(arg: &str) -> PathBuf {
    if (arg == "~" || arg.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
    {
        let rest = arg.get(2..).unwrap_or("");
        return if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest)
        };
    }
    PathBuf::from(arg)
}

/// A local I/O error. A full disk says so, and `local_full` recognises it (fatal in a recursive
/// transfer).
fn io(what: &Path, e: std::io::Error) -> Error {
    use std::io::ErrorKind as K;
    let text = if matches!(e.kind(), K::StorageFull | K::QuotaExceeded) {
        format!("{}: the local disk is full ({e})", what.display())
    } else {
        format!("{}: {e}", what.display())
    };
    Error::new(Kind::LocalIo, text)
}

// ponytail: the message marks a local full disk; a field on Error if a second fatal kind appears.
fn local_full(e: &Error) -> bool {
    e.kind == Kind::LocalIo && e.message.contains(": the local disk is full (")
}

fn fatal(e: &Error) -> bool {
    e.is_lost() || e.kind == Kind::Interrupted || e.kind == Kind::DiskFull || local_full(e)
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

// ---------------------------------------------------------------------------------------------
// The local temporary file

/// The file a download is written to until it is complete. Dropped without `commit`, it removes
/// itself.
pub struct LocalTemp {
    path: PathBuf,
    file: Option<File>,
    done: bool,
}

impl LocalTemp {
    /// Creates `<dir>/<temp_name(final)>` exclusively (no link followed) and locks it, and makes
    /// sure the path still names the file made. A file already under that name is never adopted:
    /// when its lock is free it is stale, its name is removed and the file made again; when the
    /// lock is held a live transfer has it, and this one is refused.
    pub fn create(dir: &Path, final_name: &str) -> Result<LocalTemp> {
        let path = dir.join(temp_name(final_name));
        let open = |new: bool| {
            let mut o = OpenOptions::new();
            o.read(true)
                .write(true)
                .create_new(new)
                .custom_flags(libc::O_NOFOLLOW);
            o.open(&path)
        };
        for _ in 0..3 {
            // Only an inode this call made is ever written: a file under the name already (a crash
            // that left one, or one planted with any mode or owner) is never adopted.
            let f = match open(true) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let f = match open(false) {
                        Ok(f) => f,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(io(&path, e)), // a link, or not ours to open
                    };
                    if let Err(std::fs::TryLockError::WouldBlock) = f.try_lock() {
                        return Err(Error::other(format!(
                            "{}: another transfer is writing this file",
                            dir.join(final_name).display()
                        )));
                    }
                    // Nobody holds it: remove the name (not the content: it is not written to) and
                    // go round to make our own. Only if the name still is the file just locked.
                    let m = f.metadata().map_err(|e| io(&path, e))?;
                    if let Ok(p) = std::fs::symlink_metadata(&path)
                        && p.dev() == m.dev()
                        && p.ino() == m.ino()
                    {
                        let _ = std::fs::remove_file(&path);
                    }
                    continue;
                }
                Err(e) => return Err(io(&path, e)),
            };
            if let Err(std::fs::TryLockError::WouldBlock) = f.try_lock() {
                continue; // cannot be: nobody else has the inode we just made
            } // a file system without locks: on without one
            let m = f.metadata().map_err(|e| io(&path, e))?;
            match std::fs::symlink_metadata(&path) {
                Ok(p) if p.dev() == m.dev() && p.ino() == m.ino() => {}
                _ => continue, // another rsmb removed the name as stale before it locked it
            }
            return Ok(LocalTemp {
                path,
                file: Some(f),
                done: false,
            });
        }
        Err(Error::other(format!(
            "{}: the temporary file kept changing under rsmb; is another transfer running?",
            path.display()
        )))
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        match self.file.as_mut() {
            Some(f) => f.write_all(data).map_err(|e| io(&self.path, e)),
            None => Err(Error::other("write after commit")),
        }
    }

    /// `sync_data`, then the rename onto `target`. `replace`: `-f` was given **and** the target
    /// existed when the transfer started; otherwise a hard link, which fails if anything has the
    /// name, however it got there.
    pub fn commit(mut self, target: &Path, replace: bool) -> Result<()> {
        match self.try_commit(target, replace) {
            Ok(()) => Ok(()),
            Err(mut e) => {
                e.left.extend(self.discard());
                Err(e)
            }
        }
    }

    fn try_commit(&mut self, target: &Path, replace: bool) -> Result<()> {
        let Some(f) = self.file.as_ref() else {
            return Err(Error::other("commit twice"));
        };
        f.sync_data().map_err(|e| io(&self.path, e))?;
        let exists = || {
            Error::new(
                Kind::AlreadyExists,
                format!("{}: exists; -f replaces it", target.display()),
            )
        };
        if replace {
            match std::fs::symlink_metadata(target) {
                Ok(m) if m.is_dir() => {
                    return Err(Error::new(
                        Kind::IsADirectory,
                        format!(
                            "{}: is a directory; -f never replaces one",
                            target.display()
                        ),
                    ));
                }
                Ok(m) if !m.is_file() && !m.file_type().is_symlink() => {
                    return Err(Error::new(
                        Kind::LocalIo,
                        format!(
                            "{}: not a regular file; -f does not replace it",
                            target.display()
                        ),
                    ));
                }
                _ => {}
            }
            std::fs::rename(&self.path, target).map_err(|e| io(target, e))?;
        } else {
            match std::fs::hard_link(&self.path, target) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&self.path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(exists()),
                Err(e) => {
                    // A file system without hard links.
                    if std::fs::symlink_metadata(target).is_ok() {
                        return Err(exists());
                    }
                    let _ = e;
                    std::fs::rename(&self.path, target).map_err(|e| io(target, e))?;
                }
            }
        }
        self.done = true;
        self.file = None; // the lock is held until here, after the rename
        Ok(())
    }

    /// Removes the temporary file; its path if that failed.
    pub fn discard(&mut self) -> Option<String> {
        self.file = None;
        if self.done {
            return None;
        }
        self.done = true;
        match std::fs::remove_file(&self.path) {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(shown(&self.path)),
        }
    }
}

impl Drop for LocalTemp {
    fn drop(&mut self) {
        self.discard();
    }
}

// ---------------------------------------------------------------------------------------------
// Targets

/// Where `get` writes: no `local` → the local working directory; an existing directory or a
/// trailing `/` → into it; otherwise (without `-r`) the file's own path, whose parent must
/// exist. With `-r` `local` must be an existing directory. Absolute.
pub fn local_target(local: Option<&str>, base: &str, recursive: bool) -> Result<PathBuf> {
    let t = match local {
        None => PathBuf::from(".").join(base),
        Some(a) => {
            let p = expand(a);
            if recursive || a.ends_with('/') || p.is_dir() {
                if !p.is_dir() {
                    return Err(Error::new(
                        Kind::LocalIo,
                        format!("{a}: not an existing local directory"),
                    ));
                }
                p.join(base)
            } else {
                let parent = match p.parent() {
                    Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
                    _ => PathBuf::from("."),
                };
                if !parent.is_dir() {
                    return Err(Error::new(
                        Kind::LocalIo,
                        format!("{a}: the directory {} does not exist", parent.display()),
                    ));
                }
                p
            }
        }
    };
    std::path::absolute(&t).map_err(|e| io(&t, e))
}

/// Where `put` writes: no `remote` → the working directory; a trailing `/` or `\`, or `-r` → into
/// that directory; otherwise the file's own path. A trailing separator is the only way to say
/// "into this directory" for a single file.
pub fn remote_target(
    cwd: &[String],
    remote: Option<&str>,
    base: &str,
    recursive: bool,
) -> Vec<String> {
    let mut v = match remote {
        None => cwd.to_vec(),
        Some(a) if recursive || a.ends_with(['/', '\\']) => resolve_path(cwd, a),
        Some(a) => return resolve_path(cwd, a),
    };
    v.push(base.to_string());
    v
}

// ---------------------------------------------------------------------------------------------
// Progress

/// `1023 B`, `1.0 KiB`, `18.5 MiB`, `1.1 GiB`.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KiB", "MiB", "GiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// One progress line: `get medium.bin  37%  18.5 MiB / 50.0 MiB  17.2 MiB/s  eta 2 s`; in a
/// recursive transfer the label is followed by `120/341 files`.
pub fn render(
    label: &str,
    files: Option<(usize, usize)>,
    done: u64,
    total: u64,
    elapsed: Duration,
) -> String {
    let pct = if total == 0 {
        100
    } else {
        (done as u128 * 100 / total as u128) as u64
    };
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 { done as f64 / secs } else { 0.0 };
    let eta = if rate > 0.0 {
        format!(
            "{} s",
            ((total.saturating_sub(done)) as f64 / rate).ceil() as u64
        )
    } else {
        "-".to_string()
    };
    let files = files.map_or(String::new(), |(a, b)| format!("  {a}/{b} files"));
    format!(
        "{label}{files}  {pct}%  {} / {}  {}/s  eta {eta}",
        human(done),
        human(total),
        rate_unit(rate as u64)
    )
}

/// A rate: B, KiB or MiB (never GiB), so the unit of a fast link reads the same as iperf3's.
pub fn rate_unit(bytes: u64) -> String {
    if bytes < 1 << 30 {
        human(bytes)
    } else {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    }
}

/// The progress line on stderr, redrawn in place; only when stderr is a terminal and not with
/// `--json`.
pub struct Progress {
    active: bool,
    label: String,
    files: Option<(usize, usize)>,
    total: Option<u64>,
    done: u64,
    started: Option<Instant>,
    last: Option<Instant>,
    drawn: bool,
}

impl Progress {
    pub fn new(json: bool, label: &str) -> Progress {
        Progress {
            active: !json && std::io::stderr().is_terminal(),
            label: label.to_string(),
            files: None,
            total: None,
            done: 0,
            started: None,
            last: None,
            drawn: false,
        }
    }

    pub fn total(&mut self, bytes: u64, files: usize) {
        self.total = Some(bytes);
        if files > 0 {
            self.files = Some((0, files));
        }
        self.draw(false);
    }

    pub fn add(&mut self, bytes: u64) {
        self.started.get_or_insert_with(Instant::now);
        self.done += bytes;
        self.draw(Some(self.done) == self.total);
    }

    pub fn file_done(&mut self) {
        if let Some((a, _)) = &mut self.files {
            *a += 1;
        }
        self.draw(false);
    }

    pub fn listing(&mut self, dirs: usize) {
        if !self.active
            || self
                .last
                .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        self.last = Some(Instant::now());
        self.put(&format!("{}  listing: {dirs} directories", self.label));
    }

    fn draw(&mut self, force: bool) {
        let Some(total) = self.total else { return };
        if !self.active
            || (!force
                && self
                    .last
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(100)))
        {
            return;
        }
        self.last = Some(Instant::now());
        let elapsed = self.started.map_or(Duration::ZERO, |t| t.elapsed());
        let line = render(&self.label, self.files, self.done, total, elapsed);
        self.put(&line);
    }

    fn put(&mut self, line: &str) {
        let mut e = std::io::stderr();
        let _ = write!(e, "\r{}\x1b[K", crate::session::safe(line));
        let _ = e.flush();
        self.drawn = true;
    }

    /// Before a line goes to stdout, and at the end.
    pub fn clear(&mut self) {
        if self.drawn {
            let mut e = std::io::stderr();
            let _ = write!(e, "\r\x1b[K");
            let _ = e.flush();
            self.drawn = false;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Output

pub fn seconds(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

fn transfer_line(
    out: &mut Out,
    prog: &mut Progress,
    put: bool,
    remote: &str,
    local: &Path,
    bytes: u64,
    took: Duration,
) {
    prog.clear();
    let (direction, from, to) = if put {
        ("put", shown(local), remote.to_string())
    } else {
        ("get", remote.to_string(), shown(local))
    };
    out.text(&format!(
        "{direction} {from} -> {to}  {bytes} bytes  {:.2} s",
        took.as_secs_f64()
    ));
    out.obj(
        &json!({"type": "transfer", "direction": direction, "remote": remote,
                    "local": shown(local), "bytes": bytes, "seconds": seconds(took)}),
    );
    out.flush();
}

struct Tally {
    files: usize,
    bytes: u64,
    failed: usize,
    directories: usize,
    total: usize,
}

fn summary(out: &mut Out, prog: &mut Progress, put: bool, t: &Tally, took: Duration) {
    prog.clear();
    let direction = if put { "put" } else { "get" };
    out.text(&format!(
        "{} files, {} bytes, {:.2} s; {} not transferred",
        t.files,
        t.bytes,
        took.as_secs_f64(),
        t.failed
    ));
    out.obj(
        &json!({"type": "summary", "direction": direction, "files": t.files,
                    "bytes": t.bytes, "failed": t.failed, "directories": t.directories,
                    "seconds": seconds(took)}),
    );
    out.flush();
}

fn item(out: &mut Out, prog: &mut Progress, tally: &mut Tally, e: &Error) {
    prog.clear();
    tally.failed += 1;
    out.item_error(e);
}

fn no_share(sess: &Session) -> Result<String> {
    sess.share()
        .map(str::to_string)
        .ok_or_else(|| Error::usage("no share selected: use <share> first"))
}

// ---------------------------------------------------------------------------------------------
// get

/// `get [-r] [-f] <remote> [<local>]`
pub async fn get(
    sess: &mut Session,
    out: &mut Out,
    flags: &[String],
    args: &[String],
) -> Result<()> {
    let share = no_share(sess)?;
    let (force, recursive) = (has(flags, "-f"), has(flags, "-r"));
    let src = sess.resolve(&args[0]);
    let base = src.last().cloned().unwrap_or(share);
    let src_shown = display_path(&src);
    if is_temp_name(&base) {
        return Err(temp_refused(&src_shown));
    }
    local_component(&base).map_err(|_| {
        Error::new(
            Kind::InvalidName,
            format!("{src_shown}: '{base}' is not a name a local file can have"),
        )
    })?;
    let target = local_target(args.get(1).map(String::as_str), &base, recursive)?;
    if target
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_temp_name)
    {
        return Err(temp_refused(&shown(&target)));
    }
    if recursive {
        let remote = sess.remote().await?;
        let t0 = Instant::now();
        let mut prog = Progress::new(out.json, &format!("get {src_shown}"));
        let r = get_tree(&remote, out, &src, &target, force, &mut prog, t0).await;
        prog.clear();
        // None: the path is a file, fetched as without -r.
        if let Some(()) = sess.finish(remote, r).await? {
            return Ok(());
        }
    }
    get_file(sess, out, &src, &target, force).await
}

/// Whether `target` may be written: a directory never, an existing file only with `-f`.
fn check_local_target(target: &Path, force: bool) -> Result<bool> {
    match std::fs::symlink_metadata(target) {
        Ok(m) if m.is_dir() => Err(Error::new(
            Kind::IsADirectory,
            format!("{}: is a directory", target.display()),
        )),
        Ok(_) if !force => Err(Error::new(
            Kind::AlreadyExists,
            format!("{}: exists; -f replaces it", target.display()),
        )),
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

async fn get_file(
    sess: &mut Session,
    out: &mut Out,
    src: &[String],
    target: &Path,
    force: bool,
) -> Result<()> {
    let existed = check_local_target(target, force)?;
    let remote = sess.remote().await?;
    let t0 = Instant::now();
    let name = src.last().map_or("", String::as_str);
    let mut prog = Progress::new(out.json, &format!("get {name}"));
    let r = download_to(&remote, src, target, force && existed, &mut prog).await;
    prog.clear();
    let r = r.map_err(|mut e| {
        if e.kind == Kind::IsADirectory && e.message.ends_with(": is a directory") {
            e.message.push_str("; use get -r");
        }
        e
    });
    let bytes = sess.finish(remote, r).await?;
    transfer_line(
        out,
        &mut prog,
        false,
        &display_path(src),
        target,
        bytes,
        t0.elapsed(),
    );
    Ok(())
}

/// The stream path down into a `LocalTemp`, committed when complete.
async fn download_to(
    remote: &Remote,
    src: &[String],
    target: &Path,
    replace: bool,
    prog: &mut Progress,
) -> Result<u64> {
    let dir = target.parent().unwrap_or(Path::new("/")).to_path_buf();
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temp: Option<LocalTemp> = None;
    let r = {
        let mut sink = |p: Part| -> Result<()> {
            match p {
                Part::Start(size) => {
                    temp = Some(LocalTemp::create(&dir, &name)?);
                    prog.total(size, 0);
                }
                Part::Data(d) => {
                    if let Some(t) = temp.as_mut() {
                        t.write(d)?;
                    }
                    prog.add(d.len() as u64);
                }
            }
            Ok(())
        };
        remote.download(src, &mut sink).await
    };
    match (r, temp.take()) {
        (Ok(n), Some(t)) => t.commit(target, replace).map(|()| n),
        (Ok(_), None) => Err(Error::other("download without a start")),
        (Err(mut e), Some(mut t)) => {
            e.left.extend(t.discard());
            Err(e)
        }
        (Err(e), None) => Err(e),
    }
}

/// A small downloaded file, written in one go (runs in `spawn_blocking`).
fn write_small(target: &Path, data: &[u8], replace: bool) -> Result<()> {
    let dir = target.parent().unwrap_or(Path::new("/"));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut t = LocalTemp::create(dir, &name)?;
    t.write(data)?;
    t.commit(target, replace)
}

/// For each target: whether it exists now, and the index of another target that is the same
/// local file (a file system that folds case or normalises names, on a second run).
pub fn existing_targets(targets: &[PathBuf]) -> Vec<(bool, Option<usize>)> {
    let metas: Vec<Option<Metadata>> = targets
        .iter()
        .map(|t| std::fs::symlink_metadata(t).ok())
        .collect();
    let mut by_id: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (i, m) in metas.iter().enumerate() {
        if let Some(m) = m {
            by_id.entry((m.dev(), m.ino())).or_default().push(i);
        }
    }
    metas
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let other = m
                .as_ref()
                .and_then(|m| by_id[&(m.dev(), m.ino())].iter().copied().find(|&j| j != i));
            (m.is_some(), other)
        })
        .collect()
}

type DirErrors = Vec<(Vec<String>, Error)>;

/// Makes `root` and every directory of `dirs` (relative, parents first) below it, one component at
/// a time and never through a symbolic link. Returns the errors (relative path, error) and the
/// relative paths whose subtrees are left out. `root`'s own failure is the error.
pub fn prepare_dirs(root: &Path, dirs: &[Vec<String>]) -> Result<(DirErrors, Vec<Vec<String>>)> {
    fn one(p: &Path) -> Result<()> {
        match std::fs::symlink_metadata(p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(p).map_err(|e| io(p, e))
            }
            Err(e) => Err(io(p, e)),
            Ok(m) if m.file_type().is_symlink() => Err(Error::other(format!(
                "{}: a symbolic link in the destination; not followed",
                p.display()
            ))),
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(Error::new(
                Kind::NotADirectory,
                format!("{}: not a directory", p.display()),
            )),
        }
    }
    one(root)?;
    let mut errors = Vec::new();
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut sorted: Vec<&Vec<String>> = dirs.iter().collect();
    sorted.sort();
    for d in sorted {
        if out.iter().any(|o| d.starts_with(o)) {
            continue;
        }
        let mut p = root.to_path_buf();
        p.extend(d);
        if let Err(e) = one(&p) {
            errors.push((d.clone(), e));
            out.push(d.clone());
        }
    }
    Ok((errors, out))
}

enum Done {
    Ok(u64),
    Failed(Error),
    /// Grew past the small-file threshold since the walk: the stream path.
    Grew,
}

#[allow(clippy::too_many_arguments)]
async fn get_tree(
    remote: &Remote,
    out: &mut Out,
    src: &[String],
    root: &Path,
    force: bool,
    prog: &mut Progress,
    t0: Instant,
) -> Result<Option<()>> {
    // 1. The walk.
    let n0 = src.len();
    let mut dirs: Vec<Vec<String>> = Vec::new();
    let mut files: Vec<(Vec<String>, u64)> = Vec::new();
    let mut refused: Vec<Error> = Vec::new();
    let enter =
        |_: &[String], e: &Entry| local_component(&e.name).is_ok() && !is_temp_name(&e.name);
    let walked = {
        let mut on_dir = |d: walk::Dir| {
            let rel = d.path[n0..].to_vec();
            if !rel.is_empty() {
                dirs.push(rel.clone());
            }
            prog.listing(dirs.len() + 1);
            for e in &d.entries {
                let mut p = d.path.clone();
                p.push(e.name.clone());
                let shown = display_path(&p);
                if is_temp_name(&e.name) {
                    refused.push(temp_refused(&shown).at(&shown));
                } else if local_component(&e.name).is_err() {
                    refused.push(
                        Error::new(
                            Kind::InvalidName,
                            format!("{shown}: not a name a local file can have; not copied"),
                        )
                        .at(&shown),
                    );
                } else if !e.is_dir() {
                    let mut r = rel.clone();
                    r.push(e.name.clone());
                    files.push((r, e.size));
                }
            }
            true
        };
        let r = remote.clone();
        let list = move |d: Vec<String>| {
            let r = r.clone();
            async move { r.list(&d).await }
        };
        walk::walk(list, src, None, &enter, &mut on_dir).await
    };
    let report = match walked {
        Err(e) if e.kind == Kind::NotADirectory => return Ok(None),
        Err(e) => return Err(e),
        Ok(r) => r,
    };
    files.sort_by_key(|f| f.0.join("/"));
    let mut tally = Tally {
        files: 0,
        bytes: 0,
        failed: 0,
        directories: 0,
        total: files.len() + dirs.len(),
    };

    // 2. What the walk did not enter.
    let start = display_path(src);
    let mut errors = refused;
    for p in &report.denied {
        let s = display_path(p);
        errors.push(Error::new(Kind::AccessDenied, format!("{s}: access denied")).at(&s));
    }
    for (p, first) in &report.links {
        let s = display_path(p);
        let why = match first {
            Some(f) => format!("the same directory as {}", display_path(f)),
            None => format!("the same directory as a directory above {start}"),
        };
        errors.push(
            Error::other(format!(
                "{s}: not copied: {why} (a link, or a server that reuses file ids)"
            ))
            .at(&s),
        );
    }
    for p in &report.capped {
        let s = display_path(p);
        errors.push(
            Error::other(format!(
                "{s}: not copied: the server gives it no file id and the walk is past its limit"
            ))
            .at(&s),
        );
    }
    for (p, e) in &report.failed {
        errors.push(e.clone().at(display_path(p)));
    }
    tally.total += errors.len();
    for e in &errors {
        item(out, prog, &mut tally, e);
    }

    // 3. The local directories, never through a link.
    let (dir_errors, left_out) = prepare_dirs(root, &dirs)?;
    for (d, e) in dir_errors {
        let below_files = files.iter().filter(|(f, _)| f.starts_with(&d)).count();
        let below_dirs = dirs
            .iter()
            .filter(|x| x.starts_with(&d) && **x != d)
            .count();
        let mut remote_path = src.to_vec();
        remote_path.extend(d.iter().cloned());
        let mut e = e;
        e.message.push_str(&format!(
            "; left out: {below_files} files and {below_dirs} directories below it"
        ));
        let e = e.at(display_path(&remote_path));
        item(out, prog, &mut tally, &e);
        tally.failed += below_files + below_dirs;
    }
    let inside = |rel: &[String]| !left_out.iter().any(|o| rel.starts_with(o));
    tally.directories = 1 + dirs.iter().filter(|d| inside(d)).count();
    files.retain(|(f, _)| inside(f));

    // 4. What exists, noted before anything moves.
    let targets: Vec<PathBuf> = files
        .iter()
        .map(|(rel, _)| {
            let mut p = root.to_path_buf();
            p.extend(rel);
            p
        })
        .collect();
    let existing = existing_targets(&targets);
    let remote_of = |rel: &[String]| {
        let mut p = src.to_vec();
        p.extend(rel.iter().cloned());
        p
    };
    let mut todo: Vec<(Vec<String>, PathBuf, u64, bool)> = Vec::new();
    for (i, (rel, size)) in files.iter().enumerate() {
        let rp = display_path(&remote_of(rel));
        let (existed, same) = existing[i];
        let refusal = if let Some(j) = same {
            Some(Error::new(
                Kind::InvalidName,
                format!(
                    "{rp}: the local file system takes it for the same name as {}",
                    display_path(&remote_of(&files[j].0))
                ),
            ))
        } else {
            check_local_target(&targets[i], force).err()
        };
        match refusal {
            Some(e) => item(out, prog, &mut tally, &e.at(&rp)),
            None => todo.push((rel.clone(), targets[i].clone(), *size, force && existed)),
        }
    }
    let total_bytes: u64 = todo.iter().map(|t| t.2).sum();
    prog.total(total_bytes, todo.len());

    // 5. Small files, up to L at once.
    let mut fatal_error: Option<Error> = None;
    let (small, mut large): (Vec<_>, Vec<_>) = todo.into_iter().partition(|t| t.2 <= remote.small);
    let mut queue = small.into_iter();
    let mut set: JoinSet<(Vec<String>, PathBuf, u64, bool, Done)> = JoinSet::new();
    loop {
        while fatal_error.is_none() && set.len() < L {
            let Some((rel, target, size, replace)) = queue.next() else {
                break;
            };
            let (r, path) = (remote.clone(), remote_of(&rel));
            set.spawn(async move {
                let alive = r.alive();
                let done = match r.read_small(&path, size).await {
                    Ok(None) => Done::Grew,
                    Ok(Some(data)) => {
                        let t = target.clone();
                        let n = data.len() as u64;
                        // The Alive makes the cleanup after an interrupt wait for this write.
                        let w = tokio::task::spawn_blocking(move || {
                            let _alive = alive;
                            write_small(&t, &data, replace)
                        })
                        .await;
                        match w {
                            Ok(Ok(())) => Done::Ok(n),
                            Ok(Err(e)) => Done::Failed(e),
                            Err(e) => Done::Failed(Error::other(format!("local write: {e}"))),
                        }
                    }
                    Err(e) => Done::Failed(e),
                };
                (rel, target, size, replace, done)
            });
        }
        let Some(j) = set.join_next().await else {
            break;
        };
        let (rel, target, size, replace, done) =
            j.map_err(|e| Error::other(format!("transfer task: {e}")))?;
        let rp = display_path(&remote_of(&rel));
        match done {
            Done::Ok(n) => {
                tally.files += 1;
                tally.bytes += n;
                prog.add(n);
                prog.file_done();
                transfer_line(out, prog, false, &rp, &target, n, t0.elapsed());
            }
            Done::Grew => large.push((rel, target, size, replace)),
            Done::Failed(e) if fatal(&e) => {
                fatal_error.get_or_insert(e);
            }
            Done::Failed(e) => item(out, prog, &mut tally, &e.at(&rp)),
        }
    }

    // 6. Large files, one after another.
    if fatal_error.is_none() {
        for (rel, target, _, replace) in large {
            let rp = display_path(&remote_of(&rel));
            let t1 = Instant::now();
            match download_to(remote, &remote_of(&rel), &target, replace, prog).await {
                Ok(n) => {
                    tally.files += 1;
                    tally.bytes += n;
                    prog.file_done();
                    transfer_line(out, prog, false, &rp, &target, n, t1.elapsed());
                }
                Err(e) if fatal(&e) => {
                    fatal_error = Some(e);
                    break;
                }
                Err(e) => item(out, prog, &mut tally, &e.at(&rp)),
            }
        }
    }

    // 7. The summary, then the command's error.
    finish_tree(out, prog, false, &start, tally, fatal_error, t0).map(Some)
}

fn finish_tree(
    out: &mut Out,
    prog: &mut Progress,
    put: bool,
    start: &str,
    mut tally: Tally,
    fatal_error: Option<Error>,
    t0: Instant,
) -> Result<()> {
    if fatal_error.is_some() {
        // `total` leaves out the root directory, which `directories` counts as done.
        tally.failed = (tally.total + 1).saturating_sub(tally.files + tally.directories);
    }
    summary(out, prog, put, &tally, t0.elapsed());
    if let Some(e) = fatal_error {
        return Err(e);
    }
    if tally.failed > 0 {
        let cmd = if put { "put -r" } else { "get -r" };
        return Err(Error::other(format!(
            "{cmd} {start}: {} of {} not transferred",
            tally.failed,
            tally.total.max(tally.failed)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// cat

/// `cat <file>`: the stream path to stdout; with `--json` one `content` object.
pub async fn cat(sess: &mut Session, out: &mut Out, arg: &str) -> Result<()> {
    no_share(sess)?;
    let src = sess.resolve(arg);
    let what = display_path(&src);
    if src.is_empty() {
        return Err(Error::new(
            Kind::IsADirectory,
            format!("{what}: is a directory"),
        ));
    }
    let json = out.json;
    let mut text = Vec::new();
    let remote = sess.remote().await?;
    let r = {
        let mut sink = |p: Part| -> Result<()> {
            match p {
                Part::Start(size) if json && size > CAT_JSON_MAX => Err(Error::other(format!(
                    "{what}: {size} bytes; cat --json prints at most 1 MiB of UTF-8 text; use get"
                ))),
                Part::Start(_) => Ok(()),
                Part::Data(d) if json => {
                    text.extend_from_slice(d);
                    Ok(())
                }
                Part::Data(d) => out.raw(d),
            }
        };
        remote.download(&src, &mut sink).await
    };
    let size = sess.finish(remote, r).await?;
    if json {
        let text = String::from_utf8(text)
            .map_err(|_| Error::other(format!("{what}: not UTF-8 text; use get")))?;
        out.obj(&json!({"type": "content", "path": what, "size": size, "text": text}));
    }
    out.flush();
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// put

/// `put [-r] [-f] <local> [<remote>]`
pub async fn put(
    sess: &mut Session,
    out: &mut Out,
    flags: &[String],
    args: &[String],
) -> Result<()> {
    no_share(sess)?;
    let (force, recursive) = (has(flags, "-f"), has(flags, "-r"));
    let local = expand(&args[0]);
    let meta = std::fs::metadata(&local).map_err(|e| io(&local, e))?;
    if meta.is_dir() && !recursive {
        return Err(Error::new(
            Kind::IsADirectory,
            format!("{}: is a directory; use put -r", local.display()),
        ));
    }
    if !meta.is_dir() && !meta.is_file() {
        return Err(Error::new(
            Kind::LocalIo,
            format!("{}: not a regular file", local.display()),
        ));
    }
    let canon = std::fs::canonicalize(&local).map_err(|e| io(&local, e))?;
    // The name the user gave (a link keeps its own); `.`, `..` and `/` have none and are resolved.
    let base = local
        .file_name()
        .or_else(|| canon.file_name())
        .ok_or_else(|| Error::usage(format!("put {}: has no name", local.display())))?
        .to_str()
        .ok_or_else(|| {
            Error::new(
                Kind::InvalidName,
                format!("{}: the name is not UTF-8", local.display()),
            )
        })?
        .to_string();
    if is_temp_name(&base) {
        return Err(temp_refused(&shown(&local)));
    }
    if !valid_name(&base) {
        return Err(Error::new(
            Kind::InvalidName,
            format!(
                "{}: '{base}' has a character SMB does not allow",
                local.display()
            ),
        ));
    }
    let target = remote_target(
        sess.cwd(),
        args.get(1).map(String::as_str),
        &base,
        recursive,
    );
    let Some(name) = target.last().cloned() else {
        return Err(Error::usage("put: the target is the share root"));
    };
    if is_temp_name(&name) {
        return Err(temp_refused(&display_path(&target)));
    }
    let local = std::path::absolute(&local).map_err(|e| io(&local, e))?;
    if recursive && meta.is_dir() {
        return put_r(sess, out, &canon, target, force).await;
    }
    sess.invalidate(&target[..target.len() - 1], false);
    let remote = sess.remote().await?;
    let t0 = Instant::now();
    let mut prog = Progress::new(out.json, &format!("put {base}"));
    let r = upload_file(&remote, &local, &target, force, &mut prog).await;
    prog.clear();
    let bytes = sess.finish(remote, r).await?;
    transfer_line(
        out,
        &mut prog,
        true,
        &display_path(&target),
        &local,
        bytes,
        t0.elapsed(),
    );
    Ok(())
}

/// Fills `buf` as far as the file goes.
fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn stamp(m: &Metadata) -> (u64, Option<SystemTime>) {
    (m.len(), m.modified().ok())
}

fn changed(local: &Path) -> Error {
    Error::other(format!("{}: changed while it was read", local.display()))
}

/// Reads a whole small file, checking that it did not change meanwhile (runs in a blocking
/// context or on the main thread). None: it has grown past `limit` (the stream path takes it).
fn read_small_local(local: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut f = File::open(local).map_err(|e| io(local, e))?;
    let before = stamp(&f.metadata().map_err(|e| io(local, e))?);
    if before.0 > limit {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(before.0 as usize);
    f.read_to_end(&mut data).map_err(|e| io(local, e))?;
    let after = stamp(&f.metadata().map_err(|e| io(local, e))?);
    if after != before || data.len() as u64 != before.0 {
        return Err(changed(local));
    }
    Ok(Some(data))
}

/// One file up: the small path if it fits, else the stream path, whose source checks size and
/// modification time at the end.
async fn upload_file(
    remote: &Remote,
    local: &Path,
    target: &[String],
    force: bool,
    prog: &mut Progress,
) -> Result<u64> {
    let (dir, name) = target.split_at(target.len() - 1);
    let name = &name[0];
    let temp = temp_name(name);
    let mut f = File::open(local).map_err(|e| io(local, e))?;
    let before = stamp(&f.metadata().map_err(|e| io(local, e))?);
    prog.total(before.0, 0);
    if before.0 <= remote.small
        && let Some(data) = read_small_local(local, remote.small)?
    {
        let n = data.len() as u64;
        remote.put_small(dir, &temp, name, &data, force).await?;
        prog.add(n);
        return Ok(n);
    }
    let mut read = 0u64;
    let mut source = |buf: &mut [u8]| -> Result<usize> {
        let n = read_full(&mut f, buf).map_err(|e| io(local, e))?;
        read += n as u64;
        prog.add(n as u64);
        if read > before.0 {
            return Err(changed(local));
        }
        if n == 0 {
            let after = stamp(&f.metadata().map_err(|e| io(local, e))?);
            if read != before.0 || after != before {
                return Err(changed(local));
            }
        }
        Ok(n)
    };
    remote.upload(dir, &temp, name, force, &mut source).await
}

/// Names in one directory that are equal after `to_lowercase()`: for each, another one it
/// collides with.
pub fn case_collisions(names: &[&str]) -> Vec<Option<usize>> {
    let mut by: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        by.entry(n.to_lowercase()).or_default().push(i);
    }
    names
        .iter()
        .enumerate()
        .map(|(i, n)| by[&n.to_lowercase()].iter().copied().find(|&j| j != i))
        .collect()
}

/// What the local walk of `put -r` found: directories and files relative to the root, sorted by
/// path, and one error per item it will not upload.
#[derive(Default)]
pub struct LocalTree {
    pub dirs: Vec<Vec<String>>,
    pub files: Vec<(Vec<String>, PathBuf, u64)>,
    pub errors: Vec<Error>,
}

/// Regular files below `p`, not following links (for the message of a directory left out).
fn count_files(p: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(t) if t.is_file() => 1,
            _ => 0,
        })
        .sum()
}

/// Walks a local tree for `put -r`: directories are entered; regular files collected; links and
/// special files are neither followed nor read. `seen` holds (device, inode) of the directories
/// entered, so a bind mount or a looping file system is cut at the loop.
pub fn local_walk(root: &Path, seen: &mut HashSet<(u64, u64)>) -> LocalTree {
    let mut t = LocalTree::default();
    walk_local(root, Vec::new(), seen, &mut t);
    t.dirs.sort_by_key(|d| d.join("/"));
    t.files.sort_by_key(|f| f.0.join("/"));
    t
}

fn walk_local(abs: &Path, rel: Vec<String>, seen: &mut HashSet<(u64, u64)>, t: &mut LocalTree) {
    let s = shown(abs);
    let m = match std::fs::metadata(abs) {
        Ok(m) => m,
        Err(e) => return t.errors.push(io(abs, e).at(&s)),
    };
    if !seen.insert((m.dev(), m.ino())) {
        return t.errors.push(
            Error::other(format!(
                "{s}: not uploaded: a directory already entered (a bind mount or a file system \
                 that loops)"
            ))
            .at(&s),
        );
    }
    let entries: std::io::Result<Vec<std::fs::DirEntry>> =
        std::fs::read_dir(abs).and_then(|rd| rd.collect());
    let mut entries = match entries {
        Ok(e) => e,
        Err(e) => {
            return t.errors.push(
                Error::new(
                    Kind::LocalIo,
                    format!("{s}: cannot be read ({e}); not uploaded, with everything below it"),
                )
                .at(&s),
            );
        }
    };
    if !rel.is_empty() {
        t.dirs.push(rel.clone());
    }
    entries.sort_by_key(|e| e.file_name());
    let names: Vec<Option<String>> = entries
        .iter()
        .map(|e| e.file_name().into_string().ok())
        .collect();
    let utf8: Vec<&str> = names.iter().map(|n| n.as_deref().unwrap_or("")).collect();
    let folds = case_collisions(&utf8);
    for (i, e) in entries.iter().enumerate() {
        let p = e.path();
        let ps = shown(&p);
        let ft = match std::fs::symlink_metadata(&p) {
            Ok(m) => m.file_type(),
            Err(err) => {
                t.errors.push(io(&p, err).at(&ps));
                continue;
            }
        };
        let held = |why: String| -> Error {
            if ft.is_dir() {
                let n = count_files(&p);
                Error::new(
                    Kind::InvalidName,
                    format!("{why} ({n} files below it not uploaded)"),
                )
            } else {
                Error::new(Kind::InvalidName, why)
            }
        };
        let refusal = match &names[i] {
            None => Some(held(format!("{ps}: not uploaded: the name is not UTF-8"))),
            Some(n) if is_temp_name(n) => Some(temp_refused(&ps)),
            Some(n) if !valid_name(n) => Some(held(format!(
                "{ps}: not uploaded: '{n}' has a character SMB does not allow"
            ))),
            Some(_) if folds[i].is_some() => {
                let other = shown(&entries[folds[i].unwrap_or(i)].path());
                Some(held(format!(
                    "{ps}: not uploaded: differs only in letter case from {other}"
                )))
            }
            Some(_) if ft.is_symlink() => {
                Some(Error::other(format!("{ps}: not uploaded: a symbolic link")))
            }
            Some(_) if !ft.is_dir() && !ft.is_file() => {
                Some(Error::other(format!("{ps}: not uploaded: a special file")))
            }
            Some(_) => None,
        };
        if let Some(err) = refusal {
            t.errors.push(err.at(&ps));
            continue;
        }
        let name = names[i].clone().unwrap_or_default();
        let mut r = rel.clone();
        r.push(name);
        if ft.is_dir() {
            walk_local(&p, r, seen, t);
        } else {
            let size = std::fs::symlink_metadata(&p).map_or(0, |m| m.len());
            t.files.push((r, p, size));
        }
    }
}

async fn put_r(
    sess: &mut Session,
    out: &mut Out,
    local_root: &Path,
    target_root: Vec<String>,
    force: bool,
) -> Result<()> {
    // A root that cannot be read is the command's error, before the server is touched.
    std::fs::read_dir(local_root).map_err(|e| io(local_root, e))?;
    let into = target_root[..target_root.len() - 1].to_vec();
    sess.invalidate(&into, false);
    sess.invalidate(&target_root, true);
    let lr = local_root.to_path_buf();
    let tree = tokio::task::spawn_blocking(move || local_walk(&lr, &mut HashSet::new()))
        .await
        .map_err(|e| Error::other(format!("local walk: {e}")))?;
    let remote = sess.remote().await?;
    let t0 = Instant::now();
    let mut prog = Progress::new(out.json, &format!("put {}", local_root.display()));
    let r = put_tree(
        &remote,
        out,
        tree,
        local_root,
        &target_root,
        force,
        &mut prog,
        t0,
    )
    .await;
    prog.clear();
    sess.finish(remote, r).await
}

#[allow(clippy::too_many_arguments)]
async fn put_tree(
    remote: &Remote,
    out: &mut Out,
    tree: LocalTree,
    local_root: &Path,
    target_root: &[String],
    force: bool,
    prog: &mut Progress,
    t0: Instant,
) -> Result<()> {
    let start = shown(local_root);
    let mut tally = Tally {
        files: 0,
        bytes: 0,
        failed: 0,
        directories: 0,
        total: tree.files.len() + tree.dirs.len() + tree.errors.len(),
    };
    for e in &tree.errors {
        item(out, prog, &mut tally, e);
    }
    let remote_of = |rel: &[String]| {
        let mut p = target_root.to_vec();
        p.extend(rel.iter().cloned());
        p
    };
    let local_of = |rel: &[String]| {
        let mut p = local_root.to_path_buf();
        p.extend(rel);
        p
    };

    // 3. Remote directories: the root alone (its error is the command's), then level by level.
    remote.mkdir(target_root, true).await?;
    tally.directories = 1;
    let mut left_out: Vec<Vec<String>> = Vec::new();
    let mut fatal_error: Option<Error> = None;
    let deepest = tree.dirs.iter().map(Vec::len).max().unwrap_or(0);
    for level in 1..=deepest {
        let mut queue = tree
            .dirs
            .iter()
            .filter(|d| d.len() == level && !left_out.iter().any(|o| d.starts_with(o)))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter();
        let mut set: JoinSet<(Vec<String>, Result<()>)> = JoinSet::new();
        loop {
            while fatal_error.is_none() && set.len() < L {
                let Some(d) = queue.next() else { break };
                let (r, path) = (remote.clone(), remote_of(&d));
                set.spawn(async move { (d, r.mkdir(&path, true).await) });
            }
            let Some(j) = set.join_next().await else {
                break;
            };
            let (d, r) = j.map_err(|e| Error::other(format!("transfer task: {e}")))?;
            match r {
                Ok(()) => tally.directories += 1,
                Err(e) if fatal(&e) => {
                    fatal_error.get_or_insert(e);
                }
                Err(mut e) => {
                    let below_files = tree.files.iter().filter(|f| f.0.starts_with(&d)).count();
                    let below_dirs = tree
                        .dirs
                        .iter()
                        .filter(|x| x.starts_with(&d) && **x != d)
                        .count();
                    e.message.push_str(&format!(
                        "; left out: {below_files} files and {below_dirs} directories below it"
                    ));
                    item(out, prog, &mut tally, &e.at(shown(&local_of(&d))));
                    tally.failed += below_files + below_dirs;
                    left_out.push(d);
                }
            }
        }
        if fatal_error.is_some() {
            break;
        }
    }
    let files: Vec<(Vec<String>, PathBuf, u64)> = tree
        .files
        .into_iter()
        .filter(|f| !left_out.iter().any(|o| f.0.starts_with(o)))
        .collect();
    prog.total(files.iter().map(|f| f.2).sum(), files.len());

    // 4. Small files, up to L at once.
    let (small, mut large): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| f.2 <= remote.small);
    let mut queue = if fatal_error.is_none() {
        small.into_iter()
    } else {
        Vec::new().into_iter()
    };
    let mut set: JoinSet<(Vec<String>, PathBuf, u64, Done)> = JoinSet::new();
    loop {
        while fatal_error.is_none() && set.len() < L {
            let Some((rel, path, size)) = queue.next() else {
                break;
            };
            let (r, target) = (remote.clone(), remote_of(&rel));
            set.spawn(async move {
                let limit = r.small;
                let p = path.clone();
                let read = tokio::task::spawn_blocking(move || read_small_local(&p, limit)).await;
                let done = match read {
                    Err(e) => Done::Failed(Error::other(format!("local read: {e}"))),
                    Ok(Err(e)) => Done::Failed(e),
                    Ok(Ok(None)) => Done::Grew,
                    Ok(Ok(Some(data))) => {
                        let (dir, name) = target.split_at(target.len() - 1);
                        let temp = temp_name(&name[0]);
                        match r.put_small(dir, &temp, &name[0], &data, force).await {
                            Ok(()) => Done::Ok(data.len() as u64),
                            Err(e) => Done::Failed(e),
                        }
                    }
                };
                (rel, path, size, done)
            });
        }
        let Some(j) = set.join_next().await else {
            break;
        };
        let (rel, path, size, done) = j.map_err(|e| Error::other(format!("transfer task: {e}")))?;
        match done {
            Done::Ok(n) => {
                tally.files += 1;
                tally.bytes += n;
                prog.add(n);
                prog.file_done();
                transfer_line(
                    out,
                    prog,
                    true,
                    &display_path(&remote_of(&rel)),
                    &path,
                    n,
                    t0.elapsed(),
                );
            }
            Done::Grew => large.push((rel, path, size)),
            Done::Failed(e) if fatal(&e) => {
                fatal_error.get_or_insert(e);
            }
            Done::Failed(e) => item(out, prog, &mut tally, &e.at(shown(&path))),
        }
    }

    // 5. Large files, one after another.
    if fatal_error.is_none() {
        for (rel, path, _) in large {
            let t1 = Instant::now();
            let target = remote_of(&rel);
            match upload_file(remote, &path, &target, force, prog).await {
                Ok(n) => {
                    tally.files += 1;
                    tally.bytes += n;
                    prog.file_done();
                    transfer_line(
                        out,
                        prog,
                        true,
                        &display_path(&target),
                        &path,
                        n,
                        t1.elapsed(),
                    );
                }
                Err(e) if fatal(&e) => {
                    fatal_error = Some(e);
                    break;
                }
                Err(e) => item(out, prog, &mut tally, &e.at(shown(&path))),
            }
        }
    }
    finish_tree(out, prog, true, &start, tally, fatal_error, t0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// An empty directory of its own below target/unit-tmp.
    fn scratch(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-tmp")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fnv_vectors() {
        assert_eq!(format!("{:016x}", fnv1a(b"")), "cbf29ce484222325");
        assert_eq!(format!("{:016x}", fnv1a(b"a")), "af63dc4c8601ec8c");
    }

    #[test]
    fn temp_names() {
        let t = temp_name("a.txt");
        assert_eq!(t.len(), 27);
        assert!(t.starts_with(".rsmb-") && t.ends_with(".part"), "{t}");
        assert!(
            t[6..22]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(t, temp_name("a.txt"));
        assert_ne!(t, temp_name("A.txt"));
        assert_eq!(temp_name(&"x".repeat(250)).len(), 27);
        assert_eq!(temp_name(&"y".repeat(255)).len(), 27);
        for n in ["a.txt", "", "東京", &"x".repeat(250)] {
            assert!(is_temp_name(&temp_name(n)), "{n}");
        }
        for not in [
            ".rsmb-0123456789abcdef.par",
            ".rsmb-0123456789ABCDEF.part",
            "x.rsmb-0123456789abcdef.part",
            ".rsmb-0123456789abcde.part",
            ".rsmb-0123456789abcdef0.part",
            ".rsmb-0123456789abcdeg.part",
            ".rsmb-0123456789abcdé.part",
        ] {
            assert!(!is_temp_name(not), "{not}");
        }
        assert!(is_temp_name(".rsmb-0123456789abcdef.part"));
    }

    fn names(var: &str) -> Vec<String> {
        let o = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {}/testenv/samba/names.sh; printf '%s\\n' \"${var}\"",
                env!("CARGO_MANIFEST_DIR")
            ))
            .output()
            .unwrap();
        String::from_utf8(o.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    #[test]
    fn local_components() {
        for bad in ["", ".", "..", "a/b", "/abs", "a\\b", "nu\0l"] {
            assert_eq!(
                local_component(bad).unwrap_err().kind,
                Kind::InvalidName,
                "{bad:?}"
            );
        }
        let long = "n".repeat(250);
        let mut good = vec![
            "...",
            ".hidden",
            "a..b",
            "  two leading spaces.txt",
            long.as_str(),
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        good.extend(names("CJK_NAMES"));
        good.extend(names("SPECIAL_NAMES"));
        assert!(good.len() > 25);
        for n in &good {
            assert_eq!(local_component(n).unwrap(), n);
        }
    }

    #[test]
    fn local_temp_lifecycle() {
        let d = scratch("lifecycle");
        let target = d.join("f.txt");
        let tp = d.join(temp_name("f.txt"));
        // create, a second create refused, drop removes
        let mut a = LocalTemp::create(&d, "f.txt").unwrap();
        assert!(tp.exists());
        a.write(b"first").unwrap();
        let e = LocalTemp::create(&d, "f.txt").err().unwrap();
        assert!(e.message.contains("another transfer"), "{}", e.message);
        assert_eq!(std::fs::read(&tp).unwrap(), b"first", "the first is intact");
        drop(a);
        assert!(!tp.exists(), "dropping removes it");
        // commit without replace onto an existing final
        std::fs::write(&target, b"old").unwrap();
        let mut t = LocalTemp::create(&d, "f.txt").unwrap();
        t.write(b"new").unwrap();
        let e = t.commit(&target, false).unwrap_err();
        assert_eq!(e.kind, Kind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!tp.exists(), "the temporary file is gone");
        // with replace
        let mut t = LocalTemp::create(&d, "f.txt").unwrap();
        t.write(b"new").unwrap();
        t.commit(&target, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!tp.exists());
        // a stale temporary file with old content is reused
        std::fs::write(&tp, b"stale stale stale").unwrap();
        let mut t = LocalTemp::create(&d, "g.txt").unwrap();
        drop(t);
        let stale = d.join(temp_name("h.txt"));
        std::fs::write(&stale, b"stale stale stale").unwrap();
        t = LocalTemp::create(&d, "h.txt").unwrap();
        t.write(b"xy").unwrap();
        t.commit(&d.join("h.txt"), false).unwrap();
        assert_eq!(std::fs::read(d.join("h.txt")).unwrap(), b"xy");
        let _ = std::fs::remove_file(&tp);
        // a symbolic link under the temporary name
        let outside = d.join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, d.join(temp_name("l.txt"))).unwrap();
        assert!(LocalTemp::create(&d, "l.txt").is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        // replace onto a directory
        std::fs::create_dir(d.join("dir")).unwrap();
        let mut t = LocalTemp::create(&d, "dir").unwrap();
        t.write(b"x").unwrap();
        assert_eq!(
            t.commit(&d.join("dir"), true).unwrap_err().kind,
            Kind::IsADirectory
        );
        assert!(d.join("dir").is_dir());
        // a temporary name that is a second hard link of a complete file (finding 5)
        let done = d.join("done.bin");
        std::fs::write(&done, b"complete content").unwrap();
        let dtemp = d.join(temp_name("done.bin"));
        std::fs::hard_link(&done, &dtemp).unwrap();
        let t = LocalTemp::create(&d, "done.bin").unwrap();
        assert_eq!(
            std::fs::read(&done).unwrap(),
            b"complete content",
            "not truncated"
        );
        let (a, b) = (
            std::fs::metadata(&done).unwrap(),
            std::fs::metadata(&dtemp).unwrap(),
        );
        assert_ne!(a.ino(), b.ino(), "a new inode");
        assert_eq!(b.nlink(), 1);
        assert_eq!(a.nlink(), 1);
        drop(t);
        // the final name created after create: not replaced (P17)
        let mut t = LocalTemp::create(&d, "late.txt").unwrap();
        t.write(b"mine").unwrap();
        std::fs::write(d.join("late.txt"), b"theirs").unwrap();
        assert_eq!(
            t.commit(&d.join("late.txt"), false).unwrap_err().kind,
            Kind::AlreadyExists
        );
        assert_eq!(std::fs::read(d.join("late.txt")).unwrap(), b"theirs");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A file that is already under the temporary name is never adopted, whoever made it and
    /// whatever its mode: the download is written to an inode this call created (final review,
    /// finding 5). A live transfer still holds its lock and is refused, not unlinked.
    #[test]
    fn local_temp_is_never_adopted() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("adopt");
        let reference = d.join("reference");
        std::fs::write(&reference, b"").unwrap();
        let default_mode = std::fs::metadata(&reference).unwrap().permissions().mode();
        let planted = d.join(temp_name("f.bin"));
        std::fs::write(&planted, b"planted").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o666)).unwrap();
        let before = std::fs::metadata(&planted).unwrap();
        // Open, so the number is not given out again when the name is unlinked.
        let held = File::open(&planted).unwrap();
        let mut t = LocalTemp::create(&d, "f.bin").unwrap();
        t.write(b"data").unwrap();
        t.commit(&d.join("f.bin"), false).unwrap();
        let after = std::fs::metadata(d.join("f.bin")).unwrap();
        assert_ne!(
            after.ino(),
            before.ino(),
            "the planted inode became the file"
        );
        assert_eq!(after.permissions().mode(), default_mode);
        drop(held);
        assert_eq!(std::fs::read(d.join("f.bin")).unwrap(), b"data");
        // a live transfer holds the lock: refused, and its file is still there
        let live = LocalTemp::create(&d, "g.bin").unwrap();
        let ino = std::fs::metadata(d.join(temp_name("g.bin"))).unwrap().ino();
        assert!(LocalTemp::create(&d, "g.bin").is_err());
        assert_eq!(
            std::fs::metadata(d.join(temp_name("g.bin"))).unwrap().ino(),
            ino
        );
        drop(live);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn target_rules() {
        let d = scratch("targets");
        std::fs::create_dir(d.join("sub")).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let s = |p: &Path| p.to_path_buf();
        let ds = d.display().to_string();
        assert_eq!(
            local_target(None, "f.bin", false).unwrap(),
            cwd.join("f.bin")
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/sub")), "f.bin", false).unwrap(),
            s(&d.join("sub/f.bin"))
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/sub/")), "f.bin", false).unwrap(),
            s(&d.join("sub/f.bin"))
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/new.bin")), "f.bin", false).unwrap(),
            s(&d.join("new.bin"))
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/nodir/x.bin")), "f.bin", false)
                .unwrap_err()
                .kind,
            Kind::LocalIo
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/missing/")), "f.bin", false)
                .unwrap_err()
                .kind,
            Kind::LocalIo
        );
        assert_eq!(
            local_target(Some(&ds), "tree", true).unwrap(),
            s(&d.join("tree")),
            "-r into an existing directory"
        );
        assert_eq!(
            local_target(Some(&format!("{ds}/missing")), "tree", true)
                .unwrap_err()
                .kind,
            Kind::LocalIo,
            "-r needs an existing directory"
        );
        assert_eq!(
            local_target(None, "share", true).unwrap(),
            cwd.join("share"),
            "the share root"
        );
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            assert_eq!(expand("~"), PathBuf::from(&home));
            assert_eq!(expand("~/x/y"), PathBuf::from(&home).join("x/y"));
        }
        assert_eq!(expand("~x"), PathBuf::from("~x"));
        let cwd = v(&["w"]);
        assert_eq!(remote_target(&cwd, None, "f", false), v(&["w", "f"]));
        assert_eq!(
            remote_target(&cwd, Some("d/"), "f", false),
            v(&["w", "d", "f"])
        );
        assert_eq!(
            remote_target(&cwd, Some("d\\"), "f", false),
            v(&["w", "d", "f"])
        );
        assert_eq!(
            remote_target(&cwd, Some("/x/n.bin"), "f", false),
            v(&["x", "n.bin"])
        );
        assert_eq!(
            remote_target(&cwd, Some("d"), "f", false),
            v(&["w", "d"]),
            "a file path"
        );
        assert_eq!(
            remote_target(&cwd, Some("d"), "t", true),
            v(&["w", "d", "t"]),
            "-r: into"
        );
        assert_eq!(remote_target(&cwd, None, "t", true), v(&["w", "t"]));
        assert_eq!(remote_target(&cwd, Some("/"), "f", false), v(&["f"]));
        assert!(remote_target(&[], Some(".."), "f", false).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn case_collisions() {
        let nfc = "caf\u{e9}.txt";
        let nfd = "cafe\u{301}.txt";
        let ko_nfc = "\u{d55c}\u{ae00}";
        let ko_nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        let names = ["Case.txt", "case.txt", nfc, nfd, ko_nfc, ko_nfd, "other"];
        let c = super::case_collisions(&names);
        assert_eq!(c[0], Some(1));
        assert_eq!(c[1], Some(0));
        assert!(c[2..].iter().all(Option::is_none), "{c:?}");
    }

    #[test]
    fn progress_format() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1024 * 1024 - 1), "1024.0 KiB");
        assert_eq!(human(1024 * 1024), "1.0 MiB");
        assert_eq!(human(52_432_897), "50.0 MiB");
        assert_eq!(human(1 << 30), "1.0 GiB");
        assert_eq!(human(4_617_089_843), "4.3 GiB");
        let l = render(
            "get medium.bin",
            None,
            19_398_656,
            52_432_897,
            Duration::from_millis(1076),
        );
        assert_eq!(
            l,
            "get medium.bin  36%  18.5 MiB / 50.0 MiB  17.2 MiB/s  eta 2 s"
        );
        let l = render(
            "get /smallfiles",
            Some((120, 341)),
            512,
            1024,
            Duration::from_secs(1),
        );
        assert_eq!(
            l,
            "get /smallfiles  120/341 files  50%  512 B / 1.0 KiB  512 B/s  eta 1 s"
        );
        let l = render("get b", None, 3 << 30, 3 << 30, Duration::from_secs(2));
        assert!(l.contains("1536.0 MiB/s"), "{l}");
        let l = render("put x", None, 0, 0, Duration::ZERO);
        assert!(l.contains("100%"), "{l}");
    }

    #[test]
    fn local_walk_rules() {
        let d = scratch("localwalk");
        let r = d.join("root");
        std::fs::create_dir_all(r.join("b/sub")).unwrap();
        std::fs::create_dir_all(r.join("seen/x")).unwrap();
        std::fs::create_dir_all(r.join("locked/in")).unwrap();
        std::fs::write(r.join("locked/in/f"), b"x").unwrap();
        std::fs::write(r.join("a.txt"), b"a").unwrap();
        std::fs::write(r.join("b/sub/c.txt"), b"c").unwrap();
        std::fs::write(r.join("seen/x/y.txt"), b"y").unwrap();
        std::fs::write(r.join(temp_name("a.txt")), b"partial").unwrap();
        std::os::unix::fs::symlink(r.join("a.txt"), r.join("link-file")).unwrap();
        std::os::unix::fs::symlink(r.join("b"), r.join("link-dir")).unwrap();
        std::fs::set_permissions(r.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut seen = HashSet::new();
        let m = std::fs::metadata(r.join("seen")).unwrap();
        seen.insert((m.dev(), m.ino()));
        let t = local_walk(&r, &mut seen);
        std::fs::set_permissions(r.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let rels: Vec<String> = t.files.iter().map(|f| f.0.join("/")).collect();
        assert_eq!(
            rels,
            ["a.txt", "b/sub/c.txt"],
            "sorted, nothing behind links or locked"
        );
        let dirs: Vec<String> = t.dirs.iter().map(|d| d.join("/")).collect();
        assert_eq!(
            dirs,
            ["b", "b/sub"],
            "locked and seen are not directories to create"
        );
        let msgs: Vec<&str> = t.errors.iter().map(|e| e.message.as_str()).collect();
        let has = |what: &str| msgs.iter().filter(|m| m.contains(what)).count();
        assert_eq!(has("a symbolic link"), 2, "{msgs:?}");
        assert_eq!(has("a temporary file of rsmb"), 1, "{msgs:?}");
        assert_eq!(has("cannot be read"), 1, "{msgs:?}");
        assert_eq!(has("already entered"), 1, "{msgs:?}");
        assert_eq!(t.errors.len(), 5, "{msgs:?}");
        assert!(t.errors.iter().all(|e| e.path.is_some()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn local_dirs_never_through_links() {
        let d = scratch("prepdirs");
        let root = d.join("tree");
        let outside = d.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();
        std::fs::write(root.join("file"), b"x").unwrap();
        let dirs = vec![
            v(&["sub"]),
            v(&["sub", "deeper"]),
            v(&["file"]),
            v(&["file", "x"]),
            v(&["real"]),
            v(&["new"]),
            v(&["new", "two"]),
        ];
        let (errors, left_out) = prepare_dirs(&root, &dirs).unwrap();
        assert_eq!(left_out, [v(&["file"]), v(&["sub"])]);
        let kinds: Vec<(Vec<String>, Kind)> =
            errors.iter().map(|(p, e)| (p.clone(), e.kind)).collect();
        assert_eq!(
            kinds,
            [
                (v(&["file"]), Kind::NotADirectory),
                (v(&["sub"]), Kind::Other)
            ]
        );
        assert!(errors[1].1.message.contains("symbolic link"));
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "nothing behind the link"
        );
        assert!(root.join("new/two").is_dir() && root.join("real").is_dir());
        // the root itself
        let e = prepare_dirs(&root.join("file"), &[]).unwrap_err();
        assert_eq!(e.kind, Kind::NotADirectory);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn same_local_file_twice() {
        let d = scratch("samefile");
        std::fs::write(d.join("Case.txt"), b"x").unwrap();
        std::fs::hard_link(d.join("Case.txt"), d.join("case.txt")).unwrap();
        std::fs::write(d.join("other.txt"), b"y").unwrap();
        let t = [
            d.join("Case.txt"),
            d.join("case.txt"),
            d.join("other.txt"),
            d.join("none"),
        ];
        let r = existing_targets(&t);
        assert_eq!(
            r,
            [
                (true, Some(1)),
                (true, Some(0)),
                (true, None),
                (false, None)
            ]
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
