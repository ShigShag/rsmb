#!/usr/bin/env python3
"""Run a command on a new pseudo-terminal and relay bytes: our stdin -> terminal, terminal -> our
stdout. Used by the interactive tests (docs/testing.md). Not called pty.py: that name would shadow
python's own pty module.

Usage: ptyrun.py <cols> <rows> <command> [args...]
Exit status: the command's, or 128 + signal if it was killed."""
import fcntl, os, pty, select, struct, sys, termios

cols, rows = int(sys.argv[1]), int(sys.argv[2])
pid, master = pty.fork()
if pid == 0:
    os.execvp(sys.argv[3], sys.argv[3:])
fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
stdin_open = True
while True:
    r, _, _ = select.select([master] + ([0] if stdin_open else []), [], [])
    if master in r:
        try:
            data = os.read(master, 65536)
        except OSError:
            data = b""
        if not data:
            break
        os.write(1, data)
    if 0 in r:
        data = os.read(0, 65536)
        if data:
            os.write(master, data)
        else:
            stdin_open = False
_, status = os.waitpid(pid, 0)
sys.exit(os.waitstatus_to_exitcode(status) if os.WIFEXITED(status) else 128 + os.WTERMSIG(status))
