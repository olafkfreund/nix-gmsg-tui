"""Plays the demo scenario: runs PROGRAM in a pseudo-terminal inside this terminal, and types
the keys of the scenario into it once Space is pressed (or after --start-after seconds).

    python3 doc/play.py [--start-after SECONDS] PROGRAM

Everything else passes through both ways (output, keys, the terminal's replies), so images
show as the terminal draws them. Used by doc/record.sh; quits the program at the end.
"""

import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import tty
import fcntl

ENTER, ESC, UP, DOWN = b"\r", b"\x1b", b"\x1b[A", b"\x1b[B"

# (pause before, keys): text is typed a character at a time.
SCENARIO = [
    # The chat list: pinned chats, groups, unread counts, folders below; `?` shows all the
    # keys, and again those that fit.
    (2.5, "?"),
    (2.0, "?"),
    (0.8, ENTER),
    # A group chat with photos, reactions, a reply; an answer with an emoji shortcode.
    (3.5, ENTER),
    (0.5, "Count me in for the tram "),
    (0.3, ":tram"),
    (1.2, ENTER),
    (0.4, " see you at 8!"),
    (0.6, ENTER),
    # A reaction to a photo: select it, `e`, then 😂 (the 4th of the usual ones).
    (1.5, UP),
    (0.5, UP),
    (0.8, "e"),
    (1.2, "4"),
    (1.5, ESC),
    (0.6, ESC),
    # Another group, with unread messages.
    (1.0, DOWN),
    (0.4, DOWN),
    (0.8, ENTER),
    (3.0, ESC),
    # A message the phone could not send, shown as on the phone.
    (0.8, DOWN),
    (0.3, DOWN),
    (0.3, DOWN),
    (0.3, DOWN),
    (0.8, ENTER),
    (2.5, ESC),
    # Two chats marked, then archived together.
    (0.8, UP),
    (0.5, " "),
    (0.5, DOWN),
    (0.3, DOWN),
    (0.5, " "),
    (1.0, "a"),
    (2.0, "/"),
    # Search across chats and messages.
    (0.4, "parcel"),
    (2.0, ESC),
    # A message to several people: a new group from the contacts of the phone.
    (0.6, "n"),
    (0.6, "ali"),
    (0.8, ENTER),
    (0.4, "bob"),
    (0.8, ENTER),
    (1.0, ENTER),
    (0.8, "Pastel de nata tasting on Saturday?"),
    (0.6, ENTER),
    (2.5, ESC),
    (1.5, "q"),
]
TYPING = 0.055


def schedule(start):
    """The scenario from `start`: (time, bytes), text one character at a time."""
    at, keys_at = start, []
    for pause, keys in SCENARIO:
        if isinstance(keys, bytes):
            at += pause
            keys_at.append((at, keys))
        else:
            for i, c in enumerate(keys):
                at += pause if i == 0 else TYPING
                keys_at.append((at, c.encode()))
    return keys_at


def window_size(fd):
    return fcntl.ioctl(fd, termios.TIOCGWINSZ, struct.pack("HHHH", 0, 0, 0, 0))


def main():
    args = sys.argv[1:]
    start_after = None
    if args[:1] == ["--start-after"]:
        start_after, args = float(args[1]), args[2:]
    if len(args) != 1:
        sys.exit(__doc__)
    pid, master = pty.fork()
    if pid == 0:
        os.execv(args[0], args)
    stdin, stdout = sys.stdin.fileno(), sys.stdout.fileno()
    interactive = os.isatty(stdin)

    def resize(*_):
        if interactive:
            fcntl.ioctl(master, termios.TIOCSWINSZ, window_size(stdout))

    resize()
    signal.signal(signal.SIGWINCH, resize)
    saved = termios.tcgetattr(stdin) if interactive else None
    if interactive:
        tty.setraw(stdin)
    # The keys still to type, in order; None until started.
    playing = None
    if start_after is not None:
        playing = schedule(time.monotonic() + start_after)
    try:
        while True:
            due = playing[0][0] if playing else None
            timeout = None if due is None else max(0.0, due - time.monotonic())
            try:
                ready, _, _ = select.select([master, stdin], [], [], timeout)
            except InterruptedError:
                continue
            if master in ready:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                os.write(stdout, data)
            if stdin in ready:
                data = os.read(stdin, 1024)
                if playing is None and data == b" ":
                    playing = schedule(time.monotonic())
                elif data:
                    os.write(master, data)
            while playing and playing[0][0] <= time.monotonic():
                os.write(master, playing.pop(0)[1])
    finally:
        if saved is not None:
            termios.tcsetattr(stdin, termios.TCSADRAIN, saved)
    os.waitpid(pid, 0)


if __name__ == "__main__":
    main()
