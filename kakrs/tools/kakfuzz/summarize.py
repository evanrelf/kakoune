#!/usr/bin/env python3
"""Show where each kakfuzz finding diverges: python3 summarize.py DIR/finding-*.json"""
import difflib, json, sys


def first_diff(a, b, ctx=60):
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op != "equal":
            return f"cpp: ...{a[max(0, i1 - ctx):i2 + ctx]!r}\n      rust: ...{b[max(0, j1 - ctx):j2 + ctx]!r}"
    return "(equal)"


for path in sys.argv[1:]:
    f = json.load(open(path))
    d = f.get("minimized_detail") or f["detail"]
    print(f"== {path}: {d['diff']} at case {d['index']} / {len(f['cases'])}")
    print("   last case:", json.dumps(d["case"], ensure_ascii=False)[:300])
    c, r = d["cpp"], d["rust"]
    if "status" in c or "status" in r:
        print("   cpp:", c.get("status", "ok"), c.get("stderr", "")[-300:])
        print("   rust:", r.get("status", "ok"), r.get("stderr", "")[-600:])
        continue
    if c["state"] != r["state"]:
        print("   state", first_diff(c["state"], r["state"]))
    else:
        for key in sorted(set(c["ui"]) | set(r["ui"])):
            a, b = json.dumps(c["ui"].get(key), ensure_ascii=False), json.dumps(r["ui"].get(key), ensure_ascii=False)
            if a != b:
                print(f"   ui[{key}]", first_diff(a, b))
