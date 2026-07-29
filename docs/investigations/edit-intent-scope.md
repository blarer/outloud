# Scope-aware edit parsing: what landed, and what it measures

**Date:** 2026-07-29
**Scope:** `crates/edit-intent`, `tests/tests/fuzz_edits.rs`
**Follows:** `docs/investigations/edit-intent.md`, recommendation 2

That investigation recommended extending the deterministic parser and left a
prototype at `crates/edit-intent/examples/scope_prototype.rs`. This document
records what was promoted into the shipped crate, what the prototype got
wrong, and the numbers the shipped implementation actually produces.

The prototype has been **deleted**. Everything it did well now lives in
`crates/edit-intent/src/`, with tests; keeping a second copy of the grammar
around would guarantee they drift.

---

## Headline numbers

All measured on this machine, all reproducible from the commands named.

| Measure | Value | Source |
|---|---|---|
| Corpus size | 101 phrasings | `cargo test -p edit-intent` |
| Handled deterministically | **73** | `corpus_produces_exactly_the_expected_strings` |
| Escalated to a model | **28** | same test |
| Regressions on the original 55-command corpus | **0** | `cargo run -p edit-intent --release --example shipped_parser_corpus` |
| Previously wrong-or-missing, now handled | **24 of 25** | same |
| Parse, p50 / p99 | **0.42us / 1.42us** | `cargo run -p edit-intent --release --example parse_timing` |
| Parse + apply, p50 / p99 | **0.92us / 2.62us** | same |
| Parse + apply, max over 46k samples | 8.42us | same |

Every one of the 73 handled cases asserts the **exact resulting string**, not
merely that the utterance parsed. That distinction is the whole point: a
parser that recognises a command and then applies it wrongly is worse than one
that refuses, because the user sees a confident result and has to notice the
damage unaided.

For comparison, the prior investigation measured the same class of command
through Qwen3-1.7B at **324-429ms and 10% correct**. The deterministic path is
five orders of magnitude faster and, on this corpus, correct by assertion.

### The one case still open, and why

`split this into two lines` is the single member of the original 25 that is
still escalated, deliberately. The utterance promises a specific number of
lines. The only splitter available is a sentence splitter, and on the sample
prose it would produce three. Answering a different question than the one
asked is precisely the failure mode this work exists to eliminate, so the
command is refused instead.

---

## Assessing the prototype

The prototype's core insight was right: scope, punctuation, wrapping,
identifier casing, and line operations are the missing families, and none of
them needs a model. Its structure (parse to a small enum, resolve a span,
splice) is the structure that shipped. Four things it got wrong were material.

### 1. Sentence splitting was naive, and it fed a delete

The prototype's own test `known_limitation_sentence_splitting_is_naive`
asserted the wrong answer on purpose:

```
"Ship at 3.5 percent. Tell Dr. Smith we are done"
  prototype's "last sentence" -> "Smith we are done"
```

As a highlight that is a cosmetic bug. As `delete the last sentence` it
destroys `Tell Dr.` and leaves the user with mangled text. The shipped
splitter refuses to break after an abbreviation or a single-letter initial,
and requires whitespace-or-end after the mark so decimals are safe. Asserted
by `sentence_scope_survives_abbreviations_and_decimals`.

The abbreviation list is deliberately short, and the asymmetry is why: a
*missing* entry only makes a scoped delete take less than the user meant,
while a *wrong* entry suppresses a real sentence break and makes it take more.
So `no.` and `co.` are excluded despite being common abbreviations, because
they are also ordinary words. "the answer is no. we should tell them" must
stay two sentences.

### 2. Splicing reflowed the whole document

The prototype repaired the seam left by a delete with
`out.split_whitespace().join(" ")`, which flattens **every** newline and every
run of indentation in the target, not just the cut. Deleting one word from a
three-paragraph field would collapse it to a single line. That is an
unrequested edit outside the requested span, which
`docs/planning/03-definition-of-done.md` forbids outright. The shipped
`splice_out` only touches whitespace abutting the cut. Asserted by
`scoped_delete_preserves_distant_whitespace`.

