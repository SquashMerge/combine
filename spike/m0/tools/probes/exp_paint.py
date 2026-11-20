import sys, time, re, struct
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
c = Conn()
# big main-screen terminal with history: flood 1 MB into a 200x60 terminal
try: c.call("terminal.kill", {"terminal": "big"}); c.call("terminal.rm", {"terminal": "big"})
except Exception: pass
c.call("terminal.run", {"argv": ["python3", "/Users/jcarreira/projects/squashmerge/combine/spike/m0/fixtures/flood.py", "1"], "name": "big", "no_parent": True, "size": {"cols": 200, "rows": 60}})
time.sleep(0.3)
a = c.attach("big", 200, 60); ch = a["channel"]; c.frame(2); c.credit(ch, 1<<30)
c.send(ch, DATA, b"g"); 
while c.frame(1.0) != "timeout": pass
c.send(ch, CLOSE)
time.sleep(0.3)
for hist in (0, 1000, 10000):
    c2 = Conn()
    a = c2.attach("big", 200, 60, history=hist); ch = a["channel"]
    frames = []; t0=time.time()
    while True:
        f = c2.frame(0.5)
        if f == "timeout": break
        frames.append((round((time.time()-t0)*1000,1), f[0], f[1], len(f[2]), f[2]))
    tot = sum(x[3] for x in frames)
    print("history", hist, "frames", [(x[0],x[2],x[3]) for x in frames][:12], "...", len(frames), "total", tot)
    allp = b"".join(x[4] for x in frames)
    print("   markers", allp.count(b"\x18\x1b[!p"), "ends with", repr(allp[-60:]))
    c2.credit(ch, 1<<30); c2.send(ch, CLOSE)
