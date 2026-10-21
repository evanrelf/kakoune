"""Differential fuzzer: random normal mode key sequences through the C++ and
Rust kak, comparing the resulting buffer, selections, error and exit status.

Build both first (`make -C src` and `cargo build --release`), then:

    python3 tools/difffuzz.py <seed> <count> [jobs]

Mismatches are printed with their seed, text and keys. Known divergences
(C++ undefined behaviour) are listed in UPSTREAM-QUIRKS.md."""
import os, random, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CPP = os.path.join(REPO, "src", "kak")
RUST = os.path.join(REPO, "kakrs", "target", "release", "kak")
# an empty config directory, so the user's kakrc is not loaded
ENV = dict(os.environ, KAKOUNE_CONFIG_DIR=tempfile.mkdtemp(prefix="difffuzz-cfg"),
           KAKOUNE_RUNTIME=os.path.join(REPO, "share", "kak"))

WORDS = ["foo", "bar", "baz", "(x)", "[y, z]", "{a; b}", '"str"', "'c'", "42", "-3.5",
         "é", "日本", "😀", "\t", "  ", "foo_bar", "a.b", "<t>", "x=1", "#", "//", "é"]

def random_text(r):
    lines = []
    for _ in range(r.randint(1, 8)):
        lines.append((" " * r.choice([0, 0, 2, 4])) + " ".join(r.choice(WORDS) for _ in range(r.randint(0, 7))))
    return "\n".join(lines) + ("\n" if r.random() < 0.9 else "")

def ins_text(r):
    return r.choice(["x", "hello", "a b", "(", ")", "é", "\t", "<ret>", "foo<ret>bar", "😀", "<c-r>\"", ""])

def regex(r):
    return r.choice(["\\w+", "o", "foo|bar", "\\d+", "\\s", "[()]", "^", "$", ".", "\\b", "(?<=a)b", "é", "[^ ]+", "x*", "\\n", "\\h+", "(\\w)(\\w)", "\\Q.\\E"])

def object_key(r):
    return r.choice(list("bBrawWspinuqQg") + ["<a-w>", "<space>", "(", ")", "{", "}", "[", "]", "<", ">", '"', "'", "`", ",", "c\\(,\\)<ret>"])

SIMPLE = (list("hjklHJKLwbeWBExX%;~`<>&_C+)(nN*uUyRpP")
          + ["<a-w>", "<a-b>", "<a-e>", "<a-W>", "<a-B>", "<a-E>", "<a-l>", "<a-h>", "<a-L>", "<a-H>",
             "<a-x>", "<a-s>", "<a-S>", "<a-;>", "<a-:>", "<a-_>", "<a-+>", "<a-j>", "<a-J>", "<a-)>", "<a-(>",
             "<a-C>", "<a-&>", "<a-`>", "<a-n>", "<a-N>", "<a-*>", "<a-p>", "<a-P>", "<a-R>", "<a-u>", "<a-U>",
             "<a-,>", ",", "<a-o>", "<a-O>", "<a-d>", "d", "<a-.>", "@", "<a-@>", "<a-<>", "<a->>", "<c-o>", "<c-i>", "<c-s>",
             "m", "M", "<a-m>", "<a-M>", "<a-k>", "<a-K>", "Z", "z", "<a-z>a", "<a-Z>u"])

def command(r):
    k = r.random()
    count = str(r.randint(0, 3)) if r.random() < 0.15 else ""
    reg = '"' + r.choice("abc^/") if r.random() < 0.05 else ""
    if k < 0.45:
        key = r.choice(SIMPLE)
        if key in ("<a-k>", "<a-K>"):
            key += regex(r) + "<ret>"
        return reg + count + key
    if k < 0.55:
        return count + r.choice(["g", "G"]) + r.choice("hjklgkeitbc.")
    if k < 0.65:
        return count + r.choice(["f", "t", "F", "T", "<a-f>", "<a-t>"]) + r.choice(list("o(a ;é") + ["<space>"])
    if k < 0.75:
        return count + r.choice(["<a-i>", "<a-a>", "[", "]", "{", "}", "<a-[>", "<a-]>", "<a-I>", "<a-A>"]) + object_key(r)
    if k < 0.85:
        return count + r.choice(["s", "S", "/", "?", "<a-/>", "<a-?>"]) + regex(r) + "<ret>"
    if k < 0.95:
        return count + r.choice(["i", "a", "I", "A", "o", "O", "c", "<a-c>"]) + ins_text(r) + "<esc>"
    return r.choice(["Qwd<esc>Q", "q", "r" + r.choice("x é"), ".", "|sort<ret>", "!echo hi<ret>", "<a-|>true<ret>", "$true<ret>", "<a-!>printf x<ret>"])