### 3. Line and paragraph scopes silently meant "everything"

Dictated prose contains no line breaks, so the prototype's `Unit::Line`
resolved to the entire field, and `delete the first line` would wipe it.
Shipped behaviour: line and paragraph scopes return `None` when the text has
no corresponding separator, which the caller reports as a no-match. A glance
costs the user nothing; the alternative costs them their text.

### 4. The scope model itself was too coarse

`Which` was `First | Last | This`, so ordinals beyond "first" were
unreachable. It is now `Nth(usize) | Last`, which makes "the second sentence"
share one code path with "the first". There is deliberately no `First`
variant: it is `Nth(0)`, so the two cannot drift apart.

`Scope` was also implicit rather than a type. It is now a real `Scope` on
`EditIntent`, with `EditIntent::Scoped` composing a scope around any other
intent. That is what makes `docs/ux/03-edit-by-voice.md`'s promised narrowing
real: the inner intent sees **only** its scope, so
`in the last sentence change its to it's` leaves earlier occurrences alone.
Asserted by `scope_narrowing_is_real_not_cosmetic`.

### Does the scope model hold up against real phrasings?

Mostly. It handles `first`/`second`..`fifth`/`last`/`final`/`this`/`that`
crossed with `sentence`/`word`/`line`/`paragraph`/`letter`. What it does not
handle, and correctly refuses rather than approximating:

- **Nested scopes**: "the first word of the last sentence". Resolving half of
  one would edit the wrong region confidently.
- **Counted units**: "the last two words".
- **Ordinal occurrences**: "change the second `the` to `a`". This is different
  from an ordinal *unit*, and `docs/ux/03` promises it eventually. Until it
  exists, the literal verb would read `the second the` as search text, so it
  is refused.
- **Relative positions**: "the sentence before this one", "the second to
  last".

`this`/`that` map to `Last`, which is a judgement call worth naming. Spoken at
a selection holding one sentence they are identical; on a longer selection
"this sentence" means whichever one the caret is in, and this crate is not
given the caret. `Last` is the least surprising degenerate answer, but a
future version with caret information should do better.

---

## Conservatism: what was deliberately refused

The rule is that **a phrasing which could plausibly mean two different edits
must not parse**. The corpus carries these as explicit cases so the refusals
are pinned rather than accidental:

| Utterance | Why refused |
|---|---|
| `wrap the last sentence in quotes` | names a scope the wrap rule cannot honour |
| `make the last word snake case` | identifier casing destroys all spacing |
| `make the first word of the last sentence title case` | two scopes |
| `add a period to the second sentence` | placement not computable |
| `add a comma before customers` | `before` is not implemented; silently appending at the end was a real bug found by this corpus |
| `change the period to a comma` | names two marks |
| `remove the comma` | which of the three? |
| `delete the last sentence and the one before it` | trailing words change the meaning |
| `capitalize the first sentence` | could mean Title Case Every Word, or raise one letter |
| `delete this` | equally a literal request to remove the word "this"; the literal reading fails safe |

That last one deserves emphasis. `delete this` is *not* treated as
"delete everything". Read literally it searches for the word "this", finds
nothing, and reports a no-match: nothing is written. Read as a whole-target
delete it wipes the selection. When two readings disagree, the one that fails
safe wins.

### The mechanism that makes refusal work

Refusing had to be more than "return no intent". The original literal verbs
are greedy: `delete ` followed by anything is a valid `Delete`, and `add `
followed by anything is a valid `Append`. A rule that merely declined would
hand exactly the phrasings it distrusted straight to a literal reading, which
is the bug being fixed. So the extended grammar returns a three-way
`Decision`: `Intent`, `Refuse` (stop here, go to `Freeform`), or `Pass` (try
the literal verbs). `add a period to the second sentence` reaches `Refuse`,
and therefore never becomes `Append { text: "a period to the second sentence" }`.

