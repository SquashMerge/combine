#!/usr/bin/env python3
"""Query fixture: on 'g', send each terminal query and record what comes back (1 s each).

Writes results as JSON to argv[1]. Each query is wrapped in markers "<Q name>" and
"</Q>" on the output so a viewer can see whether the query bytes themselves reach it.
"""
import json, os, select, sys, termios, tty

E = "\x1b"
ST = E + "\\"
QUERIES = [
    ("DA1", E + "[c"), ("DA2", E + "[>c"), ("DA3", E + "[=c"),
    ("DSR5", E + "[5n"), ("CPR", E + "[6n"), ("DECXCPR", E + "[?6n"),
    ("OSC10", E + "]10;?" + ST), ("OSC11", E + "]11;?" + ST), ("OSC12", E + "]12;?" + ST),
    ("OSC4_1", E + "]4;1;?" + ST), ("OSC4_200", E + "]4;200;?" + ST),
    ("WIN14t", E + "[14t"), ("WIN16t", E + "[16t"), ("WIN18t", E + "[18t"), ("WIN19t", E + "[19t"),
    ("TITLE21t", E + "[21t"),
    ("XTVERSION", E + "[>0q"),
    ("DECRQM2004", E + "[?2004$p"), ("DECRQM1049", E + "[?1049$p"), ("DECRQM_IRM", E + "[4$p"),
    ("KITTYKBD", E + "[?u"),
    ("DECRQSS_SGR", E + "P$qm" + ST), ("DECRQSS_STBM", E + "P$qr" + ST),
    ("XTGETTCAP_TN", E + "P+q544e" + ST),
    ("OSC52_read", E + "]52;c;?" + ST),
    ("ENQ", "\x05"),
    ("DECREQTPARM", E + "[x"),
    ("KITTYGFX", E + "_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA" + ST),
]

outpath = sys.argv[1]
fd = sys.stdin.fileno()
tty.setraw(fd)


def read_for(t):
    buf = b""
    while True:
        r, _, _ = select.select([fd], [], [], t)
        if not r:
            return buf
        buf += os.read(fd, 4096)
        t = 0.15  # after the first reply byte, wait only briefly for the rest


while True:
    b = os.read(fd, 16)
    if b"g" in b:
        break
res = {}
for name, q in QUERIES:
    os.write(1, ("<Q %s>" % name).encode())
    os.write(1, q.encode())
    os.write(1, b"</Q>\r\n")
    res[name] = read_for(1.0).decode("latin-1")
json.dump(res, open(outpath, "w"), indent=1)
os.write(1, b"QDONE\r\n")
while True:
    if not os.read(fd, 16):
        break
