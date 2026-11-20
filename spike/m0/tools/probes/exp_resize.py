import sys, time, struct, json
sys.path.insert(0, '/Users/jcarreira/projects/squashmerge/combine/spike/m0/tools')
from hcframe import *
def drain(c, ch, t=0.6, label=""):
    out=[]
    while True:
        f = c.frame(t)
        if f == "timeout" or f is None: break
        if f[1] == RESIZE and len(f[2])==4: out.append(("RESIZE", struct.unpack("<HH", f[2])))
        elif f[1] == DATA: out.append(("DATA", len(f[2]), f[2][:40], f[2].count(b"\x18\x1b[!p")))
        else: out.append((f[1], f[2][:80]))
        if f[1]==DATA: c.credit(ch, len(f[2]))
    print(label, out)
c0 = Conn()
try: c0.call("terminal.kill", {"terminal": "rz"}); c0.call("terminal.rm", {"terminal": "rz"})
except Exception: pass
# vim-like: a program that repaints on SIGWINCH: use the modes fixture? it ignores SIGWINCH. Use `sh -c 'trap "stty size" WINCH; while :; do sleep 1; done'`
c0.call("terminal.run", {"line": "trap 'printf \"[WINCH %s]\" \"$(stty size)\"' WINCH; printf ready; while :; do sleep 0.2; done", "name": "rz", "no_parent": True, "size": {"cols": 80, "rows": 24}})
time.sleep(0.5)
A = Conn(); ra = A.attach("rz", 80, 24, resize_notifications=True); cha = ra["channel"]
print("A granted resize_notifications:", ra["resize_notifications"], "automatic_input", ra["automatic_input"])
drain(A, cha, label="A paint")
B = Conn(); rb = B.attach("rz", 120, 40, resize_notifications=True, display={"fg":"#ffffff","bg":"#000000","cursor":"#ff0000","cell":[10,20]}); chb = rb["channel"]
print("B granted:", rb["resize_notifications"], "term size now", rb["terminal"]["cols"], rb["terminal"]["rows"])
drain(B, chb, label="B paint")
drain(A, cha, label="A after B attached")
print("size via get:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
# A focuses
A.send(cha, FOCUS, b"\x01"); time.sleep(0.5)
drain(A, cha, label="A after A FOCUS"); drain(B, chb, label="B after A FOCUS")
print("size via get:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
# A types
A.send(cha, DATA, b""); time.sleep(0.3)
# B resizes itself
B.send(chb, RESIZE, struct.pack("<HH", 132, 43)); time.sleep(0.5)
drain(A, cha, label="A after B RESIZE 132x43"); drain(B, chb, label="B after B RESIZE")
print("size via get:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
B.send(chb, FOCUS, b"\x01"); time.sleep(0.5)
drain(A, cha, label="A after B FOCUS"); drain(B, chb, label="B after B FOCUS")
print("size via get:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
# observer
O = Conn(); ro = O.attach("rz", 50, 10, observe=True, resize_notifications=True); cho = ro["channel"]
print("O granted:", ro["resize_notifications"]); drain(O, cho, label="O paint")
print("size via get after observer:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
# B closes -> who wins
B.send(chb, CLOSE); time.sleep(0.6)
drain(A, cha, label="A after B CLOSE"); drain(O, cho, label="O after B CLOSE")
print("size via get:", c0.call("terminal.get", {"terminal":"rz"})["cols"], c0.call("terminal.get", {"terminal":"rz"})["rows"])
# kill program while attached -> CLOSE frame?
c0.call("terminal.kill", {"terminal": "rz"}); time.sleep(0.5)
drain(A, cha, label="A after kill")
