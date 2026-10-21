#!/usr/bin/env python3
"""kakfuzz: stateful differential fuzzing of the Rust port against C++ Kakoune.

Each run starts one C++ and one Rust `kak -ui json` process (with the full
kakrc), keeps them alive, and feeds both the same sequence of cases. After
every case the harness sends `<esc><F12>`: a RawKey hook dumps the editor
state (selections, buffer content, registers...) to files and signals a
fifo. The harness compares the two dumps and the JSON UI output (draws,
menus, info boxes, status line) produced by the case.

Runs use swarm testing: each enables a random subset of generator categories
(see gen.py). On a mismatch, the case list is replayed in fresh processes and
delta debugged (cases, then keys) down to a minimal reproduction, written to
the output directory as JSON.

Usage:
    python3 tools/kakfuzz/kakfuzz.py [--runs N] [--cases N] [--jobs N] [--seed S] [--out DIR]
    python3 tools/kakfuzz/kakfuzz.py --replay DIR/finding-*.json
"""
import argparse, json, os, queue, random, re, shutil, signal, subprocess, sys, tempfile, threading, time
from concurrent.futures import ProcessPoolExecutor, as_completed

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
BINARIES = {"cpp": os.path.join(REPO, "src", "kak"),
            "rust": os.path.join(REPO, "kakrs", "target", "release", "kak")}
RUNTIME = os.path.join(REPO, "share", "kak")
CASE_TIMEOUT = 5.0

# The only commands the editors can run: shell completion (`!<tab>`) can pick
# any command on the PATH and the next case may run it, so the PATH is a
# directory of links to these instead of the user's PATH.
SAFE_COMMANDS = ["sh", "cat", "sort", "tr", "sed", "printf", "wc", "head", "tail", "awk", "true", "false",
                 "echo", "grep", "cut", "basename", "dirname", "expr", "mktemp", "git", "env", "uniq", "rev", "fold",
                 "find", "ls"]  # kakrc needs find and ls (autoload, colorschemes)

INIT = r"""
declare-option -hidden int fuzz_counter 0
define-command -hidden fuzz-dump %{
    set-option -add global fuzz_counter 1
    echo -to-file %{STATE} -quoting kakoune -- %val{selections_desc} %val{bufname} %val{buflist} %val{modified} %val{history_id} %val{selection_count} %reg{dquote} %reg{slash} %reg{arobase} %reg{a} %reg{caret} %reg{pipe} %opt{tabstop} %opt{indentwidth} %opt{filetype} %opt{eolformat} %val{window_range} %val{cursor_display_column}
    evaluate-commands -draft %{ execute-keys '%' ; echo -to-file %{CONTENT} -- %val{selection} }
    echo -to-file %{FIFO} done
    echo -markup "{Information}fuzz %opt{fuzz_counter}"
}
hook -group fuzz-internal global RawKey <F12> fuzz-dump
set-option global idle_timeout 100000000
set-option global fs_check_timeout 100000000
"""


