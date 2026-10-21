"""Case generators for kakfuzz.

A case is either a list of key tokens (sent as one `keys` JSON-RPC call) or
an RPC event (resize, mouse, scroll, menu_select). Generators are grouped in
categories; each run (swarm testing) enables a random subset of categories
with random weights, so runs explore different corners instead of averaging
over everything.
"""
import random

# Text pieces used for buffer contents and typed text
WORDS = ["foo", "bar", "baz", "qux", "(x)", "[y, z]", "{a; b}", '"str"', "'c'", "42", "-3.5", "0x1f",
         "é", "日本", "😀", "é", "\t", "  ", "foo_bar", "a.b", "<t>", "x=1", "#", "//", "/*", "*/",
         "if", "else", "return", "def", "fn", " ", "ab\\cd", "%", "$x", "`cmd`"]

FILES = ["a.txt", "b.py", "c.c", "d.rs", "e.md"]


def text(r, max_lines=8):
    lines = []
    for _ in range(r.randint(0, max_lines)):
        indent = r.choice(["", "", "  ", "    ", "\t"])
        lines.append(indent + " ".join(r.choice(WORDS) for _ in range(r.randint(0, 7))))
    return "\n".join(lines) + ("\n" if r.random() < 0.9 else "")


def typed(r):
    """Text typed in insert mode, as key tokens"""
    out = []
    for _ in range(r.randint(0, 6)):
        k = r.random()
        if k < 0.55:
            out.extend(literal_keys(r.choice(WORDS + ["x", "y z", "hello"])))
        elif k < 0.85:
            out.append(r.choice(["<ret>", "<tab>", "<backspace>", "<del>", "<space>", "<left>", "<right>",
                                 "<up>", "<down>", "<home>", "<end>", "<c-u>", "<c-w>", "<a-;>h", "<a-;>w",
                                 "<a-;><a-;>", "<c-r>\"", "<c-r>/", "<c-r>a", "<c-v>x", "<c-v>u00e9"]))
        else:
            out.append(r.choice(["<c-x>f", "<c-x>w", "<c-x>l", "<c-x>o", "<c-n>", "<c-p>", "<c-o>",
                                 "<c-x>w<c-n>", "<c-x>l<c-n><c-n>"]))
    return out


SPECIAL = {"<": "<lt>", ">": "<gt>", " ": "<space>", "-": "<minus>", "+": "<plus>", "\t": "<tab>",
           "\n": "<ret>", ";": "<semicolon>"}


def literal_keys(s):
    return [SPECIAL.get(c, c) for c in s]


def regex(r):
    return r.choice(["\\w+", "o", "foo|bar", "\\d+", "\\s", "[()]", "^", "$", ".", "\\b", "(?<=a)b",
                     "é", "[^ ]+", "x*", "\\n", "\\h+", "(\\w)(\\w)", "\\Q.\\E", "(?i)FOO", "a{2,}",
                     "(?<n>\\w)\\k<n>", "\\A", "\\z", "[[:alpha:]]+", "\\S+\\s", "(?=o)", "(", "[", "*a",
                     "(foo)?bar", "\\x41", "\\u00e9", "[a-z&&[^aeiou]]"])


# ---------------------------------------------------------------- categories

def motions(r):
    return [r.choice(list("hjklHJKLwbeWBExX%;") + [
        "<a-w>", "<a-b>", "<a-e>", "<a-W>", "<a-B>", "<a-E>", "<a-l>", "<a-h>", "<a-L>", "<a-H>",
        "<a-x>", "<a-;>", "<a-:>", "<pageup>", "<pagedown>", "<c-f>", "<c-b>", "<c-d>", "<c-u>",
        "<left>", "<right>", "<up>", "<down>", "<home>", "<end>", "m", "M", "<a-m>", "<a-M>"])]


def goto_view(r):
    return [r.choice(["g", "G", "v", "V"]) + r.choice(list("hjklgkeitbcaf.vmt<>"))]


