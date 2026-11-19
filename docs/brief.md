# The user's brief

> The original task as the user gave it on 2026-11-17, unchanged except that absolute paths on the
> user's machine were made relative. These are the requirements a reviewer checks the work against.
>
> **Later instructions from the user override parts of it.** They are listed under
> "Amendments" at the end and are binding.

## Task: build a fast Rust SMB client shell (replacement for my use of smbclient-ng)

### What I want and why

I use smbclient-ng (Python, built on impacket) as an interactive SMB shell on Linux. It is slow where it matters most: every `cd` takes long, especially over VPN or other high-latency links. I want a new client written in Rust that covers the basic functionality, has reliable upload and download, and makes navigation feel instant.

I do not need feature parity with smbclient-ng. I need the core done well. `tree` and `find` are the two extras I care about.

The Python source is at `../smbclient-ng`. Treat it as a read-only reference for behaviour and command syntax. Create the Rust project as a new git repository at `rsmb` (binary name `rsmb`).

### Why the current client is slow (already investigated)

The cost is network round trips, not Python CPU time. A rewrite that copies the same request pattern will be just as slow, so the design below matters more than the language.

- Every prompt opens a brand-new TCP connection to port 445 as a liveness check, then sends an SMB echo (`smbclientng/utils/utils.py:527`, `smbclientng/core/SMBSession.py:327`).
- Every `cd` verifies the directory by running a wildcard listing on the parent. impacket's `listPath` does tree connect, create, query directory (at least twice), close, tree disconnect: about six round trips to check one directory (`smbclientng/core/SMBSession.py:1375`).
- Every Tab press lists the whole parent directory again with no cache (`smbclientng/core/CommandCompleter.py:181`).

Design requirements that follow from this:

- One TCP connection and one tree connect per share, kept open for the session. No per-prompt liveness probe; detect a dead connection when a request fails and reconnect then.
- `cd` checks the target with a single open/close of that directory (or one compound request if the library supports it), never a listing.
- Directory listings are cached per session and reused by tab completion. Invalidate on any command that changes the directory (`put`, `rm`, `mkdir`, `rmdir`, rename) and offer a manual refresh.
- Requests that are independent (traversal for `tree` and `find`, chunks of a large transfer) run concurrently on the one connection, with a bounded concurrency limit.

### Library recommendation

This comes from web research on 2026-11-17. Nothing was compiled or run, so verify before relying on any of it.

