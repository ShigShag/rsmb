//! rsmb: an SMB shell client for high-latency links. Flags, start-up, the runtime, signals, the
//! non-interactive runner and exit codes. docs/architecture.md describes the whole program.

mod commands;
mod session;
mod shell;
mod transfer;
mod walk;

use std::future::Future;
use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{CommandFactory, Parser};
use tokio::runtime::Runtime;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Notify;

use commands::Out;
use session::{Error, Kind, Session, Target};

/// rsmb: a fast SMB shell client. 'rsmb --help-all' prints the whole manual.
#[derive(Parser, Debug)]
#[command(name = "rsmb", version)]
pub struct Args {
    /// Server name or address
    #[arg(
        short = 'H',
        long,
        value_name = "HOST",
        required_unless_present = "help_all"
    )]
    host: Option<String>,
    /// Server port
    #[arg(short = 'P', long, value_name = "PORT", default_value_t = 445)]
    port: u16,
    /// User name (empty: anonymous; Guest: guest). DOMAIN\user is split unless -d is given
    #[arg(short = 'u', long, value_name = "USER", default_value = "")]
    user: String,
    /// Domain or workgroup
    #[arg(short = 'd', long, value_name = "DOMAIN")]
    domain: Option<String>,
    /// Password (visible in the process list; without it rsmb prompts on a terminal)
    #[arg(short = 'p', long, value_name = "PASSWORD", conflicts_with = "no_pass")]
    password: Option<String>,
    /// Use an empty password and never prompt
    #[arg(long)]
    no_pass: bool,
    /// Connect through a SOCKS5 proxy: socks5://[user:pass@]host:port (the proxy resolves the
    /// server's name; percent-encode special characters in user and password; IPv6 proxy in
    /// brackets; visible in the process list)
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
    /// Response deadline in seconds; a silent server fails a request after between this and
    /// max(this, 15 s), one reply must be complete within 6 times this; connecting has its own
    /// 10 s, through a proxy too, which this does not change
    #[arg(long, value_name = "SECONDS", default_value_t = 30,
          value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
    /// Run the commands in FILE (one per line) first
    #[arg(short = 'S', long = "startup-script", value_name = "FILE")]
    startup_script: Option<PathBuf>,
    /// Run one command line (repeatable, in order, after -S)
    #[arg(short = 'C', long = "command", value_name = "LINE")]
    command: Vec<String>,
    /// No interactive shell after the commands
    #[arg(short = 'N', long = "not-interactive")]
    not_interactive: bool,
    /// Do not stop at the first failing command; exit with the first failure's code
    #[arg(short = 'k', long = "keep-going")]
    keep_going: bool,
    /// One JSON object per stdout line
    #[arg(long)]
    json: bool,
    /// Print the complete manual (as JSON with --json) and exit; needs no server
    #[arg(long)]
    help_all: bool,
}

/// Every start-up flag as (`-H, --host <HOST>`, help), from clap's own metadata.
pub fn options() -> Vec<(String, String)> {
    let mut cmd = Args::command();
    cmd.build();
    cmd.get_arguments()
        .map(|a| {
            let mut flag = String::new();
            if let Some(s) = a.get_short() {
                flag.push_str(&format!("-{s}"));
            }
            if let Some(l) = a.get_long() {
                if !flag.is_empty() {
                    flag.push_str(", ");
                }
                flag.push_str(&format!("--{l}"));
            }
            if a.get_action().takes_values()
                && let Some(names) = a.get_value_names()
            {
                for n in names {
                    flag.push_str(&format!(" <{n}>"));
                }
            }
            let mut help = a.get_help().map(|h| h.to_string()).unwrap_or_default();
            if a.get_action().takes_values() {
                for d in a.get_default_values().iter().filter(|d| !d.is_empty()) {
                    help.push_str(&format!(" [default: {}]", d.to_string_lossy()));
                }
            }
            (flag, help)
        })
        .collect()
}

pub enum Flow {
    Ok,
    Failed(u8),
    Exit,
    /// The interactive shell asks whether to overwrite (the message), then runs the line again
    /// with -f. Only `run_line` with `out.interactive` set returns it.
    Confirm(String),
}

static INTERRUPT: Notify = Notify::const_new();
/// A command or the start-up connect is running.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The interactive loop has started.
pub static SHELL: AtomicBool = AtomicBool::new(false);
pub static TERM: AtomicBool = AtomicBool::new(false);

/// Runs `work` on the runtime; None when SIGINT or SIGTERM arrived first (the work is dropped).
/// The caller sets `BUSY`.
fn interruptible<F: Future>(rt: &Runtime, work: F) -> Option<F::Output> {
    rt.block_on(async {
        let notified = INTERRUPT.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        tokio::select! {
            biased;
            _ = &mut notified => None,
            v = work => Some(v),
        }
    })
}

