#!/usr/bin/env python3
"""Capture the reCamera UART console with the FTDI adapter.

The board's console is ttyS0,115200 (from /proc/cmdline) and U-Boot uses
board=cv181x baudrate=115200, so this opens the port at 115200 8N1 raw and
writes every byte to the log until the deadline. A start marker is written
first so a boot can be timed and compared between runs.

Usage: uart-capture.py [device] [seconds] [logfile]
"""
import os
import sys
import time
import termios

DEV = sys.argv[1] if len(sys.argv) > 1 else "/dev/ttyUSB0"
SECS = float(sys.argv[2]) if len(sys.argv) > 2 else 900.0
LOG = sys.argv[3] if len(sys.argv) > 3 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "work", "serial", "uart-capture.log")
os.makedirs(os.path.dirname(os.path.abspath(LOG)), exist_ok=True)

fd = os.open(DEV, os.O_RDWR | os.O_NOCTTY)
iflag, oflag, cflag, lflag, ispeed, ospeed, cc = termios.tcgetattr(fd)
cc = list(cc)
cc[termios.VMIN] = 0
cc[termios.VTIME] = 1
termios.tcsetattr(fd, termios.TCSANOW, [
    0, 0, termios.CS8 | termios.CREAD | termios.CLOCAL, 0,
    termios.B115200, termios.B115200, cc])

t0 = time.time()
with open(LOG, "wb") as out:
    out.write(("# uart-capture %s 115200 8N1 start %s\n"
               % (DEV, time.strftime("%H:%M:%S"))).encode())
    out.flush()
    while time.time() - t0 < SECS:
        data = os.read(fd, 4096)
        if data:
            out.write(data)
            out.flush()
os.close(fd)
