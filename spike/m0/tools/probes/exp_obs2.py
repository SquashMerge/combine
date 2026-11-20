import sys, time, struct
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
def drain(c, ch, t=0.5, label=""):
    out=[]
    while True:
        f = c.frame(t)
        if f == "timeout" or f is None: break
        if f[1] == RESIZE: out.append(("RESIZE", struct.unpack("<HH", f[2])))
        elif f[1] == DATA:
            n = f[2].count(b"\x18\x1b[!p")
            out.append(("PAINT" if n else "DATA", len(f[2]), None if n else f[2][-30:]))
            c.credit(ch, len(f[2]))
        else: out.append((f[1], f[2][:60]))
    print(label, out)
c0 = Conn()
c0.call("terminal.run", {"argv": ["vim", "-u", "NONE", "-N", "+set nocompatible"], "name": "ob2", "no_parent": True, "size": {"cols": 80, "rows": 24}})
time.sleep(1)
O = Conn(); ro = O.attach("ob2", 80, 24, observe=True, resize_notifications=True); cho = ro["channel"]; drain(O, cho, label="O attach")
A = Conn(); ra = A.attach("ob2", 100, 30); cha = ra["channel"]; drain(A, cha, label="A attach 100x30"); drain(O, cho, label="O after A attach")
A.resize(cha, 120, 40); time.sleep(0.4); drain(A, cha, label="A after own resize 120x40"); drain(O, cho, label="O after A resize")
A.send(cha, DATA, b"ihello"); time.sleep(0.4); drain(A, cha, label="A typing"); drain(O, cho, label="O sees typing")
B = Conn(); rb = B.attach("ob2", 90, 20); chb = rb["channel"]; drain(B, chb, label="B attach 90x20"); drain(A, cha, label="A after B attach"); drain(O, cho, label="O after B attach")
A.send(cha, FOCUS, b"\x01"); time.sleep(0.4); drain(A, cha, label="A after A focus"); drain(O, cho, label="O after A focus")
c0.call("terminal.kill", {"terminal": "ob2"}); c0.call("terminal.rm", {"terminal": "ob2"})
