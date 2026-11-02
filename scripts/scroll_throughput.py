#!/usr/bin/env python3
"""Sustained-scroll throughput for the live TUI, list pane and detail pane.

Per-notch latency says nothing about how a gesture feels; what matters is how
fast a whole gesture retires. This fires a realistic burst of wheel notches
(as a trackpad produces) and reports notches/second and how long the last
repaint took to arrive.

Usage: scroll_throughput.py [binary] [repo]
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
WHEEL_DOWN, MOTION = 65, 35


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


def gesture(fd, notches, col, row, motion_per_notch=1, gap=0.008):
    """Fire `notches` wheel events, returning (seconds, bytes) to drain it."""
    payload = bytearray()
    for i in range(notches):
        for m in range(motion_per_notch):
            payload += sgr(MOTION, col + m, row + m, True)
        payload += sgr(WHEEL_DOWN, col, row, True)
        payload += sgr(WHEEL_DOWN, col, row, False)
    t0 = time.perf_counter()
    os.write(fd, bytes(payload))
    total = 0
    # Retire the gesture: keep reading until the stream goes quiet.
    last = t0
    while time.perf_counter() - last < 0.25:
        r, _, _ = select.select([fd], [], [], 0.05)
        if not r:
            continue
        try:
            chunk = os.read(fd, 1 << 16)
        except OSError:
            break
        if not chunk:
            break
        total += len(chunk)
        last = time.perf_counter()
    return last - t0, total


def run(fd, label, notches, col, row, motion_per_notch, gap=0.008):
    times = []
    for _ in range(5):
        d, b = gesture(fd, notches, col, row, motion_per_notch, gap)
        times.append(d)
        quiet(fd, 0.15)
    med = statistics.median(times)
    print(
        f"  {label:<40} {notches:>4} notches in {med * 1000:7.1f} ms"
        f"  = {notches / med:8.0f} notches/s"
    )
    return med


def main():
    pid, fd = spawn()
    quiet(fd, 6.0)
    os.write(fd, b"\x1b")  # dismiss agent-file prompt
    quiet(fd, 1.0)

    print(f"== sustained scroll, {COLS}x{ROWS}, {os.path.basename(CWD)} ==")
    # Left pane = list. Right of the 40% split = detail pane.
    run(fd, "list pane, 40 notches", 40, 20, 14, 1)
    run(fd, "list pane, 40 notches + motion", 40, 20, 14, 3)
    run(fd, "detail pane, 40 notches", 40, 150, 14, 1)
    run(fd, "detail pane, 120 notches", 120, 150, 14, 1)
    run(fd, "list, 200 notches", 200, 20, 14, 1)

    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    main()
