#!/usr/bin/env python3
"""Write RAW bytes to the reCamera console over the FTDI serial port.

uart-send.py writes a line and sleeps; this writes exactly the bytes asked for,
without echoing them back (so a password never lands in the caller's output),
and without relying on pyserial, which is not installed here.

Reads nothing: the passive capture (uart-capture.py) keeps the log.

Usage:
    uart-raw.py 0d                 # a bare CR
    uart-raw.py 03                 # Ctrl-C
    uart-raw.py --str 'root' 0d    # text plus a CR
    uart-raw.py --file /tmp/pw 0d  # text read from a 600 file, never printed
"""
import os
import sys
import time
import termios

DEV = os.environ.get("UART_DEV", "/dev/ttyUSB0")
BAUD = getattr(termios, "B115200")

chunks = []
args = sys.argv[1:]
i = 0
while i < len(args):
    a = args[i]
    if a == "--str":
        i += 1
        chunks.append(args[i].encode())
    elif a == "--file":
        i += 1
        with open(args[i], "rb") as fh:
            chunks.append(fh.read().strip())
    elif a == "--wait":
        i += 1
        chunks.append(("__WAIT__" + args[i]).encode())
    else:
        chunks.append(bytes.fromhex(a))
    i += 1

fd = os.open(DEV, os.O_RDWR | os.O_NOCTTY)
_, _, _, _, _, _, cc = termios.tcgetattr(fd)
cc = list(cc)
cc[termios.VMIN] = 0
cc[termios.VTIME] = 1
termios.tcsetattr(fd, termios.TCSANOW, [
    0, 0, termios.CS8 | termios.CREAD | termios.CLOCAL, 0,
    BAUD, BAUD, cc])

for c in chunks:
    if c.startswith(b"__WAIT__"):
        time.sleep(float(c[8:]))
        continue
    os.write(fd, c)
    time.sleep(0.3)

os.close(fd)
print("bytes enviados:", len(chunks))
