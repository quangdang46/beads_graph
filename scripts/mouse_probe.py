#!/usr/bin/env python3
"""Drive the real bvr binary under a pty with a minimal terminal emulator.

The unit tests call `handle_mouse` directly, so they cannot see whether the
terminal was really in the alternate screen with mouse capture on, and they
cannot see which row a click actually landed on. This replays what a terminal
sends (SGR 1006 mouse reports) and reconstructs the screen from the byte
stream, so both are checked against the real binary on the real dataset.

Usage: mouse_probe.py [binary] [repo-dir]
"""
import os
import pty
import re
import select
import sys
import time

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/bvr")
CWD = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/projects/ultraworkers")
COLS, ROWS = 200, 50

LEFT, WHEEL_UP, WHEEL_DOWN = 0, 64, 65

# A list row is `<marker><icon>  <prio>  <STATUS> <id> <title>`, so the id is
# whatever follows the status badge. Anchoring on the badge beats guessing at
# the id shape: these datasets use `omp-zjb` and `r0-grp-a-102` alike.
ID_RE = re.compile(
    r"\b(?:OPEN|IN_PROGRESS|BLOCKED|DONE|CLOSED|DEFERRED)\s+([A-Za-z0-9][A-Za-z0-9._-]*)"
)


def sgr(button, col, row, press=True):
    """SGR 1006 report. col/row are 0-based screen coords, as a user sees
    them; the wire format is 1-based, so both are +1 here."""
    return f"\x1b[<{button};{col + 1};{row + 1}{'M' if press else 'm'}".encode()


class Screen:
    """Just enough VT100 to reconstruct what the user sees."""

    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.x = self.y = 0

    def put(self, ch):
        if 0 <= self.y < self.rows and 0 <= self.x < self.cols:
            self.grid[self.y][self.x] = ch
        self.x += 1
        if self.x >= self.cols:
            self.x = 0
            self.y = min(self.y + 1, self.rows - 1)

    def feed(self, data):
        s = data.decode("utf-8", "replace")
        i, n = 0, len(s)
        while i < n:
            c = s[i]
            if c == "\x1b":
                m = re.match(r"\x1b\[([0-9;?]*)([a-zA-Z])", s[i:])
                if m:
                    self.csi(m.group(1), m.group(2))
                    i += m.end()
                    continue
                m = re.match(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", s[i:])
                if m:
                    i += m.end()
                    continue
                i += 2 if i + 1 < n else 1
                continue
            if c == "\r":
                self.x = 0
            elif c == "\n":
                self.y = min(self.y + 1, self.rows - 1)
            elif c == "\b":
                self.x = max(0, self.x - 1)
            elif c >= " ":
                self.put(c)
            i += 1

    def csi(self, params, final):
        priv = params.startswith("?")
        p = params[1:] if priv else params
        args = [int(x) for x in p.split(";") if x.isdigit()]

        if final == "H" or final == "f":
            self.y = (args[0] - 1) if len(args) > 0 else 0
            self.x = (args[1] - 1) if len(args) > 1 else 0
            self.y = max(0, min(self.y, self.rows - 1))
            self.x = max(0, min(self.x, self.cols - 1))
        elif final == "J" and not priv:
            mode = args[0] if args else 0
            if mode == 2:
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
        elif final == "K" and not priv:
            for x in range(self.x, self.cols):
                self.grid[self.y][x] = " "

    def line(self, row):
        return "".join(self.grid[row]).rstrip()

    def selected_id(self):
        """The id on the row carrying the selection marker."""
        for row in range(self.rows):
            text = self.line(row)
            if "▸" in text:
                m = ID_RE.search(text)
                if m:
                    return row, m.group(1)
        return None, None

    def id_on(self, row):
        m = ID_RE.search(self.line(row))
        return m.group(1) if m else None


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


def pump(fd, screen, seconds=1.0):
    out = b""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if not r:
            continue
        try:
            chunk = os.read(fd, 1 << 16)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
        screen.feed(chunk)
    return out


def main():
    import termios

    pid, fd = spawn()
    screen = Screen(COLS, ROWS)
    boot = pump(fd, screen, 6.0)

    alt_in = boot.find(b"\x1b[?1049h")
    alt_out = boot.find(b"\x1b[?1049l")
    mouse_off = boot.find(b"\x1b[?1000l")
    lflag = termios.tcgetattr(fd)[3]
    raw = not (lflag & termios.ICANON)

    print("== terminal state while the loop is running ==")
    print(f"  alt screen entered   : {alt_in != -1}")
    print(f"  alt screen still held: {alt_out == -1}   (leaving it early kills clicks)")
    print(f"  mouse capture held   : {mouse_off == -1}")
    print(f"  raw mode active      : {raw}")
    setup_ok = alt_in != -1 and alt_out == -1 and mouse_off == -1 and raw
    print(f"  => {'PASS' if setup_ok else 'FAIL'}")

    # The agent-file prompt covers the list at startup and, like Go, eats
    # clicks while it is up. Dismiss it the way a user would.
    os.write(fd, b"\x1b")
    pump(fd, screen, 1.0)
    b_row, b_id = screen.selected_id()
    print(f"\n== clicks ==\n  selection before: row {b_row} id {b_id}")
    mismatches = []
    for target in (5, 9, 14, 20, 30):
        os.write(fd, sgr(LEFT, 20, target, True))
        os.write(fd, sgr(LEFT, 20, target, False))
        pump(fd, screen, 0.7)
        r, i = screen.selected_id()
        want = screen.id_on(target)
        ok = i is not None and i == want
        print(f"  click row {target:>2}: selected {i!s:<12} (row {r}) painted {want!s:<12} {'ok' if ok else 'MISMATCH'}")
        if not ok:
            mismatches.append(target)
    print(f"  => {'PASS' if not mismatches else 'FAIL ' + str(mismatches)}")

    print("\n== wheel ==")
    os.write(fd, sgr(LEFT, 20, 14, True))
    os.write(fd, sgr(LEFT, 20, 14, False))
    pump(fd, screen, 0.6)
    before, _ = screen.selected_id()
    t0 = time.time()
    for _ in range(10):
        os.write(fd, sgr(WHEEL_DOWN, 20, 14, True))
        os.write(fd, sgr(WHEEL_DOWN, 14 and 14, False))
        time.sleep(0.01)
    data = pump(fd, screen, 1.5)
    after, _ = screen.selected_id()
    print(f"  10 notches: selection row {before} -> {after}, {len(data)} bytes, {time.time()-t0:.2f}s")
    print(f"  => {'PASS' if after != before else 'FAIL (no movement)'}")

    os.write(fd, b"\x03")
    time.sleep(0.2)
    os.write(fd, b"q")
    pump(fd, screen, 0.8)
    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass


if __name__ == "__main__":
    main()
