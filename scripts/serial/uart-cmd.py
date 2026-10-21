#!/usr/bin/env python3
"""Send one or more lines to the reCamera console and print only the new output.

Keeps a byte offset per log file in /tmp/uart-offset so successive calls show
only what arrived since the previous one. Reading is done through the passive
capture's log (uart-capture.py), so this never steals bytes from it.

Usage: uart-cmd.py "cmd1" ["cmd2" ...]
"""
import os
import sys
import time
import termios

DEV = "/dev/ttyUSB0"
LOG = open("/tmp/uart-atual.txt").read().strip()
OFF = "/tmp/uart-offset"


def open_port():
    fd = os.open(DEV, os.O_RDWR | os.O_NOCTTY)
    _, _, _, _, _, _, cc = termios.tcgetattr(fd)
    cc = list(cc)
    cc[termios.VMIN] = 0
    cc[termios.VTIME] = 1
    termios.tcsetattr(fd, termios.TCSANOW, [
        0, 0, termios.CS8 | termios.CREAD | termios.CLOCAL, 0,
        termios.B115200, termios.B115200, cc])
    return fd


def offset():
    try:
        return int(open(OFF).read().strip())
    except Exception:
        return 0


fd = open_port()
for cmd in sys.argv[1:]:
    os.write(fd, cmd.encode() + b"\r")
    time.sleep(0.4)
os.close(fd)
time.sleep(2.5)

b = open(LOG, "rb").read()
o = offset()
new = b[o:]
open(OFF, "w").write(str(len(b)))
body = new.split(b"\n", 1)[1] if b"\n" in new else new
sys.stdout.write(body.decode("latin1", "replace").replace("\r\n", "\n"))
