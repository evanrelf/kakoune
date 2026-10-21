# Upstream quirks

Questionable behaviour in the C++ Kakoune (hangs, bugs, regressions) that the
Rust port copies on purpose, so it behaves exactly like the original. Each
entry is a candidate to fix later, as a deliberate and documented change.

Undefined behaviour in the C++ can't be copied; the port replaces it with
defined behaviour (see `PORTING.md`). Entries below where that leaves an open
question are marked **needs decision**.

## Hangs

- **Nested regex objects** (`selectors::regex_select_nested`, normal mode
  `<a-i>`/`<a-a>` with a custom `c` object): loops forever when an opening and
  a closing match start at the same position, e.g. opening `\w` and
  closing `\d`.
- **Nested word objects** (`selectors::select_nested_words`): loops forever
  when the selection ends inside invalid UTF-8.
- **Self-based face** (`FaceRegistry::add_face`): `set-face global X red@X`
  is accepted and creates a face based on itself; a later
  `set-face global Y @X` loops forever in the cycle check
  (face_registry.cc:163-168).
- **Wrap with a too wide first codepoint** (`wrap` highlighter): a line
  starting with a codepoint wider than the wrap column (e.g. `-width 1` and a
  double width character) is split at its first atom again on every new
  line, forever. Found by reading the code, not reproduced yet.
- **`replace` with an empty pattern** (`string_utils::replace`): loops
  forever. No caller passes an empty pattern today.

A hang is worse than an error: the editor stops responding, and the panic
handler that backs up modified buffers never runs.

## Event loop

- **Forced fds block** (`EventManager::handle_next_events`): when a fd is
  forced (e.g. by a signal handler), the C++ resets the timeout, so the wait
  blocks until some fd is ready or a signal interrupts it. This regressed in
  9358c62bf ("Take a timeout argument in EventManager::handle_next_events"),
  which replaced `block = false`; zeroing the timeout looks like the intent.

## Editing

- **Repeating a completion insertion** (`InsertCompleter::select`): the
  inserted completion is recorded one key per byte, each byte a sign extended
  `char` (`for (auto& c : completion) record_key(c)`). Replaying it with `.`
  inserts nothing for non ASCII characters.

## Highlighters

- **Tabulations after a Range atom ending in a tab**: the following atom is
  skipped, so its tabs are not expanded.
- **flag-lines on wrapped lines**: flag atoms are moved out on first use, so
  a buffer line shown on several display lines gets empty atoms (padded to
  full width) on the later ones.
- **Unmatched regex captures** give an empty `{0,0}` range, which is then
  ignored rather than highlighted.
- **Signed `char` decoding**: the column ruler's blank test and wrap's
  `-indent` decode bytes through a signed `char`, so non ASCII bytes never
  count as blank.

## Parsing and printing

- **`join` drops leading empty elements** (`string_utils::join`): the joiner
  is only added once the result is non empty, so join/split doesn't round
  trip, and completion tuples whose first element is empty fail to parse back
  ("not enough elements").
- **str-to-str map options**: printing escapes only `=`, but parsing
  unescapes both `=` and `\`, so values with backslashes don't round trip.
- **Keys**: `< >` and `<\x1b>` parse to raw space/escape keys, which print
  unbracketed and re-parse as `<space>`/`<esc>`. `<s-\x1a>` parses as
  `<c-z>`: canonicalisation replaces the modifiers of control codes 1-26
  (dropping alt) before the shift check. The closing `>` of a key name is
  found by decoded codepoint, so an invalid `\xC0>` closes it.
- **`split_escaped` with the separator as escaper**: a separator escapes the
  character after it.
- **Format of bools**: a bool parameter prints as `1`/`0` (there is no
  `to_string(bool)`, it goes through `to_string(int)`).
- **Invalid UTF-8 decoding**: invalid lead bytes decode sign extended
  (`0xFFFFFFxx`) because the C++ passes them as a signed `char`; they are
  then neither blank nor word characters. The byte level selectors
  (`select_sentence`, `select_paragraph`, `select_argument`,
  `select_to_first_non_blank`) convert every byte that way, so the `\xA0`
  byte inside `à` never counts as a blank. Same for `trim_indent`'s trailing
  blanks, and face parsing's word characters (so non ASCII forces the color
  parsing path).
- **`utf8::advance` from the end** returns the end, even with a negative
  count.
- **`categorize`**: `\r` and `\v` are blanks but categorize as punctuation
  (word for WORD).

## Completion ranking

- **Contiguous** compares bytes through the signed `char` to codepoint
  conversion.
- **OnlyWordBoundary** compares a codepoint count with the query's byte
  length.
- **The used letters precheck** can reject non ASCII matches the full matcher
  would accept through `towlower` (e.g. KELVIN SIGN for `k`).

## Performance

- **Undo after many scattered edits** (`compute_modified_ranges`): when the
  changes are not sorted, each one is merged into the ranges separately,
  which is quadratic in the number of changes. Undoing an erase of every
  `foo` in a 200k line file takes seconds (the port matches the C++, and is
  a bit faster).

## Needs decision (C++ undefined behaviour)

- **Inconsistent match ordering** (`ranked_match::sort_matches`): the
  `RankedMatch` ordering is not a strict weak order. `'/'` orders like NUL,
  and invalid UTF-8 decodes differently depending on the following bytes. C++
  `std::sort` with such a comparator is undefined behaviour; Rust's `sort_by`
  may panic ("user-provided comparison function does not correctly implement
  a total order") on candidates containing NUL or invalid UTF-8.
- **Mouse button out of range** (`Key::mouse_button`): mask value 3 is
  printed as `right`; the C++ asserts and throws `logic_error`. Only
  reachable with malformed modifiers.
- **Arithmetic overflow**: coordinate arithmetic and `left_pad`/`right_pad`
  with sizes near `i32::MIN` panic in debug builds (wrap in release); int
  option add/remove wraps. Signed overflow is undefined behaviour in C++.
- **Out of range positions**: `substr` clamps where the C++ asserts;
  negative `utf8::advance` clamps at 0 where the C++ walks before the string.
- **`expand_tabs` with tabstop 0**: divides by zero (Rust panics). The
  `tabstop` option rejects 0, so it isn't reachable through options.
- **Float formatting** (`format`): the C++ uses `std::to_chars` when
  available (shortest representation, e.g. `0.1`) and `snprintf("%f")`
  otherwise (`0.100000`, Apple libc++). The port follows `to_chars`. Nothing
  formats floats yet.