def find_char(r):
    return [r.choice(["f", "t", "F", "T", "<a-f>", "<a-t>", "<a-F>", "<a-T>"]) + r.choice(list("o(a;é") + ["<space>", "<ret>"])]


def objects(r):
    obj = r.choice(list("bBrawWspinuqQg") + ["<a-w>", "<space>", "(", ")", "<lt>", "<gt>", '"', "'", "`",
                                              ",", "c\\(,\\)<ret>", "c\\{,\\}<ret>", "c,<ret>", "<a-;>"])
    return [r.choice(["<a-i>", "<a-a>", "[", "]", "{", "}", "<a-[>", "<a-]>", "<a-{>", "<a-}>", "<a-I>", "<a-A>"]) + obj]


def regex_ops(r):
    k = r.choice(["s", "S", "/", "?", "<a-/>", "<a-?>", "<a-k>", "<a-K>"])
    toks = [k] + literal_keys(regex(r))
    if r.random() < 0.2:
        toks.append(r.choice(["<c-r>/", "<up>", "<c-w>", "<backspace>", "<tab>"]))
    toks.append(r.choice(["<ret>", "<ret>", "<ret>", "<esc>"]))
    return toks + ([r.choice(["n", "N", "<a-n>", "<a-N>", "*", "<a-*>"])] if r.random() < 0.3 else [])


def edits(r):
    return [r.choice(list("dcyRpP~`<>&_@") + ["<a-d>", "<a-c>", "<a-p>", "<a-P>", "<a-R>", "<a-`>", "<a-j>",
                                               "<a-J>", "<a-&>", "<a-@>", "<a-lt>", "<a-gt>", "<a-o>", "<a-O>",
                                               "r" + r.choice("x é"), "rx", "<gt>", "<lt>"])]


def insert(r):
    return [r.choice(["i", "a", "I", "A", "o", "O", "c", "<a-c>"])] + typed(r) + ["<esc>"]


def multi(r):
    return [r.choice(["C", "<a-C>", "<a-s>", "<a-S>", "(", ")", "<a-(>", "<a-)>", ",", "<a-,>", "+",
                      "<a-+>", "<a-_>", "x", "%"])]


def history(r):
    return [r.choice(["u", "U", "<a-u>", "<a-U>", "<c-k>", "<c-j>", "<c-o>", "<c-i>", "<c-s>", ".", "<a-.>"])]


def registers(r):
    reg = r.choice(list("abc\"/^@_|:") + ["<c-r>"])
    k = r.random()
    if k < 0.3:
        return ['"', reg, r.choice(list("yPpRdcZz") + ["<a-z>a", "<a-Z>u", "<a-p>"])]
    if k < 0.5:
        return ["Q"] + motions(r) + edits(r) + ["Q"]
    if k < 0.6:
        return ['"', "a", "Q"] + insert(r) + ["Q"]
    if k < 0.75:
        return [r.choice(["q", '"aq', "Z", "z", '"bZ', '"bz', "<a-z>a", "<a-z>u", "<a-Z>i"])]
    return [str(r.randint(0, 5))] + r.choice([motions, edits, objects, find_char])(r)


def counts(r):
    return [str(r.randint(1, 12))] + r.choice([motions, edits, find_char, objects, history, multi])(r)


def prompt_edit(r):
    """Open a prompt and use its line editing and completion"""
    k = r.choice([":", "/", "|", "!", "<a-|>", "$"])
    toks = [k] + literal_keys(r.choice(["e", "set-op", "buf", "echo hi", "ech", "set-option global ta",
                                        "add-highlighter ", "doc ", "colorscheme ", "foo bar", "eval"]))
    for _ in range(r.randint(0, 5)):
        toks.append(r.choice(["<tab>", "<s-tab>", "<c-a>", "<c-e>", "<c-w>", "<c-u>", "<c-k>", "<left>",
                              "<right>", "<a-f>", "<a-b>", "<up>", "<down>", "<c-r>\"", "<c-r><c-w>",
                              "<backspace>", "<del>", "x", "<space>", "<c-o>", "<c-x>", "<a-!>"]))
    # completing then validating a : prompt can run any command, quit and
    # kill included, which ends the run: only validate without completion
    completed = any(t in ("<tab>", "<s-tab>", "<up>", "<down>", "<c-u>", "<c-w>") for t in toks)
    toks.append("<esc>" if k == ":" and completed else r.choice(["<esc>", "<esc>", "<ret>"]))
    return toks