class Kak:
    """A kak -ui json process driven over stdin, with a state dump fifo"""

    def __init__(self, impl, workdir, files, norc=False):
        self.impl = impl
        self.dir = workdir
        os.makedirs(os.path.join(workdir, "cfg"), exist_ok=True)
        for name, content in files.items():
            with open(os.path.join(workdir, name), "w") as f:
                f.write(content)
        self.fifo = os.path.join(workdir, "fifo")
        self.state = os.path.join(workdir, "state")
        self.content = os.path.join(workdir, "content")
        os.mkfifo(self.fifo)
        bindir = os.path.join(workdir, "bin")
        os.makedirs(bindir)
        for cmd in SAFE_COMMANDS:
            target = shutil.which(cmd)
            if target:
                os.symlink(target, os.path.join(bindir, cmd))
        init = INIT.replace("%{STATE}", self.state).replace("%{CONTENT}", self.content).replace("%{FIFO}", self.fifo)
        # TMPDIR stays the default: session socket paths must stay short
        env = dict(os.environ, KAKOUNE_CONFIG_DIR=os.path.join(workdir, "cfg"), KAKOUNE_RUNTIME=RUNTIME,
                   HOME=workdir, KAKOUNE_POSIX_SHELL="/bin/sh", PATH=bindir)
        self.session = f"kf{os.getpid()}{impl}{random.randrange(1 << 30)}"
        # the user's config is never loaded (empty KAKOUNE_CONFIG_DIR); norc
        # also skips the bundled kakrc (-n)
        self.proc = subprocess.Popen([BINARIES[impl]] + (["-n"] if norc else []) +
                                     ["-ui", "json", "-s", self.session, "-e", init, "a.txt"],
                                     cwd=workdir, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, start_new_session=True)
        self.out = queue.Queue()
        self.dumps = queue.Queue()
        self.stderr = b""
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_fifo, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()
        self.counter = 0
        self.ui = {}

    def _read_stdout(self):
        for line in self.proc.stdout:
            self.out.put(line)
        self.out.put(None)

    def _read_stderr(self):
        self.stderr = self.proc.stderr.read()

    def _read_fifo(self):
        while True:
            try:
                with open(self.fifo, "rb") as f:
                    f.read()
            except OSError:
                return
            self.dumps.put(True)

    def wait_startup(self):
        """Skip the output of the startup draws"""
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                line = self.out.get(timeout=max(0.01, deadline - time.time()))
            except queue.Empty:
                break
            if line is None or b'"refresh"' in line:
                break

    def send(self, *msgs):
        """Send messages in a single write, so they are read in one go"""
        try:
            self.proc.stdin.write("".join(json.dumps(m) + "\n" for m in msgs).encode())
            self.proc.stdin.flush()
            return True
        except (BrokenPipeError, OSError):
            return False

    def step(self, case):
        """Run one case, returns (state, json output messages) or a failure string"""
        msgs = []
        if isinstance(case, dict):
            msgs.append({"jsonrpc": "2.0", **case})
            keys = ["<esc>", "<F12>"]
        else:
            keys = list(case) + ["<esc>", "<F12>"]
        msgs.append({"jsonrpc": "2.0", "method": "keys", "params": keys})
        if not self.send(*msgs):
            return "DEAD"
        self.counter += 1
        deadline = time.time() + CASE_TIMEOUT
        try:
            self.dumps.get(timeout=CASE_TIMEOUT)
        except queue.Empty:
            return "DEAD" if self.proc.poll() is not None else "TIMEOUT"
        state = self.normalize(open(self.state, "rb").read() + b"\n--content--\n" + open(self.content, "rb").read())
        # the dump's echo makes the following redraw end with this status
        marker = f"fuzz {self.counter}".encode()
        messages = []
        seen_marker = False
        while True:
            try:
                line = self.out.get(timeout=max(0.01, deadline - time.time()))
            except queue.Empty:
                return "TIMEOUT"
            if line is None:
                return "DEAD"
            line = self.normalize(line)
            messages.append(line)
            if b'"draw_status"' in line and marker in line:
                seen_marker = True
            elif seen_marker and b'"refresh"' in line:
                self.ui = fold_ui(self.ui, messages)
                return state, json.dumps(self.ui, sort_keys=True, ensure_ascii=False).encode()

    def normalize(self, data):
        data = data.replace(os.path.realpath(self.dir).encode(), b"WORK").replace(self.dir.encode(), b"WORK")
        return data.replace(self.session.encode(), b"SESSION")

    def close(self):
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except OSError:  # already gone
            pass
        self.proc.wait()
        # unblock the fifo reader thread
        try:
            fd = os.open(self.fifo, os.O_WRONLY | os.O_NONBLOCK)
            os.close(fd)
        except OSError:
            pass

    def crash_info(self):
        code = self.proc.poll()
        return {"exit": code, "stderr": self.stderr[-2000:].decode(errors="replace")}


