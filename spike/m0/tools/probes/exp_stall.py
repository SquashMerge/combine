import sys, time, re
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
mb, stall, name = float(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
c = Conn()
try: c.call("terminal.kill", {"terminal": name}); c.call("terminal.rm", {"terminal": name})
except Exception: pass
c.call("terminal.run", {"argv": ["python3", "/Users/jcarreira/projects/squashmerge/combine/spike/m0/fixtures/flood.py", str(mb)], "name": name, "no_parent": True, "size": {"cols": 80, "rows": 24}})
time.sleep(0.5)
a = c.attach(name, 80, 24); ch = a["channel"]
segs = []  # (t, credit_phase, bytes)
f = c.frame(2); paint = f[2]
c.send(ch, DATA, b"g")
phase = "nocredit"; data = []
t0 = time.time()
granted = False
while True:
    f = c.frame(0.5 if granted else stall)
    if f == "timeout":
        if not granted:
            granted = True; phase = "credit"; c.credit(ch, 1 << 30); continue
        break
    if f is None: break
    if f[0] == ch and f[1] == DATA: data.append((phase, f[2]))
    else: print("non-data frame", f[0], f[1], f[2][:80])
allb = b"".join(d for _, d in data)
print("paint", len(paint), "data", len(allb), "expected", int(mb*1024*1024/64)*64 + len(b"DONE %d\r\n" % int(mb*1024*1024/64)))
# find discontinuities
nums = [(m.start(), int(m.group(1))) for m in re.finditer(rb"L(\d{10}) ", allb)]
prev = None
for pos, n in nums:
    if prev is not None and n != prev + 1:
        print("jump at byte", pos, ":", prev, "->", n, "context:", repr(allb[max(0,pos-90):pos+20]))
    prev = n
print("first line", nums[0][1] if nums else None, "last", prev)
print("CAN/DECSTR in data:", b"\x18" in allb, b"\x1b[!p" in allb, "other ESC:", allb.count(b"\x1b"))
i = allb.find(b"\x1b")
if i >= 0: print("first ESC context", repr(allb[max(0,i-40):i+200]))
marks = [m.start() for m in re.finditer(rb"\x18\x1b\[!p", allb)]
print("repaint markers at", marks)
# lines delivered after each marker until next marker
for i, m in enumerate(marks):
    end = marks[i+1] if i+1 < len(marks) else len(allb)
    print(" segment after marker", i, "bytes", end - m)