SHELL = ["sort", "cat", "true", "false", "echo hi", "tr a-z A-Z", "sed s/o/0/", "printf x", "wc -l",
         "head -1", "awk '{print $1}'", "printf '%s\\n' a b", "exit 3", "cat >/dev/null"]


def shell(r):
    k = r.choice(["|", "!", "<a-!>", "<a-|>", "$"])
    return [k] + literal_keys(r.choice(SHELL)) + ["<ret>"]


def command(r, text_):
    """Run a kakscript command line through the : prompt"""
    return [":"] + literal_keys(text_) + ["<ret>"]


def options(r):
    opt = r.choice([
        "tabstop " + str(r.choice([1, 2, 4, 8, 3])), "indentwidth " + str(r.choice([0, 2, 4])),
        "scrolloff " + r.choice(["0,0", "2,2", "5,10"]), "autoinfo " + r.choice(["", "command", "onkey|normal"]),
        "autocomplete " + r.choice(["", "insert", "prompt", "insert|prompt"]), "aligntab " + r.choice(["true", "false"]),
        "matching_pairs " + r.choice(["( ) [ ]", "< >", ""]), "extra_word_chars " + r.choice(["_", "_ -", "é"]),
        "eolformat " + r.choice(["lf", "crlf"]), "BOM " + r.choice(["none", "utf8"]),
        "filetype " + r.choice(["", "python", "c", "rust", "markdown", "sh", "kak"]),
        "incsearch " + r.choice(["true", "false"]), "ui_options ''",
        "completers " + r.choice(["filename", "word=all", "word=buffer line=buffer", ""]),
        "static_words " + r.choice(["foo foobar foobaz", ""]),
        "path " + r.choice(["./ %/", "/nonexistent"]), "startup_info_version 0",
        "writemethod " + r.choice(["overwrite", "replace"]), "auto_pairs " + r.choice(["( ) { }", ""]),
    ])
    scope = r.choice(["global", "buffer", "window", "current"])
    return command(r, f"set-option {r.choice(['', '-add ', '-remove '])}{scope} {opt}")


def highlighters(r):
    hl = r.choice([
        "number-lines", "number-lines -relative -hlcursor", "wrap", "wrap -word -indent -width 20",
        "wrap -marker > -width 10", "show-whitespaces", "show-whitespaces -tab > -lf $",
        "show-matching", "regex \\w+ 0:red", "regex (o)(\\w) 1:green 2:+u", "dynregex %reg{/} 0:+r",
        "column 10 red", "line 2 blue", "fill yellow", "ranges fuzz_ranges",
        "replace-ranges fuzz_rr", "flag-lines red fuzz_flags", "group g", "regions r",
    ])
    if r.random() < 0.3:
        return command(r, "remove-highlighter " + r.choice(["window/number-lines", "window/wrap", "window/show-whitespaces",
                                                            "window/show-matching", "window/g", "buffer/nonexist"]))
    return command(r, f"add-highlighter {r.choice(['window', 'buffer', 'global'])}/ {hl}")


def buffers(r):
    return command(r, r.choice([
        "edit " + r.choice(FILES), "edit -scratch *s*", "edit -scratch", "buffer " + r.choice(FILES + ["*s*", "*debug*"]),
        "delete-buffer", "delete-buffer!", "buffer-next", "buffer-previous", "rename-buffer new-" + r.choice(FILES),
        "write", "write -force", "edit!", "write " + r.choice(FILES), "edit -readonly a.txt", "edit -existing nope.txt",
        "evaluate-commands -buffer * %{ set-option buffer tabstop 3 }", "set-option buffer readonly true",
        "set-option buffer readonly false"]))