def fold_ui(ui, messages):
    """The UI state after applying the JSON-RPC calls: comparing it rather
    than the message sequence ignores how input batches were split into
    redraws, which depends on timing"""
    ui = dict(ui)
    for line in messages:
        try:
            msg = json.loads(line)
        except ValueError:
            ui.setdefault("garbage", []).append(line.decode(errors="replace"))
            continue
        method, params = msg.get("method"), msg.get("params")
        if method == "draw":
            ui["draw"] = params
        elif method == "draw_status":
            ui["status"] = params
        elif method == "menu_show":
            ui["menu"] = params
            ui["menu_selected"] = -1
        elif method == "menu_select":
            ui["menu_selected"] = params
        elif method == "menu_hide":
            ui.pop("menu", None)
            ui.pop("menu_selected", None)
        elif method == "info_show":
            ui["info"] = params
        elif method == "info_hide":
            ui.pop("info", None)
        elif method in ("set_ui_options", "set_cursor"):
            ui[method] = params
        elif method != "refresh":
            ui.setdefault("other", []).append(method)
    return ui


def initial_files(r):
    return {name: gen.text(r) for name in gen.FILES}


def compare(results):
    """Compare the per implementation results of one case, returns None or a description"""
    a, b = results["cpp"], results["rust"]
    if a == b:
        return None
    if isinstance(a, str) or isinstance(b, str):
        return f"cpp: {a if isinstance(a, str) else 'ok'}, rust: {b if isinstance(b, str) else 'ok'}"
    if a[0] != b[0]:
        return "state differs"
    return "ui output differs"


def execute(files, cases, norc=False):
    """Run cases in a fresh pair of processes. Returns (index of first mismatch or None, detail)"""
    tmp = tempfile.mkdtemp(prefix="kakfuzz")
    kaks = {}
    try:
        for impl in BINARIES:
            # same length directory names: paths end up in buffers (e.g. the ^
            # register), and coordinates after them must not differ
            kaks[impl] = Kak(impl, os.path.join(tmp, impl[0]), files, norc)
        for k in kaks.values():
            k.wait_startup()
            k.send({"jsonrpc": "2.0", "method": "resize", "params": [24, 80]})
        results = {impl: k.step(["<esc>"]) for impl, k in kaks.items()}
        if compare(results):
            return -1, describe(-1, None, results, kaks)
        for i, case in enumerate(cases):
            results = {impl: k.step(case) for impl, k in kaks.items()}
            diff = compare(results)
            if diff:
                return i, describe(i, case, results, kaks, diff)
            if any(isinstance(v, str) for v in results.values()):
                # both died or hung the same way: faithful, stop this run
                return None, {"both": results["cpp"], "case": i}
        return None, None
    finally:
        for k in kaks.values():
            k.close()
        shutil.rmtree(tmp, ignore_errors=True)


def describe(i, case, results, kaks, diff="startup differs"):
    d = {"index": i, "case": case, "diff": diff}
    for impl, res in results.items():
        if isinstance(res, str):
            d[impl] = {"status": res, **kaks[impl].crash_info()}
        else:
            d[impl] = {"state": res[0].decode(errors="replace"), "ui": json.loads(res[1])}
    return d


def mismatches(files, cases, norc=False):
    idx, detail = execute(files, cases, norc)
    return idx is not None