/// Runs one line on the runtime; an interrupt drops the command, then the cleanup of what it
/// left runs (itself interruptible) before the error is printed. `BUSY` is held from before the
/// command until after the cleanup, so a signal in between cannot exit past it. A stdout that
/// cannot be written ends the run here, with exit code 1, whatever `-k` says.
pub fn run_blocking(
    rt: &Runtime,
    sess: &mut Session,
    out: &mut Out,
    line: &str,
    force: bool,
) -> Flow {
    BUSY.store(true, Ordering::SeqCst);
    let flow = match interruptible(rt, commands::run_line(sess, out, line, force)) {
        Some(flow) => flow,
        None => {
            let left = match interruptible(rt, sess.cleanup(true)) {
                Some(left) => left,
                None => sess.take_left(),
            };
            sess.abandon();
            let line = line.trim();
            let mut e = Error::new(Kind::Interrupted, "interrupted");
            if !left.is_empty() {
                e.message
                    .push_str(&format!("; left on the server: {}", left.join(", ")));
                e.left = left;
            }
            out.error(&e, Some(line));
            out.done(line, false);
            Flow::Failed(130)
        }
    };
    BUSY.store(false, Ordering::SeqCst);
    if out.broken() {
        exit(out, 1);
    }
    flow
}

fn install_signals(rt: &Runtime) {
    let streams = rt.block_on(async {
        Ok::<_, std::io::Error>((
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        ))
    });
    let Ok((mut int, mut term)) = streams else {
        return;
    };
    rt.spawn(async move {
        loop {
            tokio::select! {
                _ = int.recv() => {
                    if BUSY.load(Ordering::SeqCst) {
                        INTERRUPT.notify_waiters();
                    } else if !SHELL.load(Ordering::SeqCst) {
                        std::process::exit(130);
                    }
                }
                _ = term.recv() => {
                    TERM.store(true, Ordering::SeqCst);
                    if BUSY.load(Ordering::SeqCst) {
                        INTERRUPT.notify_waiters();
                    } else {
                        // At the prompt or in a Tab: the line editor has the terminal raw.
                        shell::restore_terminal();
                        std::process::exit(130);
                    }
                }
            }
        }
    });
}

fn exit(out: &mut Out, code: u8) -> ! {
    out.flush();
    std::process::exit(code.into());
}

fn fail(out: &mut Out, e: &Error, at_startup: bool) -> ! {
    commands::report(out, e);
    exit(out, e.exit_code(at_startup));
}