KAKSCRIPT_ATOMS = [
    "echo hello", "echo -markup {Error}x", "echo %val{selection}", "echo %val{selections}", "echo %val{bufname}",
    "echo %opt{tabstop}", "echo %reg{dquote}", "echo %sh{echo $kak_selection}", "echo %arg{@}",
    "echo %val{cursor_line} %val{cursor_column} %val{cursor_char_column}", "echo %val{selections_desc}",
    "echo %val{buf_line_count} %val{timestamp} %val{history_id} %val{modified}", "echo %val{window_range}",
    "echo -quoting shell a 'b c'", "echo -quoting kakoune a 'b c'", "fail oops", "nop", "nop %sh{true}",
    "set-register a x y z", "set-register / foo", "execute-keys gg", "execute-keys -draft %{ggdd}",
    "execute-keys -itersel x", "execute-keys -with-hooks iX<esc>", "evaluate-commands -draft %{ execute-keys w }",
    "evaluate-commands -itersel %{ execute-keys l }", "evaluate-commands -save-regs a %{ set-register a q }",
    "evaluate-commands -no-hooks %{ execute-keys ihi<esc> }", "try %{ fail x } catch %{ echo caught %val{error} }",
    "try %{ fail x } catch %{ fail y } catch %{ echo second }", "declare-option int fuzz_i 3",
    "declare-option str-list fuzz_l a b", "declare-option -hidden completions fuzz_c",
    "set-option global fuzz_i 7", "set-option -add global fuzz_i 2", "echo %opt{fuzz_i} %opt{fuzz_l}",
    "declare-option range-specs fuzz_ranges", "set-option window fuzz_ranges %val{timestamp} 1.1,1.3|red",
    "declare-option line-specs fuzz_flags", "set-option window fuzz_flags %val{timestamp} '1|>' '2|+'",
    "declare-option range-specs fuzz_rr", "set-option window fuzz_rr %val{timestamp} '1.1,1.2|XX'",
    "define-command -params .. fuzz-cmd %{ echo %arg{@} %arg{#} }", "fuzz-cmd a b c",
    "define-command -override -params 1 fuzz-one %{ execute-keys %arg{1} }", "fuzz-one x", "fuzz-one 'wd'",
    "alias global fz fuzz-cmd", "fz 1 2", "unalias global fz",
    "hook -group fuzzy global InsertChar x %{ execute-keys -draft h }", "hook -group fuzzy window NormalKey j %{ echo j }",
    "hook -once -group fuzzy buffer BufWritePre .* %{ echo pre }", "remove-hooks global fuzzy", "remove-hooks window fuzzy",
    "hook -group fuzzy global ModeChange .*:insert %{ echo ins }", "trigger-user-hook foo",
    "hook -group fuzzy global User foo %{ echo user }",
    "map global user q 'xd' -docstring hi", "map window normal X 'xd'", "unmap window normal X", "map global insert <c-y> '<esc>u'",
    "map global object Q 'c\\(,\\)<ret>'", "map global goto q '<esc>gg'",
    "info hello", "info -title t -style modal body", "info -anchor 1.1 -style inline anchored",
    "menu a %{ echo a } b %{ echo b }", "menu -auto-single only %{ echo single }",
    "prompt test: %{ echo %val{text} }", "prompt -init x -password p: %{ echo %val{text} }", "on-key %{ echo %val{key} }",
    "select 1.1,1.3 2.1,2.2", "select -timestamp %val{timestamp} 1.2,1.1", "select -codepoint 1.1,1.2",
    "set-face global Error red", "set-face window PrimarySelection +r", "unset-face window PrimarySelection",
    "colorscheme default", "colorscheme gruvbox-dark", "update-option buffer fuzz_ranges",
    "change-directory .", "evaluate-commands %sh{ printf 'echo %s' \"$kak_buflist\" }",
    "evaluate-commands %sh{ printf %s \"execute-keys $kak_count\" }", "echo %{nested {braces} ok}",
    "echo 'single ''quoted'''", "echo \"double %val{bufname} \\\"esc\\\"\"", "echo %[brackets]", "echo %<angles>",
    "echo %|pipes|", "echo %sh{printf '%s' \"$kak_opt_tabstop\"}", "complete-command -menu fuzz-cmd shell-script-candidates %{ echo a }",
    "debug buffers", "debug options", "echo -debug dbg", "add-highlighter window/hl regex o 0:red",
    "set-option buffer filetype python", "set-option buffer filetype c", "comment-line", "comment-block",
    "format", "lint", "git status", "doc keys", "ctags-search foo", "spell", "terminal true",
    "evaluate-commands -client client0 echo cl", "evaluate-commands -try-client nope echo tc",
    "declare-user-mode fz", "map global fz a 'gg'", "enter-user-mode fz", "enter-user-mode -lock fz",
    "provide-module fm %{ echo loaded }", "require-module fm", "require-module nope",
    "set-option global autoreload yes", "set-option global ui_options terminal_assistant=none",
    "write-all", "kill-zombie", "echo %file{a.txt}", "echo %file{nope}",
]

