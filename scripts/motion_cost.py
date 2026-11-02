#!/usr/bin/env python3
"""Cost of pointer motion vs a real scroll, in bytes the TUI writes.

The redraw policy is meant to repaint when state changes, not when input
arrives. SGR 1003 makes the terminal report every pointer movement, so if the
loop treats "an event arrived" as "repaint", simply moving the mouse redraws
the whole screen. Bytes written for a pure-motion stream is the direct check:
nothing on screen changes, so the right answer is zero.

Two things make a naive measurement lie, and both are handled here:
  - The TUI repaints once a second for the freshness badge, so every
    measurement has a baseline subtracted.
  - A wheel gesture that runs past the end of the list changes nothing and
    writes nothing, so the list is rewound to the top before each sample.

Usage: motion_cost.py [binary] [repo]
"""
import os
import pty
import select
import statistics
import sys
import time

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/bvr")
CWD = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/projects/ultraworkers")
COLS, ROWS = 200, 50
MOTION, WHEEL_UP, WHEEL_DOWN = 35, 64, 65


def sgr(b, c, r, p=True):
    return f"\x1b[<{b};{c + 1};{r + 1}{'M' if p else 'm'}".encode()


def spawn():
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(CWD)
        os.environ["TERM"] = "xterm-256color"
        os.environ["BV_NO_UPDATE_CHECK"] = "1"
        os.execv(BIN, [BIN])
    import fcntl
    import struct
    import termios
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    return pid, fd


def quiet(fd, s=0.4):
    end = time.time() + s
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.02)
        if not r:
            return
        try:
            if not os.read(fd, 1 << 16):
                return
        except OSError:
            return


def wheel(n, down):
    b = WHEEL_DOWN if down else WHEEL_UP
    out = bytearray()
    for _ in range(n):
        out += sgr(b, 20, 14, True)
        out += sgr(b, 20, 14, False)
    return bytes(out)


def motion(n):
    out = bytearray()
    for i in range(n):
        out += sgr(MOTION, 10 + (i % 90), 5 + (i % 30), True)
    return bytes(out)


def measure(fd, payload, window=0.30):
    """Bytes written after `payload`, over a fixed window."""
    os.write(fd, payload)
    total = 0
    end = time.time() + window
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.01)
        if not r:
            continue
        try:
            chunk = os.read(fd, 1 << 16)
        except OSError:
            break
        if not chunk:
            break
        total += len(chunk)
    return total


def rewind(fd):
    """Put the list back at the top so a scroll gesture has room to move."""
    os.write(fd, wheel(400, False))
    quiet(fd, 0.4)


def sample(fd, payload_fn, n_events, reps=5):
    """Median bytes attributable to the payload, baseline removed."""
    # Baseline: the same window with no input at all (just the 1 Hz tick).
    quiet(fd, 0.4)
    baselines = [measure(fd, b"") for _ in range(reps)]
    base = statistics.median(baselines)

    deltas = []
    for _ in range(reps):
        rewind(fd)
        d = measure(fd, payload_fn(n_events))
        deltas.append(d)
    return base, statistics.median(deltas)


def main():
    pid, fd = spawn()
    quiet(fd, 6.0)
    os.write(fd, b"\x1b")  # dismiss the agent-file prompt
    quiet(fd, 1.0)

    print(f"== bytes written per input, {COLS}x{ROWS}, {os.path.basename(CWD)} ==")
    print(f"  (idle baseline subtracted; 1 Hz badge tick is the only background paint)\n")

    base, m100 = sample(fd, motion, 100)
    print(f"  100 pointer-motion events, nothing changes : {m100:>8} bytes")
    base, m1000 = sample(fd, motion, 1000)
    print(f" 1000 pointer-motion events, nothing changes : {m1000:>8} bytes")
    base, w10 = sample(fd, lambda n: wheel(n, True), 10)
    print(f"   10 wheel notches, list scrolls            : {w10:>8} bytes")
    base, w50 = sample(fd, lambda n: wheel(n, True), 50)
    print(f"   50 wheel notches, list scrolls            : {w50:>8} bytes")
    print(f"\n  idle baseline (1 Hz tick): {base} bytes per {0.30}s window")

    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    main()