def ddmin(items, test):
    """Delta debugging: a minimal sublist of items for which test still holds"""
    n = 2
    while len(items) >= 2:
        chunk = max(1, len(items) // n)
        reduced = False
        for start in range(0, len(items), chunk):
            candidate = items[:start] + items[start + chunk:]
            if candidate and test(candidate):
                items, n, reduced = candidate, max(n - 1, 2), True
                break
        if not reduced:
            if chunk == 1:
                break
            n = min(len(items), n * 2)
    return items


def minimize(files, cases, upto, norc=False):
    cases = cases[:upto + 1]
    cases = ddmin(cases, lambda c: mismatches(files, c, norc))
    # then the keys of each key case
    for i in range(len(cases)):
        if isinstance(cases[i], list) and len(cases[i]) > 1:
            def test(keys, i=i):
                trial = cases[:i] + [keys] + cases[i + 1:]
                return mismatches(files, trial, norc)
            cases[i] = ddmin(cases[i], test)
    # and the initial file contents
    for name in list(files):
        if files[name] and mismatches({**files, name: ""}, cases, norc):
            files = {**files, name: ""}
    return files, cases


def minimize_finding(path):
    """Check that a finding reproduces in fresh processes, then minimize it in place"""
    finding = json.load(open(path))
    files, cases, norc = finding["files"], finding["cases"], finding.get("norc", False)
    hits = sum(execute(files, cases, norc)[0] is not None for _ in range(3))
    if hits == 0:
        return "does not reproduce (flaky)"
    idx, _ = execute(files, cases, norc)
    if idx is None or idx < 0:
        return f"reproduces {hits}/3 but not on the 4th run"
    mfiles, mcases = minimize(files, cases, idx, norc)
    _, mdetail = execute(mfiles, mcases, norc)
    finding.update(files=mfiles, cases=mcases, minimized_detail=mdetail, reproduced=f"{hits}/3")
    with open(path, "w") as f:
        json.dump(finding, f, indent=1, ensure_ascii=False)
    return f"minimized to {len(mcases)} cases, {sum(len(c) if isinstance(c, list) else 1 for c in mcases)} keys (reproduced {hits}/3)"


def run(seed, ncases, outdir, do_minimize):
    r = random.Random(seed)
    swarm = gen.Swarm(r)
    norc = r.random() < 0.3
    files = initial_files(r)
    cases = [swarm.case(r) for _ in range(ncases)]
    t0 = time.time()
    idx, detail = execute(files, cases, norc)
    elapsed = time.time() - t0
    if idx is None:
        return {"seed": seed, "swarm": swarm.enabled, "ok": True, "elapsed": elapsed, "note": detail}
    finding = {"seed": seed, "swarm": swarm.enabled, "norc": norc, "detail": detail}
    if do_minimize and idx >= 0:
        mfiles, mcases = minimize(files, cases, idx, norc)
        _, mdetail = execute(mfiles, mcases, norc)
        finding.update(files=mfiles, cases=mcases, minimized_detail=mdetail)
    else:
        finding.update(files=files, cases=cases[:idx + 1])
    path = os.path.join(outdir, f"finding-{seed}.json")
    with open(path, "w") as f:
        json.dump(finding, f, indent=1, ensure_ascii=False)
    return {"seed": seed, "swarm": swarm.enabled, "ok": False, "path": path, "elapsed": elapsed,
            "diff": detail.get("diff") if detail else None}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=20)
    ap.add_argument("--cases", type=int, default=200)
    ap.add_argument("--jobs", type=int, default=os.cpu_count() // 2)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default="kakfuzz-out")
    ap.add_argument("--no-minimize", action="store_true")
    ap.add_argument("--replay")
    ap.add_argument("--minimize", nargs="+", metavar="FINDING", help="minimize existing findings (in parallel)")
    args = ap.parse_args()

    if args.minimize:
        with ProcessPoolExecutor(args.jobs) as ex:
            for path, res in zip(args.minimize, ex.map(minimize_finding, args.minimize)):
                print(f"{path}: {res}", flush=True)
        return

    if args.replay:
        finding = json.load(open(args.replay))
        idx, detail = execute(finding["files"], finding["cases"], finding.get("norc", False))
        print(json.dumps({"index": idx, "detail": detail}, indent=1, ensure_ascii=False))
        return

    os.makedirs(args.out, exist_ok=True)
    findings = 0
    total_cases = 0
    t0 = time.time()
    with ProcessPoolExecutor(args.jobs) as ex:
        futures = [ex.submit(run, args.seed + i, args.cases, args.out, not args.no_minimize) for i in range(args.runs)]
        for fut in as_completed(futures):
            res = fut.result()
            total_cases += args.cases
            if not res["ok"]:
                findings += 1
                print(f"FINDING seed={res['seed']} {res['diff']} -> {res['path']}  swarm={res['swarm']}", flush=True)
            elif res.get("note"):
                print(f"note seed={res['seed']}: both {res['note']['both']} at case {res['note']['case']}", flush=True)
    print(f"done: {args.runs} runs x {args.cases} cases in {time.time() - t0:.0f}s, {findings} findings")


if __name__ == "__main__":
    main()
