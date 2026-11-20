import sys, time, struct, json, re
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
mode = sys.argv[1]  # "viewer" or "noviewer"
out = "/tmp/cm0/q_%s.json" % mode
c0 = Conn()
name = "q" + mode
try: c0.call("terminal.kill", {"terminal": name}); c0.call("terminal.rm", {"terminal": name})
except Exception: pass
c0.call("terminal.run", {"argv": ["python3", "/Users/jcarreira/projects/squashmerge/combine/spike/m0/fixtures/queries.py", out], "name": name, "no_parent": True, "size": {"cols": 80, "rows": 24}})
time.sleep(0.4)
disp = {"fg":"#ddeeff","bg":"#112233","cursor":"#aabbcc","cell":[9,18]}
A = Conn(); ra = A.attach(name, 80, 24, display=disp if mode=="viewer" else {}, observe=(mode!="viewer")); ch = ra["channel"]
data = b""
f = A.frame(2); A.credit(ch, len(f[2]))
if mode == "viewer":
    A.send(ch, DATA, b"g")
else:
    A.send(ch, CLOSE); time.sleep(0.3)
    c0.call("terminal.keys", {"terminal": name, "keys": ["g"]}) if False else None
    # type 'g' with a short-lived active attach, then close it before queries run
    B = Conn(); rb = B.attach(name, 80, 24); B.frame(2); B.send(rb["channel"], DATA, b"g"); B.send(rb["channel"], CLOSE); time.sleep(0.1); B.s.close()
end = time.time() + 40
while time.time() < end:
    f = A.frame(1.0) if mode == "viewer" else "timeout"
    if mode != "viewer":
        try:
            json.load(open(out)); break
        except Exception: time.sleep(0.5); continue
    if f == "timeout" or f is None: continue
    if f[1] == DATA:
        data += f[2]; A.credit(ch, len(f[2]))
        if b"QDONE" in data: break
res = json.load(open(out))
leaks = {}
for m in re.finditer(rb"<Q (\w+)>(.*?)</Q>", data, re.S):
    leaks[m.group(1).decode()] = m.group(2)
for k, v in res.items():
    print("%-14s reply=%-60r viewer_saw=%r" % (k, v, leaks.get(k, b"-")))
