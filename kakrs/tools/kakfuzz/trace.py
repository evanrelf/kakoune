#!/usr/bin/env python3
"""Replay cases on both implementations, printing the state after each:
python3 trace.py FINDING.json [extra case as JSON]..."""
import json, os, re, shutil, sys, tempfile, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import kakfuzz

finding = json.load(open(sys.argv[1]))
cases = finding["cases"] + [json.loads(c) for c in sys.argv[2:]]
tmp = tempfile.mkdtemp(prefix="kftrace")
kaks = {impl: kakfuzz.Kak(impl, os.path.join(tmp, impl[0]), finding["files"], finding.get("norc", False))
        for impl in kakfuzz.BINARIES}
try:
    for k in kaks.values():
        k.wait_startup()
        k.send({"jsonrpc": "2.0", "method": "resize", "params": [24, 80]})
    for i, case in enumerate([["<esc>"]] + cases):
        res = {impl: k.step(case) for impl, k in kaks.items()}
        print(f"--- {i - 1}: {json.dumps(case, ensure_ascii=False)}")
        for impl, r in res.items():
            state = r if isinstance(r, str) else r[0].decode(errors="replace").split("\n--content--\n")[0]
            print(f"  {impl:4} {state[:220]}")
            if r == "DEAD":
                time.sleep(0.2)
                err = kaks[impl].stderr.decode(errors="replace")
                m = re.search(r"panicked at [^\n]*\n[^\n]*", err)
                frames = re.findall(r"\d+: (kak::[^\n]*)", err)
                print("      ", m.group(0) if m else err[:300])
                print("      ", " <- ".join(frames[1:9]))
finally:
    for k in kaks.values():
        k.close()
    shutil.rmtree(tmp, ignore_errors=True)
