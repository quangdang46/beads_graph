#!/usr/bin/env python3
"""CPU the TUI burns per input event, measured on the live process.

Bytes written are the wrong metric for the redraw policy: ratatui diffs the
buffer and writes only changed cells, so a redundant repaint of an unchanged
screen writes nothing while still costing a full view rebuild. The cost is
CPU, so this samples the child's user+sys time across a known number of input
events and reports microseconds per event.

Usage: input_cpu_cost.py [binary] [repo]
"""
import os
import pty
import select
import statistics
import subprocess
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


def quiet(fd, s=0.35):
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


def child_cpu(pid):
    """Cumulative CPU seconds of the live child.

    `getrusage(RUSAGE_CHILDREN)` only accounts for children that have been
    reaped, so it reads zero for a process still running. `ps -o time=` is
    the live equivalent. It reports centiseconds, so samples feed thousands
    of events rather than tens.
    """
    out = subprocess.run(
        ["ps", "-p", str(pid), "-o", "time="], capture_output=True, text=True
    ).stdout.strip()
    if not out:
        return 0.0
    parts = out.split(":")
    secs = 0.0
    for p in parts:
        secs = secs * 60 + float(p)
    return secs


def wheel(n, down=True):
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


def timed(fd, pid, payload, settle=0.25):
    """CPU seconds consumed while the TUI chews through `payload`."""
    quiet(fd, 0.2)
    before = child_cpu(pid)
    os.write(fd, payload)
    # Wait for it to go quiet so the whole burst is accounted for.
    end = time.time() + 3.0
    last = time.time()
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.02)
        if r:
            try:
                if not os.read(fd, 1 << 16):
                    break
            except OSError:
                break
            last = time.time()
        elif time.time() - last > settle:
            break
    return child_cpu(pid) - before


def measure(fd, pid, label, payload_fn, n, reps=3, rewind=True, pace=0.001):
    """CPU per event when events arrive spread out, as a real gesture does.

    Feeding a burst at once measures nothing: the drain collapses it into a
    single repaint no matter how long it is. A moving hand delivers events
    over time, so each one closes its own burst and costs its own frame. The
    `pace` sleep between writes is what reproduces that.
    """
    vals = []
    for _ in range(reps):
        if rewind:
            os.write(fd, wheel(300, False))
            quiet(fd, 0.25)
        quiet(fd, 0.2)
        before = child_cpu(pid)
        chunk = payload_fn(n)
        # Split the payload back into single events so each can be paced.
        step = len(chunk) // n if n else len(chunk)
        for i in range(0, len(chunk), max(1, step)):
            os.write(fd, chunk[i : i + max(1, step)])
            if pace:
                time.sleep(pace)
        end = time.time() + 2.0
        last = time.time()
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.02)
            if r:
                try:
                    if not os.read(fd, 1 << 16):
                        break
                except OSError:
                    break
                last = time.time()
            elif time.time() - last > 0.25:
                break
        vals.append(child_cpu(pid) - before)
    med = statistics.median(vals)
    print(f"  {label:<48} {med * 1e6 / n:8.1f} us/event  ({med * 1000:7.1f} ms per {n})")
    return med


def main():
    pid, fd = spawn()
    quiet(fd, 6.0)
    os.write(fd, b"\x1b")  # dismiss the agent-file prompt
    quiet(fd, 1.0)

    idle = []
    for _ in range(3):
        quiet(fd, 0.2)
        b = child_cpu(pid)
        time.sleep(1.0)
        idle.append(child_cpu(pid) - b)
    print(f"== CPU per input event, {COLS}x{ROWS}, {os.path.basename(CWD)} ==")
    print(f"  {'idle (1 Hz badge tick)':<46} {statistics.median(idle) * 1e6:8.1f} us/tick\n")

    measure(fd, pid, "1500 pointer-motion events (nothing changes)", motion, 1500)
    measure(fd, pid, "600 wheel notches (list scrolls)", lambda n: wheel(n, True), 600)
    # A pure wheel-up burst at the very top changes nothing, so it isolates
    # the cost of a repaint that changes nothing at all.
    os.write(fd, wheel(300, False))
    quiet(fd, 0.4)
    measure(
        fd, pid, "600 wheel notches at the top (no movement)",
        lambda n: wheel(n, False), 600, rewind=False,
    )

    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    main()
