import sys, time, struct, json
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
def drain(c, ch, t=0.6, label=""):
    out=[]
    while True:
        f = c.frame(t)
        if f == "timeout" or f is None: break
        if f[1] == RESIZE and len(f[2])==4: out.append(("RESIZE", struct.unpack("<HH", f[2])))
        elif f[1] == DATA: out.append(("DATA", len(f[2]), f[2][-60:] if f[2].count(b"\x18\x1b[!p")==0 else b"<paint>"))
        else: out.append((f[1], f[2][:80]))
        if f[1]==DATA: c.credit(ch, len(f[2]))
    print(label, out)
c0 = Conn()
for n in ("ob",):
    try: c0.call("terminal.kill", {"terminal": n}); c0.call("terminal.rm", {"terminal": n})
    except Exception: pass
c0.call("terminal.run", {"line": "stty raw -echo; trap 'printf \"[WINCH %s]\" \"$(stty size)\"' WINCH; while :; do c=$(dd bs=1 count=1 2>/dev/null); printf '<%s>' \"$c\"; done", "name": "ob", "no_parent": True, "size": {"cols": 80, "rows": 24}})
time.sleep(0.5)
O = Conn(); ro = O.attach("ob", 80, 24, observe=True); cho = ro["channel"]; drain(O, cho, label="O paint")
O.send(cho, DATA, b"a"); time.sleep(0.4); drain(O, cho, label="O after observer DATA 'a'")
O.send(cho, AUTO, b"b"); time.sleep(0.4); drain(O, cho, label="O after observer AUTOMATIC_INPUT 'b'")
O.send(cho, RESIZE, struct.pack("<HH", 90, 30)); time.sleep(0.4); drain(O, cho, label="O after observer RESIZE")
A = Conn(); ra = A.attach("ob", 100, 30); cha = ra["channel"]; drain(A, cha, label="A paint"); drain(O, cho, label="O after A attach")
A.send(cha, AUTO, b"c"); time.sleep(0.4); drain(A, cha, label="A after AUTOMATIC_INPUT 'c'")
A.send(cha, DATA, b"d"); time.sleep(0.4); drain(A, cha, label="A after DATA 'd'")
R = Conn(); rr = R.attach("ob", 70, 20, resumed=True); chr_ = rr["channel"]; drain(R, chr_, label="R(resumed 70x20) paint")
print("size after resumed attach:", c0.call("terminal.get", {"terminal":"ob"})["cols"], c0.call("terminal.get", {"terminal":"ob"})["rows"])
R.send(chr_, DATA, b"e"); time.sleep(0.4); drain(R, chr_, label="R after typing")
print("size after resumed types:", c0.call("terminal.get", {"terminal":"ob"})["cols"], c0.call("terminal.get", {"terminal":"ob"})["rows"])
drain(A, cha, label="A sees")