1. **`smb` (smb-rs, https://github.com/afiffon/smb-rs)**: first choice. Pure Rust, async on tokio, SMB 2.x to 3.1.1 with signing and encryption, concurrent requests on one connection, share listing, named pipes, parsed security descriptors. Known problems to check: issue #184 (no `TCP_NODELAY`, which can add about 40 ms per round trip and would directly hurt navigation), issue #185 (credits from interim responses dropped, can hang a connection), strict dependency version pins, no compound requests found.
2. **`smb2` (https://github.com/vdavid/smb2)**: fallback. Pure Rust, async, very active, compound requests, pipelined I/O, reconnect, DFS. Weaker points: API changes often, methods take `&mut self` so concurrent independent requests are unverified, no generic named-pipe access.

Do not use `pavao`. It wraps libsmbclient and serializes every call behind a global lock.

Suggested supporting crates, yours to confirm or replace: `tokio`, `clap` for arguments, `rustyline` for the interactive line editor and completion, `tokio-socks` for SOCKS5, `indicatif` for transfer progress. Keep the dependency list short.

Compatibility with Windows servers is unverified for both SMB crates. The test environment below is Samba only; say so in the README rather than claiming Windows support.

### Stages

Finish and verify each stage before starting the next. Commit locally at the end of each stage. Do not push anywhere.

**Stage 0: test environment and library decision**

- Build the test environment (see below).
- Write a small spike with each candidate crate: connect, list shares, list a directory, check that a directory exists, read a file. Measure the number of SMB requests and the wall time for each, with and without injected latency.
- Check whether each crate accepts a caller-supplied TCP stream or transport. SOCKS5 support in Stage 4 depends on this, so it is part of the decision now. If neither crate allows it, find out how small a patch or fork would be.
- Pick the crate and record the measurements and reasons in the README. If neither crate is workable, stop and tell me what you found before going further.

**Stage 1: connection and navigation**

- Connect with host, port, username, password and domain, plus guest/anonymous. Other authentication methods are out of scope for now.
- Interactive shell with history and tab completion for commands and remote paths.
- Non-interactive mode that runs commands given on the command line and exits with a meaningful status code. The integration tests will use it.
- Commands: `shares`, `use`, `cd`, `pwd`, `ls` (sizes, timestamps, directory markers), `help`, `exit`.
- All of the design requirements from the section above.

**Stage 2: transfers and file operations**

- `get` and `put`, single file and recursive, keeping directory structure.
- Streaming I/O: a multi-gigabyte file must not be held in memory.
- Progress display, correct byte counts, and a non-zero exit status on failure in non-interactive mode.
- A failed or interrupted transfer must not leave a file that looks complete. Write to a temporary name and rename on success, or clearly report the partial file.
- Refuse to overwrite silently; require a flag or a prompt.
- `mkdir`, `rm`, `rmdir`, `cat`, and the local helpers needed to make transfers usable (`lcd`, `lpwd`, `lls`).

**Stage 3: tree and find**

- `tree` with a depth limit.
- `find` with filters for name (glob), type (file or directory), size and depth.
- Both traverse concurrently within the bounded limit and keep going past directories that deny access, reporting those at the end.

**Stage 4: SOCKS5 proxy**

- A `--proxy socks5://[user:pass@]host:port` option. All traffic to the server goes through the proxy, including name resolution when a hostname is given.
- Everything from Stages 1 to 3 works through the proxy.

**Not in scope unless I ask later:** ACL display, the module system, multiple simultaneous sessions, mount, the remaining smbclient-ng commands.

### Test environment

- A Samba server in a container, started with one command. Check whether Docker or Podman is installed and use what is there.
- Fixture data created by a script: a deep directory tree, a directory with around 10,000 entries, a file larger than 1 GB, names with spaces and non-ASCII characters, a read-only share, and a directory the test user cannot enter.
- A way to inject latency (for example 100 ms each way) between client and server, so navigation speed can be measured rather than guessed.
- A SOCKS5 proxy container for Stage 4, with the Samba server on a network reachable only through that proxy, so a test that passes proves the traffic went through it.
- Integration tests that run the real binary against this environment. Transfer tests compare checksums of the source and the result.

Speed targets, measured in the integration tests:

- After the initial connect, no command opens another TCP connection.
- `cd` into an existing directory costs at most two SMB requests.
- A second Tab completion in the same directory costs zero requests.
- Report the measured `cd` and `ls` times for both this client and smbclient-ng at 100 ms injected latency.

### How to run the work

- Use Opus subagents for implementation work, and keep stages that touch the same files in sequence rather than in parallel.
- **Planning:** after Stage 0, write a plan covering Stages 1 to 4. Then give that plan to adversarial Opus agents whose job is to break it, each from a different angle: protocol behaviour and round-trip counts, risk in the chosen library, data-loss cases in transfers, and whether the tests would actually catch a regression. Give them the plan and the evidence, not your own opinion of it. Revise the plan with what holds up and tell me what you changed and what you rejected.
- **Review:** when all stages are done, run independent Opus review agents over the finished code, covering correctness, transfer safety, and performance against the targets above. Reviewers get the code and the requirements, not your summary of how it went. Fix what they confirm, then run the full test suite again.
- Keep the code plain. No plugin system, no abstraction layers for things that have a single implementation, nothing built for features outside the stages above.

### What to report back

- At the end of each stage: what works, the test results with real output, and anything skipped or failing. Carry on to the next stage without waiting for me unless a stage fails its own tests.
- At the end: the measured comparison against smbclient-ng, the review findings and what was done about each, and a list of known limitations.
- Stop and ask me only if the library decision in Stage 0 has no workable option, or if something would require changing the scope above.

## Amendments (given by the user during planning, 2026-11-17; these override the text above)

1. **Location and remote.** The project lives in this repository, not in a new `rsmb` directory. The binary is still `rsmb`. Commit and **push to `origin` after each stage**. Treat the remote as a plain git server: no GitHub Actions, nothing GitHub-specific.
2. **Hostile test environment.** It must pose hard cases that could hurt performance, such as very many files in one directory, and other edge cases.
3. **Names.** It must include file and directory names in Chinese, Korean and other non-Latin scripts.
4. **Documentation.** A central `AGENTS.md` and a `docs/` folder where everything is documented in markdown. `AGENTS.md` is, among other things, the table of contents of `docs/`, and tells agents that anything new or any big change must be reflected in the documentation. Keeping it current is very important. Principles that prove good during the work (for example how to set up a test environment) are recorded in `AGENTS.md`. No `CLAUDE.md`.
5. **Three uses.** The project should be easy to develop by AI end to end; usable interactively by the user; usable non-interactively by an AI.
6. **`help all`.** A command that prints every command with examples, so an AI that does not know the program can learn how to use it. `AGENTS.md` states that this is kept up for every new feature.
7. **Automated testing.** Local only. Run after each major feature addition unless the user instructs otherwise; this rule is written down in `AGENTS.md`.
8. **Process.** This replaces "How to run the work" above. The main session only orchestrates. One master plan describes the four stages. Per stage: a separate Fable agent writes the stage plan; one Opus adversary agent attacks that plan; one Opus agent implements it; one Opus agent reviews the result; each finding is validated as true before anything is fixed; one Sonnet agent fixes the confirmed findings; then commit. One agent per task, not several agents in parallel on the same task.
9. **Stage 5: Kerberos** (2026-11-18). Kerberos login with a ticket is a required feature. It takes
   `-k` (`--kerberos`), which uses the ticket cache (`KRB5CCNAME`). Only the ticket cache: no
   password-based Kerberos, no flag for the KDC host, no certificate login. Through `--proxy` it
   must work too, including the exchange with the KDC (as smbclient-ng does under proxychains);
   nothing goes around the proxy. `--keep-going` keeps only its long form. Work stays in this
   repository; each stage is pushed to `origin` and also to the public repository
   `ShigShag/rsmb`. Commit messages carry no AI attribution (no `Co-Authored-By`, no session link).
10. **Stage 6: `access`** (2026-11-19). "Right now there's no way for me to check the permissions
   on files I have listed in a directory or directly, like to see if I would be able to place a
   file inside a directory." It becomes an extra command, so that `cd` and `ls` keep their
   behaviour and their round trips. The server is asked (MxAc), nothing is tried. Every agent of
   this stage runs on Opus, the planner and the fix agent included. Decided on the plan: the
   check before a large `put` asks for attributes only (it refused a `put -f` that a small file
   passes); `rmdir` is shown as unknown when the directory may be deleted but its parent lets
   nothing change.