---

## Bugs found while building this, all by tests rather than by reading

1. **`before` silently became `at the end`.** `add a comma before customers`
   put a comma at the far end of the field. Found by adding it to the corpus
   as an expected escalation.
2. **Overlapping scope detection refused valid commands.** "the first letter"
   contains "first letter", so scanning token-by-token counted two scopes and
   refused `uppercase the first letter` - the exact command this work exists
   to fix. Scanning now skips past a match.
3. **`semicolon` was read as `colon`.** Substring containment. Matches are now
   blanked from a working copy as they are found.
4. **A unit word alone was treated as a scope**, which broke
   `sentence case please`, a command that always worked. A scope now requires
   both a position word and a unit word.
5. **The ordinal-occurrence guard ate `change second to 2nd`**, where
   "second" is the operand rather than a targeting word. Caught by an existing
   workspace pipeline test. The guard now requires the definite article.

Five bugs in a grammar this size is the strongest available argument that the
exact-string testing bar is the right one. Three of the five would have been
invisible to a test that only checked "did it parse".

---

## Test surface

`cargo test -p edit-intent` (36 tests) and
`cargo test -p workspace-tests --test fuzz_edits` (10 tests).

The workspace fuzz suite was extended rather than duplicated, because that is
where the crate's byte-slicing bugs were found before. New properties:

- `scoped_deletes_never_add_content` - the over-edit gate, applied to the
  scope-aware path where a mis-resolved span takes the wrong *words*.
- `a_scoped_edit_never_touches_text_before_its_scope` - a leaking scope is
  worse than no scope, because the user believes they narrowed the radius.
- `identifier_casing_emits_only_identifier_characters` - the `İ` trap: it is
  alphanumeric, but lowercases into a combining mark that is not. Exactly what
  Qwen3-1.7B got wrong in the head-to-head.
- `punctuation_never_stacks`.

`random_utterance` now generates the scope-aware commands too, so all 20k
iterations of the existing panic-freedom property cover them.

---

## Known limitations

Honest list, none of which cause a wrong edit; each escalates instead.

- Sentence splitting handles abbreviations, initials, and decimals, but not
  ellipses, quoted sentences ending inside quotation marks, or non-English
  terminators (`。` is not a break).
- No un-wrapping (`remove the quotes`), no moving text, no swapping units, no
  counted units, no ordinal occurrences, no `before` placement.
- `this`/`that` resolve to the last unit, as discussed above.
- Identifier casing is whole-target only.
- `Replace` remains substring-based, so `change the to a` also hits "them".
  Pre-existing behaviour, not a regression, and a real gap.

### One hand-off the delivery layer owns

`delete the last sentence` against a field holding exactly one sentence
empties it. That is the correct edit and the parser should not refuse it. But
the blast radius is 100% of the target, which is exactly the case
`docs/ux/03-edit-by-voice.md` routes to a preview rather than an instant
apply. The parser's job is to be right; deciding whether "right and total"
needs confirming belongs to delivery. Pinned by
`a_scoped_delete_may_legitimately_empty_the_target` so the hand-off is
explicit rather than assumed.

---

## Confidence

**High** on the coverage and timing numbers: both come from tests and a
harness in the tree, both assert exact strings or measure directly, and the
regression check against the original 55-command corpus reports 0.

**High** on the refusals being deliberate: each is a corpus case with a stated
reason, so removing one requires editing the test.

**Medium** on the corpus being representative. It is 101 phrasings of my
construction, drawn from `docs/ux/03-edit-by-voice.md`, the prior
investigation's corpus, and ordinary editing operations. It is not observed
user traffic, and real usage would shift the mix. The 73/101 figure is a
statement about this corpus.

**Medium** on the abbreviation list. It is short by design, and the failure
mode of a missing entry is mild, but no corpus of dictated prose was available
to tune it against.
