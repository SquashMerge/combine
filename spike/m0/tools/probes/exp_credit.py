import sys, time, struct
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
c = Conn()
r = c.call("terminal.run", {"argv": ["python3", "/Users/jcarreira/projects/squashmerge/combine/spike/m0/fixtures/flood.py", "8"], "name": "flood1", "no_parent": True, "size": {"cols": 80, "rows": 24}})
print("run", r.get("address"), r.get("terminal", {}).get("address") if isinstance(r.get("terminal"), dict) else "")
time.sleep(0.5)
a = c.attach("flood1", 80, 24)
ch = a["channel"]; print("attach ch", ch, "resize_notifications", a["resize_notifications"])
f = c.frame(2); print("paint frame", f[0], f[1], len(f[2]))
c.send(ch, DATA, b"g")
got = 0; frames = []
t0 = time.time()
while True:
    f = c.frame(2.0)
    if f == "timeout": break
    if f is None: print("EOF"); break
    if f[0] == ch and f[1] == DATA:
        got += len(f[2]); frames.append(len(f[2]))
    else: print("other", f[0], f[1], f[2][:100])
print("without credit after paint: received", got, "bytes in", len(frames), "frames; max frame", max(frames) if frames else 0, "elapsed", round(time.time()-t0,2))
# grant 100000 bytes
c.credit(ch, 100000); got2 = 0
while True:
    f = c.frame(1.0)
    if f == "timeout": break
    got2 += len(f[2])
print("after CREDIT 100000: received", got2)
# now drain everything with generous credit and look for a repaint (CAN + DECSTR)
c.credit(ch, 1 << 30); got3 = 0; data = b""
while True:
    f = c.frame(2.0)
    if f == "timeout": break
    data += f[2]
print("after big credit: received", len(data), "bytes; repaint marker present:", b"\x18\x1b[!p" in data, "; DONE seen:", b"DONE" in data)
print("first 200 bytes after big credit:", repr(data[:200]))
print("tail:", repr(data[-150:]))