GARBAGE = ["%{", "}", "%sh{", "'", "\"", "%val{", "%opt{nope}", "%reg{zz}", "echo %{", "-", "--",
           "set-option", "set-option global", "map", "hook", "define-command", "echo -to-file", "%%",
           "%arg{1}", "evaluate-commands %{ %{ }", "try %{", "\\", ";;", ";", "eval %{fail %{x}}"]


def kakscript(r):
    parts = [r.choice(KAKSCRIPT_ATOMS) for _ in range(r.randint(1, 3))]
    if r.random() < 0.15:
        parts.append(r.choice(GARBAGE))
    sep = r.choice(["; ", "\n"])
    line = sep.join(parts)
    if r.random() < 0.2:
        line = "evaluate-commands %{ " + line + " }"
    if r.random() < 0.1:
        line = "try %{ " + line + " } catch %{ echo -markup {Error}%val{error} }"
    return command(r, line)


def rpc_event(r):
    k = r.random()
    if k < 0.3:
        return {"method": "resize", "params": [r.randint(3, 40), r.randint(10, 120)]}
    if k < 0.6:
        method = r.choice(["mouse_press", "mouse_release"])
        return {"method": method, "params": [r.choice(["left", "right", "middle"]), r.randint(0, 25), r.randint(0, 90)]}
    if k < 0.75:
        return {"method": "mouse_move", "params": [r.randint(0, 25), r.randint(0, 90)]}
    if k < 0.9:
        return {"method": "scroll", "params": [r.randint(-5, 5), r.randint(0, 25), r.randint(0, 90)]}
    return {"method": "menu_select", "params": [r.randint(-1, 4)]}


CATEGORIES = {
    "motions": motions, "goto_view": goto_view, "find_char": find_char, "objects": objects,
    "regex_ops": regex_ops, "edits": edits, "insert": insert, "multi": multi, "history": history,
    "registers": registers, "counts": counts, "prompt_edit": prompt_edit, "shell": shell,
    "options": options, "highlighters": highlighters, "buffers": buffers, "kakscript": kakscript,
    "rpc_event": rpc_event,
}


class Swarm:
    """A random subset of categories with random weights (one per run)"""

    def __init__(self, r, enabled=None):
        names = list(CATEGORIES)
        if enabled is None:
            enabled = [n for n in names if r.random() < 0.5] or [r.choice(names)]
        self.enabled = enabled
        self.weights = [r.choice([1, 2, 4, 8]) for _ in enabled]

    def case(self, r):
        """A case: a list of key tokens, or an RPC event dict"""
        cats = r.choices(self.enabled, self.weights, k=r.randint(1, 4))
        if "rpc_event" in cats and r.random() < 0.5:
            return rpc_event(r)
        toks = []
        for c in cats:
            if c != "rpc_event":
                toks.extend(CATEGORIES[c](r))
        return toks or ["l"]
