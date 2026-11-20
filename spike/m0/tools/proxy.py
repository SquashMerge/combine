#!/usr/bin/env python3
"""Logging Unix-socket proxy: listen on LISTEN, forward to TARGET, log every chunk as hex.

usage: proxy.py LISTEN TARGET LOGFILE
Each line: <t_ms> <conn> <dir C>S|S>C> <len> <hex>
Refuses to proxy to the user's default herdr-core socket.
"""
import os, socket, sys, threading, time

listen, target, logpath = sys.argv[1:4]
if "/herdr-core/default/" in target:
    sys.exit("refusing the default herdr-core socket")
log = open(logpath, "w", buffering=1)
lock = threading.Lock()
t0 = time.time()


def pump(src, dst, conn, tag):
    try:
        while True:
            b = src.recv(65536)
            if not b:
                break
            with lock:
                log.write("%d %d %s %d %s\n" % ((time.time() - t0) * 1000, conn, tag, len(b), b.hex()))
            dst.sendall(b)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


if os.path.exists(listen):
    os.unlink(listen)
srv = socket.socket(socket.AF_UNIX)
srv.bind(listen)
os.chmod(listen, 0o600)
srv.listen(8)
n = 0
while True:
    c, _ = srv.accept()
    n += 1
    u = socket.socket(socket.AF_UNIX)
    u.connect(target)
    threading.Thread(target=pump, args=(c, u, n, "C>S"), daemon=True).start()
    threading.Thread(target=pump, args=(u, c, n, "S>C"), daemon=True).start()
