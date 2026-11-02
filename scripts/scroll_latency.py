#!/usr/bin/env python3
"""Measure input->paint latency of the live TUI, with and without motion noise.

Frame cost is not the same as felt latency. The event loop drains a whole
pending burst before painting, and crossterm's EnableMouseCapture turns on
SGR 1003, so every pointer movement is an event too. A hand on a trackpad
generates those continuously, so this measures what a scrolling session
actually experiences: notches interleaved with motion, as a real gesture
produces them.

Usage: scroll_latency.py [binary] [repo]
"""
import os
import pty
import re
import select
import statistics
import sys
import time

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/bvr")
CWD = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/projects/ultraworkers")
COLS, ROWS = 200, 50
WHEEL_DOWN, MOTION = 65, 35


def sgr(button, col, row, press=True):
    return f"\x1b[<{button};{col + 1};{row + 1}{'M' if press else 'm'}".encode()


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


def quiet(fd, seconds=0.4):
    """Read until the TUI has nothing left to say."""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.02)
        if not r:
            return
        try:
            if not os.read(fd, 1 << 16):
                return
        except OSError:
            return


def latency(fd, pre_noise=0):
    """Write one wheel notch, return seconds until the repaint arrives."""
    noise = b""
    for i in range(pre_noise):
        # Motion events a moving hand produces alongside the gesture.
        noise += sgr(MOTION, 20 + (i % 7), 10 + (i % 5), True)
    os.write(fd, noise)
    t0 = time.perf_counter()
    os.write(fd, sgr(WHEEL_DOWN, 20, 14, True))
    os.write(fd, sgr(WHEEL_DOWN, 20, 14, False))
    while True:
        r, _, _ = select.select([fd], [], [], 2.0)
        if not r:
            return None
        try:
            chunk = os.read(fd, 1 << 16)
        except OSError:
            return None
        if not chunk:
            return None
        if len(chunk) > 0:
            return time.perf_counter() - t0


def sample(fd, n=25, pre_noise=0):
    out = []
    for _ in range(n):
        d = latency(fd, pre_noise)
        if d is None:
            return out
        out.append(d)
        quiet(fd, 0.12)
    return out


def report(label, xs):
    if not xs:
        print(f"  {label:<34} no samples")
        return
    ms = sorted(x * 1000 for x in xs)
    print(
        f"  {label:<34} n={len(ms):<3} "
        f"median {statistics.median(ms):7.2f} ms   "
        f"p90 {ms[int(len(ms) * 0.9) - 1]:7.2f} ms   max {ms[-1]:7.2f} ms"
    )


def main():
    pid, fd = spawn()
    quiet(fd, 6.0)
    os.write(fd, b"\x1b")  # dismiss the agent-file prompt
    quiet(fd, 1.0)

    print(f"== input->paint latency, {COLS}x{ROWS}, {os.path.basename(CWD)} ==")
    report("wheel only", sample(fd, 25, 0))
    report("wheel + 4 motion events queued", sample(fd, 25, 4))
    report("wheel + 30 motion events queued", sample(fd, 25, 30))
    report("wheel + 120 motion events queued", sample(fd, 25, 120))

    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    main()