def run(binary, text, keys, workdir, ext=".txt", kakrc=False, retry=True):
    path = os.path.join(workdir, "in" + ext)
    with open(path, "w") as f:
        f.write(text)
    res = os.path.join(workdir, "res")
    if os.path.exists(res):
        os.remove(res)
    script = ("try %{ execute-keys -with-maps %{" + keys + "} } catch %{ echo -to-file " + res + ".err %val{error} }; "
              "echo -to-file " + res + " %val{selections_desc} %val{main_reg_dquote}; "
              "write -force " + path + ".out; quit!")
    for f in (res + ".err", path + ".out"):
        if os.path.exists(f):
            os.remove(f)
    try:
        p = subprocess.run([binary] + ([] if kakrc else ["-n"]) + ["-ui", "dummy", "-e", script, path], cwd=workdir, env=ENV,
                           stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
        status = p.returncode
        if status == 255 and retry:
            return run(binary, text, keys, workdir, ext, kakrc, retry=False)
    except subprocess.TimeoutExpired:
        return ("TIMEOUT",)
    def read(f):
        if not os.path.exists(f):
            return None
        return open(f, "rb").read().replace(os.path.realpath(workdir).encode(), b"WORK").replace(workdir.encode(), b"WORK")
    extra = (p.stderr[-300:],) if status not in (0,) else ()
    return (status, read(res), read(res + ".err"), read(path + ".out")) + extra

def case(seed):
    r = random.Random(seed)
    text = random_text(r)
    keys = "".join(command(r) for _ in range(r.randint(1, 12)))
    # braces must stay balanced inside %{...}
    keys = keys.replace("{", "<lt>").replace("}", "<gt>") if keys.count("{") != keys.count("}") else keys
    kakrc = r.random() < 0.2
    ext = r.choice([".c", ".py", ".rs", ".sh", ".md"]) if kakrc else ".txt"
    with tempfile.TemporaryDirectory() as d1, tempfile.TemporaryDirectory() as d2:
        a = run(CPP, text, keys, d1, ext, kakrc)
        b = run(RUST, text, keys, d2, ext, kakrc)
    return seed, text, keys, a, b

if __name__ == "__main__":
    seed, count = int(sys.argv[1]), int(sys.argv[2])
    jobs = int(sys.argv[3]) if len(sys.argv) > 3 else 8
    fails = 0
    timeouts = 0
    crashes = []
    with ThreadPoolExecutor(jobs) as ex:
        for s, text, keys, a, b in ex.map(case, range(seed, seed + count)):
            if a == ("TIMEOUT",) and b == ("TIMEOUT",):
                timeouts += 1
                continue
            if a[0] != "TIMEOUT" and b[0] != "TIMEOUT" and a[0] < 0 and b[0] == 255 and a[1:4] == b[1:4]:
                crashes.append(s)  # C++ crashed on a signal, Rust panicked
                continue
            if a != b:
                fails += 1
                print(f"=== MISMATCH seed={s}\ntext={text!r}\nkeys={keys!r}\ncpp ={a!r}\nrust={b!r}", flush=True)
    print(f"done: {count} cases, {fails} mismatches, {timeouts} timeouts in both, {len(crashes)} crashes in both: {crashes[:20]}")

def repro(seed, binary):
    r = random.Random(seed)
    text = random_text(r)
    keys = "".join(command(r) for _ in range(r.randint(1, 12)))
    keys = keys.replace("{", "<lt>").replace("}", "<gt>") if keys.count("{") != keys.count("}") else keys
    d = tempfile.mkdtemp()
    path = os.path.join(d, "in.txt")
    open(path, "w").write(text)
    script = ("try %{ execute-keys -with-maps %{" + keys + "} } catch %{ echo -to-file " + d + "/res.err %val{error} }; "
              "echo -to-file " + d + "/res %val{selections_desc}; quit!")
    p = subprocess.run([binary, "-n", "-ui", "dummy", "-e", script, path], cwd=d, env=ENV,
                       stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
    print("keys:", keys, "\nexit:", p.returncode)
    err = p.stderr.decode(errors="replace")
    print("\n".join(l for l in err.splitlines() if "panicked" in l or "Received" in l or "kak::" in l or "index" in l or "assert" in l)[:3000])
