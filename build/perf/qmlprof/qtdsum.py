import sys, re, collections, xml.etree.ElementTree as ET
RANGE_TYPES = {"Binding","Javascript","HandlingSignal","Creating","Compiling"}
def load(fn):
    root = ET.parse(fn).getroot()
    ev = {}
    for e in root.iter("event"):
        t = e.findtext("type")
        f = (e.findtext("filename") or "").replace("qrc:/qt/qml/","").replace("file:///usr/lib/qt6/qml/","")
        ev[e.get("index")] = (t, f"{f}:{e.findtext('line')}", e.findtext("displayname"), e.findtext("details") or "")
    rs = []
    for r in root.iter("range"):
        d = r.get("duration")
        if d is None: continue
        i = r.get("eventIndex")
        if ev.get(i, ("",))[0] in RANGE_TYPES:
            rs.append((int(r.get("startTime")), int(d), i))
    return root, ev, rs
def summarize(fn, top=25):
    root, ev, rs = load(fn)
    rs.sort(key=lambda x: (x[0], -x[1]))
    self_t = collections.Counter(); incl = collections.Counter(); cnt = collections.Counter()
    bytype = collections.Counter()
    stack = []  # (end, idx, childsum)
    toplevel = 0
    def pop():
        end, i, st, dur, ch = stack.pop()
        s = dur - ch
        self_t[i] += s; bytype[ev[i][0]] += s
    for st, dur, i in rs:
        while stack and stack[-1][0] <= st: pop()
        if stack: stack[-1][4] += dur
        else: toplevel += dur
        incl[i] += dur; cnt[i] += 1
        stack.append([st+dur, i, st, dur, 0])
    while stack: pop()
    print(f"== {fn.split('/')[-1]}: top-level QML time {toplevel/1e6:.1f} ms; self by type: " +
          ", ".join(f"{k} {v/1e6:.1f}" for k, v in bytype.most_common()))
    print("  self ms  incl ms   n  type            location / details")
    for i, s in self_t.most_common(top):
        t, loc, dn, det = ev[i]
        print(f"  {s/1e6:7.1f}  {incl[i]/1e6:7.1f} {cnt[i]:4d}  {t:15s} {loc}  {det[:60]!r}")
for fn in sys.argv[1:]: summarize(fn)
