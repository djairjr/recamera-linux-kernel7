#!/usr/bin/env python3
"""Send a line to the reCamera U-Boot / Linux console over the FTDI serial port.

Reads nothing: the passive capture (uart-capture.py) keeps the log, so this
only writes. Both processes share /dev/ttyUSB0 at 115200 8N1 raw.

Usage: uart-send.py "printenv bootcmd" [device]
"""
import os
import sys
import time
import termios

LINE = sys.argv[1]
DEV = sys.argv[2] if len(sys.argv) > 2 else "/dev/ttyUSB0"

fd = os.open(DEV, os.O_RDWR | os.O_NOCTTY)
_, _, _, _, _, _, cc = termios.tcgetattr(fd)
cc = list(cc)
cc[termios.VMIN] = 0
cc[termios.VTIME] = 1
termios.tcsetattr(fd, termios.TCSANOW, [
    0, 0, termios.CS8 | termios.CREAD | termios.CLOCAL, 0,
    termios.B115200, termios.B115200, cc])
os.write(fd, LINE.encode() + b"\r")
time.sleep(0.3)
os.write(fd, b"\n")
time.sleep(0.3)
os.close(fd)
print(f"enviado: {LINE!r}")