fn main() {
    let args = match Args::try_parse() {
        Ok(a) => a,
        // --help and --version keep clap's output. Any other flag error is a JSON `error`
        // object when --json was given (clap has not parsed it if the error came first).
        Err(e) if e.use_stderr() && std::env::args_os().any(|a| a == "--json") => {
            // Everything before the blank line ("the following required arguments were not
            // provided:", then the argument), on one line.
            let text = e.to_string();
            let first: Vec<&str> = text.lines().take_while(|l| !l.trim().is_empty()).collect();
            let message = first.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
            let message = message.strip_prefix("error: ").unwrap_or(&message);
            fail(&mut Out::new(true), &Error::usage(message), true);
        }
        Err(e) => e.exit(),
    };
    let mut out = Out::new(args.json);
    if args.help_all {
        commands::help_all(&mut out);
        let code = if out.flush() { 0 } else { 1 };
        exit(&mut out, code);
    }
    let stdin_tty = std::io::stdin().is_terminal();

    // Usage checks, before anything touches the terminal or the network.
    let proxy = match args.proxy.as_deref().map(session::parse_proxy) {
        Some(Err(e)) => fail(&mut out, &e, true),
        Some(Ok(p)) => Some(p),
        None => None,
    };
    if proxy.is_some() && args.host.as_ref().is_some_and(|h| h.len() > 255) {
        fail(
            &mut out,
            &Error::usage(
                "-H: with --proxy the server name can be at most 255 bytes (a SOCKS5 request \
                 carries no longer one)",
            ),
            true,
        );
    }
    let mut lines: Vec<String> = Vec::new();
    if let Some(path) = &args.startup_script {
        match std::fs::read(path) {
            Ok(b) => lines.extend(String::from_utf8_lossy(&b).lines().map(String::from)),
            Err(e) => fail(
                &mut out,
                &Error::usage(format!("-S {}: {e}", path.display())),
                true,
            ),
        }
    }
    lines.extend(args.command.iter().cloned());
    let given = args.startup_script.is_some() || !args.command.is_empty();
    if args.not_interactive && !given && stdin_tty {
        fail(
            &mut out,
            &Error::usage("-N needs -C, -S or commands on stdin"),
            true,
        );
    }
    // --json never starts the line editor: its prompt and echo would go to stdout. Lines typed at
    // a terminal are then read like lines from a pipe, under the same rules.
    let interactive = stdin_tty && !args.not_interactive && !args.json;

    let (user, domain) = match (&args.domain, args.user.split_once('\\')) {
        (None, Some((d, u))) => (u.to_string(), d.to_string()),
        (d, _) => (args.user.clone(), d.clone().unwrap_or_default()),
    };
    let password = match (&args.password, args.no_pass || user.is_empty()) {
        (Some(p), _) => p.clone(),
        (None, true) => String::new(),
        (None, false) if stdin_tty => {
            let who = if domain.is_empty() {
                user.clone()
            } else {
                format!("{domain}\\{user}")
            };
            match rpassword::prompt_password(format!("Password for {who}: ")) {
                Ok(p) => p,
                Err(e) => fail(
                    &mut out,
                    &Error::new(Kind::LocalIo, format!("password: {e}")),
                    true,
                ),
            }
        }
        (None, false) => fail(
            &mut out,
            &Error::usage("no password: give -p or --no-pass"),
            true,
        ),
    };
    let target = Target {
        host: args.host.clone().unwrap_or_default(),
        port: args.port,
        user,
        password,
        domain,
        proxy,
        timeout: Duration::from_secs(args.timeout),
    };

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => fail(
            &mut out,
            &Error::new(Kind::LocalIo, format!("runtime: {e}")),
            true,
        ),
    };
    install_signals(&rt);
    BUSY.store(true, Ordering::SeqCst);
    let connected = interruptible(&rt, Session::connect(target));
    BUSY.store(false, Ordering::SeqCst);
    let mut sess = match connected {
        None => fail(
            &mut out,
            &Error::new(Kind::Interrupted, "interrupted"),
            true,
        ),
        Some(Err(e)) => fail(&mut out, &e, true),
        Some(Ok(s)) => s,
    };
    if interactive {
        let _ = writeln_stderr(&sess.describe());
    }

    // -S, then -C.
    let mut first_failure: Option<u8> = None;
    for line in &lines {
        match run_blocking(&rt, &mut sess, &mut out, line, false) {
            Flow::Ok | Flow::Confirm(_) => {}
            Flow::Exit => exit(&mut out, first_failure.unwrap_or(0)),
            Flow::Failed(130) if !interactive || TERM.load(Ordering::SeqCst) => exit(&mut out, 130),
            Flow::Failed(c) => {
                first_failure.get_or_insert(c);
                if !args.keep_going {
                    if interactive {
                        break;
                    }
                    exit(&mut out, c);
                }
            }
        }
    }
    if interactive {
        SHELL.store(true, Ordering::SeqCst);
        let code = shell::run(&rt, sess, &mut out);
        exit(&mut out, code);
    }
    if !given {
        let stdin = std::io::stdin();
        for line in stdin.lock().split(b'\n') {
            let line = match line {
                Ok(l) => String::from_utf8_lossy(&l).into_owned(),
                Err(e) => fail(
                    &mut out,
                    &Error::new(Kind::LocalIo, format!("stdin: {e}")),
                    false,
                ),
            };
            match run_blocking(&rt, &mut sess, &mut out, &line, false) {
                Flow::Ok | Flow::Confirm(_) => {}
                Flow::Exit => break,
                Flow::Failed(130) => exit(&mut out, 130),
                Flow::Failed(c) => {
                    first_failure.get_or_insert(c);
                    if !args.keep_going {
                        exit(&mut out, c);
                    }
                }
            }
        }
    }
    exit(&mut out, first_failure.unwrap_or(0));
}

fn writeln_stderr(s: &str) -> std::io::Result<()> {
    use std::io::Write;
    writeln!(std::io::stderr(), "{s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_sound() {
        Args::command().debug_assert();
        assert!(Args::try_parse_from(["rsmb", "-H", "h", "-p", "x", "--no-pass"]).is_err());
        assert!(Args::try_parse_from(["rsmb", "--help-all"]).is_ok());
        assert!(Args::try_parse_from(["rsmb"]).is_err(), "-H is required");
        assert!(Args::try_parse_from(["rsmb", "-H", "h", "--timeout", "0"]).is_err());
        let a = Args::try_parse_from(["rsmb", "-H", "h", "-u", "TESTDOM\\tester"]).unwrap();
        assert_eq!(a.user.split_once('\\'), Some(("TESTDOM", "tester")));
        let a = Args::try_parse_from(["rsmb", "-H", "h", "-C", "pwd", "-C", "ls"]).unwrap();
        assert_eq!(a.command, ["pwd", "ls"]);
        let opts = options();
        let manual = crate::commands::tests::rendered(false);
        for a in Args::command().get_arguments() {
            if let Some(l) = a.get_long() {
                assert!(
                    opts.iter().any(|(f, _)| f.contains(&format!("--{l}"))),
                    "{l}"
                );
                assert!(
                    manual.contains(&format!("--{l}")),
                    "--{l} missing from help all"
                );
            }
        }
        for f in ["--help", "--version", "-H, --host <HOST>", "--no-pass"] {
            assert!(manual.contains(f), "{f}");
        }
        for (flag, help) in &opts {
            if flag.contains("--proxy") {
                assert!(
                    help.contains("the proxy resolves the server's name"),
                    "{help}"
                );
            }
            if flag.contains("--timeout") {
                assert!(help.contains("connecting has its own 10 s"), "{help}");
            }
            if flag.contains("--port") {
                assert!(help.ends_with("[default: 445]"), "{help}");
            }
            if flag.contains("--timeout") {
                assert!(help.ends_with("[default: 30]"), "{help}");
            }
        }
    }
}
