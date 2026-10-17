# Leftmost-Longest Experimental: Where the Remaining Gap Comes From

Status: measurement note, 2026-10-17. Branch `linear-leftmost-longest-automaton`.
Companion to `leftmost_first_observations.md` (Appendices A–C), which holds the
full experimental history. This file is the short, self-contained answer to
"what is the gap, and can it be fixed?".

## TL;DR

The residual per-character overhead of the zero-rewind experimental matcher on
English prose is **almost entirely one thing**: when a character is not in the
mapper, the experimental matcher must walk the failure chain to harvest
pre-committed emits, while the legacy matcher returns to ROOT in O(1).

On characters that *are* in the alphabet the two matchers are at parity
(19,745 vs 19,089 failure hops over 568,811 characters — a 3.4% difference).
Everything else measured is noise, layout, or a wash.

## Why an earlier note said "~10%"

Appendix B records a callgrind run that reported the remaining cost as "~10% on
English". That number is not comparable with the table below and should not be
quoted as the current gap. It was measured on:

- **words_100 (920 states, 16 KB — fits in L1)**, not words100k;
- **400 sherlock lines**, not the whole book;
- instruction/data-reference counts under cache simulation, with the note that
  L1/LL miss rates were *identical* — i.e. it deliberately excluded the
  memory-hierarchy effect that dominates at larger automaton sizes.

At 920 states the fail-chain flush walk is nearly free (every state is in L1),
so that run measured the *residual* gap with the main effect removed. The
current 17–41% figures come from full-haystack wall clock on a quiet 4-core
box across five automaton sizes, and they include the flush walk.

Both numbers are correct for what they measured; they are not the same
quantity. The honest summary is the table below, plus the causal experiment:
remove the unmapped characters and the sign flips.

## The measurement

words100k (100,001 patterns, 645k states, 11.5 MB) over `sherlock.txt`
(568,811 chars, 23,826 matches), same binary, only `MatchKind` differs:

| | legacy | experimental | delta |
|---|---|---|---|
| character visits (incl. rewind) | 622,130 | 589,087 | exp 5.3% fewer |
| inner-loop iterations | 485,851 | 888,270 | +402,419 |
| — of which on unmapped chars | 0 | 437,218 | |
| failure hops | 19,089 | 321,626 | +302,537 |
| — of which on unmapped chars | 0 | 301,881 | |
| **failure hops on mapped chars** | **19,089** | **19,745** | **+656 (+3.4%)** |

Two things to read out of this:

1. Legacy **does** rewind — it visits 5.3% *more* characters than the
   experimental matcher. So the zero-rewind design does deliver what it
   promises.
2. On mapped characters the matchers do essentially identical fail-walk work.
   The entire gap is the 301,881 hops triggered by the 27.3% of characters that
   are not in the alphabet.

## Why the unmapped case differs

Legacy short-circuits before touching the state array:

```rust
if let Some(mapped_c) = self.mapper.get(c) { loop { ... } }
else { ROOT_STATE_IDX }          // zero states[] accesses
```

The experimental matcher stores pre-committed emit chains **on states**
(`State::output_pos`). Resetting to ROOT therefore requires harvesting any
pending emit from the current state or an ancestor, in innermost-first order.
It has no way to know whether such an emit exists without walking, so it enters
the generic loop: load state, find no transition possible, step to `fail`, load
again, repeat.

Per unmapped character that is not already at ROOT, the walk averages
**2.72 state loads** into an 11.5 MB array.

## Cost accounting

Build-subtracted scan counters (`perf stat`, 20 reps, `zin`, 4-core idle):

| scan-only, words100k | cycles | instructions | L1 miss | LLC loads |
|---|---|---|---|---|
| legacy | 641,297,449 | 531,477,016 | 4,711,861 | 9,659,584 |
| experimental | 772,681,567 | 560,645,054 | 6,082,624 | 12,228,672 |
| ratio | **1.205** | 1.055 | 1.291 | 1.266 |

The builder must be subtracted: it costs 1.5–1.9 G cycles, comparable to the
whole 20-rep scan. Earlier numbers in Appendix C that did not subtract it are
superseded.

Wall clock, alternating arms, medians, `taskset -c 0`:

| dataset | exp/leg |
|---|---|
| words100 (920 states, 16 KB — fits L1) | 1.26 |
| words5k (32k states) | 1.40 |
| words15k | 1.41 |
| words100k (645k states, 11.5 MB) | 1.16–1.18 |
| unidic (1.02M states, 0.0% unmapped) | **0.94–0.96** |

The gap scales with automaton size, because each flush load is more likely to
miss cache in a bigger array. unidic is the counter-example that confirms the
mechanism: Japanese text has essentially no unmapped characters, and there the
experimental matcher *wins*.

## Causal confirmation

Same automaton, same text, only the unmapped fraction changed.

- Take sherlock and delete every non-`a-z` character (0% unmapped, same words):
  exp/leg flips from **1.421 (slower)** to **0.879 (faster)**.
- Independent replication: unidic, 0.0% unmapped → 0.94–0.97.

The sign flip on a single-variable change is the strongest evidence available
here, and it rules out "the experimental design is just slower".

## What is actually applied today

Applied and measured as wins (all in HEAD):

- **locals-hoist** (`aaf4f21`): `pos` / `state_id` live in registers inside
  `ensure_buffered_impl` and are written back at exits. +8–19% on `zin`.
- **fast-ROOT removal** (`c5f503e`): a correctness fix (it broke 7 fuzz tests),
  kept because it is correct, not because it is faster.
- The current scan shape otherwise: one `states[state_id]` load per inner
  iteration feeding both the probe and the fail/output reads.

Not applied, because each measured neutral or negative (see the table below):
`U3` unmapped probe skip, `Vec::new()` buffers, `_mm_prefetch`, ROOT re-probe
merge, `emit_below` bitmap, direct-mapped flush cache, whole-chain flush,
UTF-8 decode restructuring, and the fast-ROOT re-enable.

**LTO was never part of the numbers above.** It was measured separately and
does not help the experimental matcher: with `lto = "fat"` + `codegen-units = 1`
the experimental arm's absolute time is unchanged (words100k 15.3–15.6 ms both
ways; words15k 15.6–16.0 both ways), while *legacy* gets ~4.3% slower. The
exp/leg ratio therefore looks better (1.20 → 1.15) purely because the
denominator grew. Since both matchers are built from the same crate, LTO cannot
be applied to one arm only, so this is not an available improvement.

Three further directions were measured this round, all negative:

- **`step_child` inlined to the legacy four-line form** (two `if let`s instead
  of a tuple destructure): no change at any dataset (words100k 1.178–1.212 vs
  baseline 1.175–1.218; unidic 0.946–0.952 vs 0.945–0.957). LLVM was already
  emitting the straight-line xor/load/compare.
- **LTO** (`fat` / `thin` + `codegen-units = 1`): experimental absolute time
  unchanged; legacy ~4.3% slower. The ratio only improves because the
  denominator grows.
- **PGO** (`-C profile-generate` trained on all five datasets, then
  `-C profile-use`): both arms get *slower* (legacy 10.7 → 12.2 ms,
  experimental 13.6 → 14.6 ms on words100). Harmful on this workload.

With that, every direction identified so far has been measured. Eleven were
neutral or negative; two are structurally impossible.

### Why nothing helps: the first probe is free

A detail that explains all of the above. The inner loop already loads
`states[state_id]` at the top of every iteration to read `base`. When the
transition fails, the code reads `output_pos` from that **already-loaded**
state. So the first check in the flush walk costs nothing.

The walk averages 2.54 hops, so the number of *paid* random loads is about
**1.54 per unmapped character**, not 2.54. Any scheme that replaces those
1.54 loads with "one probe into a side table" is trading one dependent load
for another — which is exactly what the bitmap, the cache, and path
compression all do, and why all three measured as a wash or worse.

Path compression (`skip[s]` = first ancestor with an emit) is the same idea
with 32-bit entries instead of 1-bit: 2.5 MB instead of 78 KB, so it is worse
than the bitmap that already failed. Not implemented.

### What the structure does and does not preserve

`State` keeps `base`, `check`, `fail`, `output_pos` and nothing else — exactly
16 bytes. The NFA's `output` vector (which is non-empty iff a pattern *ends*
at that state) is **discarded** in `set_fails_and_outputs`; only `output_pos`
is copied.

That distinction matters, and it is why a legacy-style register cannot be
transplanted directly:

| | legacy | experimental |
|---|---|---|
| `output_pos.is_some()` means | a pattern ends here | an in-flight committed chain exists (inherited from the parent) |
| fraction of states with it | only word-end states | **52.0%** (words100k) |
| of those, `Emit.gap == 0` | n/a | **31.9%** only |

So neither `output_pos.is_some()` nor `gap == 0` identifies "a match ends at
this state" in the experimental automaton.

### Correction: bit 31 of `output_pos` is NOT free

An earlier revision of this note claimed a word-end bit could be recovered for
free from bit 31 of the 1-based `output_pos` index. That is wrong, and the
reason matters:

- `output_pos` is `Option<NonZeroU32>`, niche-optimised: 0 means `None` and the
  value indexes `emits[pos - 1]`. Its real range is **1..=u32::MAX**.
- The builder enforces exactly that and nothing tighter:
  `NonZeroU32::new(u32::try_from(self.emits.len()).unwrap()).unwrap()` — it
  panics only past `u32::MAX`. There is no `2^31` bound anywhere.
- `emits` is the same order of magnitude as `states`: 222,366 emits for 645,120
  states (words100k), 785,533 for 1,024,000 (unidic), i.e. 0.34 and 0.77 per
  state. So the pool genuinely can grow into the billions.

Borrowing bit 31 would therefore silently halve the supported emit capacity
from 4.29e9 to 2.15e9. It saves no memory (the field is already there) and it
costs range. Not acceptable, and not needed — see the next point.

### `gap == 0` semantics, measured

`Emit.gap` is the distance from the state's path end back to the head match's
end, and `iter_emits` computes `end = pos - gap`. Two separate questions:

**(a) Structurally, is `gap == 0` equivalent to "a pattern ends at this state"?**
No. It is a strict superset. Measured over all states:

| dataset | word-end states | `gap == 0` states | word-end but `gap != 0` | `gap == 0` but not word-end |
|---|---|---|---|---|
| words5k | 5,000 | 5,015 | **0** | 15 |
| words100k | 100,001 | 107,038 | **0** | **7,037** |
| unidic | 674,927 | 685,059 | **0** | **10,132** |

So `word-end => gap == 0` always (it is a necessary condition), but the
converse fails for ~6.6% of `gap == 0` states. The extra cases come from
`push_leaf` and the root-covered fast path: when the *active suffix* completes
a leaf pattern, that match ends at the same text position as the current
state's boundary, so `gap == 0` fires even though the pattern does not end at
this trie state.

**(b) At runtime, does `gap == 0` mean the last match ends at the mismatch
point?** By definition yes (`end = pos - gap`), but it is *not* always the case,
and that is the interesting part:

| dataset | harvests | head `gap == 0` | head `gap > 0` |
|---|---|---|---|
| words100k | 23,717 | 9,559 (40.3%) | **14,158 (59.7%)** |
| unidic | 235,983 | 218,808 (92.7%) | 17,175 (7.3%) |

In 59.7% of English harvests the head match ends *before* the mismatch
position, i.e. characters were consumed after the match without being matched.
That is expected for leftmost-longest over prose (the scan extends past a
committed match looking for a longer one, then fails).

So `gap == 0` is a usable test for "the head match ends exactly here", but it is
neither equivalent to "this state is a pattern end" nor a constant property of
harvests.

## Candidate 5: trie-path "definitely no emit" flag (unsound)

Proposal: maintain one bool while scanning — `path_clean`, meaning "no state on
the trie path since ROOT has `output_pos` with `Emit.gap == 0`". Set it false on
any such state, reset it true at ROOT, and force it false permanently once any
fail step happens. At an unmapped character, if `path_clean` is still true, jump
straight to ROOT instead of walking the failure chain.

It covers a lot: measured on real corpora, `path_clean` is true for **84.5%**
(words100k), **99.7%** (words15k), **99.9%** (words5k) and **100%** (words100) of
unmapped-character resets. unidic never triggers it (0.0% unmapped).

It is nevertheless **unsound**, and the counterexample is small:

    patterns = ["yo"]  (as part of words_100000)
    text     = "anyo "

Tracing the trie path for `anyo`:

| position | state | `output_pos` | gap |
|---|---|---|---|
| `a` | 94 | none | – |
| `an` | 505054 | none | – |
| `any` | 515580 | none | – |
| `anyo` | 515621 | none | – |

No fail step occurred, and no state on the path carries an emit, so
`path_clean` is genuinely true. But `fail("anyo")` is state 189351, which *does*
hold the `yo` emit. The walk would find it; the shortcut skips it.

The guard does not help: it only fires when a fail step happens *during* the
scan, and here the scan is a pure descent.

Measured behaviourally (baseline vs short-circuit enabled, full corpora):

| dataset | baseline matches | with shortcut | lost |
|---|---|---|---|
| words100 | 10 | 10 | 0 |
| words5k | 567 | 567 | 0 |
| words15k | 2,100 | 2,100 | 0 |
| words100k | 23,826 | **21,031** | **2,795 (11.7%)** |
| unidic | 236,442 | 236,442 | 0 |

**The root cause is a set mismatch**: the flag describes the *trie path*
(prefixes of the current state), while the flush walk needs the *failure chain*
(suffixes). A suffix can be a pattern even when no prefix is, and no amount of
"abandon on fail step" tracking changes that, because the descent itself is
clean.

### A real gap this exposed in the test suite

All nine existing fuzzers draw patterns *and* haystacks from the same alphabet,
so **every haystack character is mapped** and the unmapped path is never
exercised — even though unmapped characters are the entire subject of this
investigation. The hand-written corpus tests are too small to hit the shape
above, which is why the shortcut passed the full suite and all 9 fuzz
differentials before the behavioural comparison caught it.

`test_fuzz_unmapped_characters` was added: patterns come from a small alphabet,
haystacks interleave words with separators that never appear in any pattern.
It passes on the baseline and fails the shortcut at round 71. That test is the
durable outcome of this candidate.

### Why `output_pos` looks like it should be enough (and is not)

A natural reading of the code is: "`anyo` ends with the pattern `yo`, so
`S_anyo.output_pos` should point at the `yo` emit with `gap == 0`, and one read
would replace the walk." Measured against words100000, that model does not
match the experimental automaton:

| state | legacy `output_pos` chain | experimental `output_pos` |
|---|---|---|
| `any` (515580) | — | **none** |
| `anyo` (515621) | `["yo"]` | **none** |
| `nyo` (189351) | `["yo"]` | **none** |
| `yo` (6149) | `["yo"]` | `["yo"]`, `gap == 0` |

The model *does* hold for legacy, which stores outputs as a **dictionary-suffix
chain**: every state whose path ends with a pattern carries that pattern.

The experimental automaton stores the chain at the state where the pattern was
**committed**, reached through the *suffix transition*, not through "is a suffix
of my path". For `anyo` the active suffix is `nyo` (a trie node because
`nyoogroop` exists), so the chain lives two fail hops away: `anyo -> nyo -> yo`.
`anyo` itself has no `output_pos` at all. That is precisely why the walk exists,
and why a flag describing trie-path ancestors cannot replace it.

### Candidate 6: propagate `output_pos` along the failure chain at build time

If `output_pos[s] = own(s) ?? output_pos[fail(s)]`, then a single read at an
unmapped character would suffice — no hops. This is the right *set* (the failure
chain, not the trie path), the gaps stay valid (all suffixes of a prefix end at
the same text position), and the DP is O(1) per state in BFS order, the same
recurrence as the rejected bitmap. Crucially it needs **no side array**: the
pointer goes in the existing field.

Implemented and tested. It fails:

    patterns ["abcd", "bce", "b"], haystack "abce"
    expected [(2,1,2), (1,1,4)]        actual [(1,1,4)]   // "b"@[1,2) lost

The reason is that `output_pos` is not a spare slot — it is the **ordering
primitive**. The runtime walks the failure chain and expects each state to
contribute exactly its *own* pending chain, once, in order. Overwriting it with
an inherited chain either duplicates a chain (the walk still reaches the
ancestor) or masks the state's own emits.

Storing both would need a second per-state field: 4 bytes × 645,120 = 2.5 MB,
i.e. the path-compression table already rejected as worse than the 78 KB bitmap
that measured as a wash.

## What was tried and rejected

Each of these either broke correctness or was a wash. All are documented in
Appendix C with numbers.

| attempt | outcome |
|---|---|
| `U3`: skip guaranteed-fail *probes* on unmapped chars | removed 49% of iterations, +0.13% wall clock — it removed the probes, not the walk, which is the part that costs |
| `UM0`: reset to ROOT without harvesting | −84% matches. Pending emits are lost |
| `UM1`/`UM2`: harvest only the current state | correct match *set*, wrong *order*, 5 fuzz failures |
| `UM3`: rewrite the walk to harvest innermost-first without probing | full suite + 9 fuzz differentials green, exact match parity — **a wash** |
| `emit_below` bitmap: per-state bit "my fail chain holds an emit" | fires 90,783× as predicted, all tests green, but cycles −3.0%, **L1 misses +8.5%**, wall clock flat/slightly worse. The bitmap lookup is itself a dependent random load |
| direct-mapped flush cache (design level, see below) | 5,039 distinct start states, 22 walks each — ideal locality — yet a wash: the walk is only 2.54 hops, too short to beat probe overhead |
| `decode_char_unchecked` restructuring | the apparent −16% was code-layout noise (an identical-semantics control measured the same); a real ASCII fast path is neutral on English and worse on unidic |
| fast-ROOT path + fusion | breaks 7 fuzz tests — a correctness fix, not a perf option |
| prefetch, ROOT re-probe merge, `Vec::new()` | all neutral-to-worse |
| flush at arrival instead of at failure | impossible for leftmost-longest: would emit `ab` before seeing `abc`. Deferral is the semantics |
| trie-path "definitely no emit" flag | covers 84.5–100% of unmapped resets but is unsound: `fail(s)` can hold an emit while the whole trie path is clean. Loses 2,795 matches (11.7%) on words100k |
| propagate `output_pos` along the fail chain at build time | right set, O(1) DP, no side array — but `output_pos` is the ordering primitive, not a spare slot; overwriting it loses matches (`abce`) |
| consume the unmapped character once (whole-chain flush) | implemented correctly (full suite + 9 fuzz green) but **much slower everywhere** (words100k 1.17 → 1.57). One-emit-per-call is lazy, not wasteful |

The recurring lesson: **the cost is dependent random loads, and adding a
dependent random load to avoid other dependent random loads is a wash.**

## Is there room to improve?

Not at loop level, and two design-level attempts are now also ruled out
(below). The constraint is structural:

- Legacy keeps the single pending match in a **register** (`last_output_pos`),
  so flushing it is free, and pays for that with rewind.
- Experimental keeps pending matches **on states**, so scanning never rewinds,
  and pays for that with a flush walk.

To get both you would need the pending-emit head in the iterator (legacy's
trick) *while still* pre-committing chains on states for the zero-rewind
guarantee. That is a data-structure change to what a state stores and what the
iterator carries, not a branch that can be added to the inner loop.

Concretely, the promising direction is: when the automaton resets to ROOT at an
unmapped character, the iterator already knows it is resetting. If the
*iterator* cached "the emit head for the current state" — maintained
incrementally as the scan advances, exactly as legacy maintains
`last_output_pos` — the flush would not need to walk at all. The difficulty is
that a state can carry a whole chain, not one match, and the chain's head is
what `output_pos` points at; caching it per state would duplicate state data,
while caching it per iterator step would require the scan to update it on every
transition (which is the work we are trying to avoid).

### Design-space exploration (done)

I implemented and measured two candidate design changes. Both are washes, and
the reasons are structural rather than accidental.

**Candidate 1: direct-mapped flush cache.** The walk from a given start state is
deterministic (it depends only on the state id), and only **5,039 distinct start
states** are ever used on sherlock — 22 walks per distinct state. That locality
looks ideal for memoisation.

Measured: **no gain, slightly worse** (words100k 1.198–1.204 vs 1.176–1.201;
words15k 1.409–1.437 vs 1.401–1.406; unidic 0.99–1.01 vs 0.94–0.97). Full test
suite and all 9 fuzz differentials pass, so this is not a correctness artifact.

Why it fails: the walk averages only **2.54 hops**. Replacing 2.54 dependent
loads with "1 cache probe + tag compare + branch + occasional fill" is not a
win at that length, and the cache table itself competes for L1/L2 while being
touched 110,792 times per pass. This is the classic memoisation threshold:
memoise only when recompute cost is much greater than probe cost.

A first attempt that stored the cache on the *iterator* was 2.8× slower, but
that was an implementation error (16 KB allocated and zeroed per line, 204 MB
per pass over sherlock's 13,052 lines), not evidence about the idea. The
number above is from the corrected version with a process-wide static table.

**Candidate 2: flush at arrival instead of at failure.** Legacy has nothing
pending when it hits an unmapped character, because it flushed at the previous
mapped mismatch. Emulating that would make the unmapped reset O(1) like legacy.

This is **not possible for leftmost-longest**. Flushing `output_pos` on arrival
at a state would emit a match that later characters could still extend:
`ab` would be emitted before `abc` is seen. The emit chains exist precisely to
*defer* commitment so that the longest extension can supersede. Deferral is the
mechanism, not waste. It would only be sound for leftmost-**first**, where the
first match wins and extension is irrelevant.

### What that leaves

Two facts constrain the design:

1. The walk is short (2.54 hops) — too short to profit from memoisation.
2. The deferral is required by leftmost-longest semantics — it cannot be
   hoisted to arrival time.

**Candidate 3: consume the character once (flush the whole chain).** The
current code emits *one* match and returns without advancing `pos`, so the same
unmapped character is decoded and re-entered once per pending emit. Measured:
**20,010 repeated decodes out of 155,346 unmapped-character events (12.9%)**.

A correct whole-chain flush was implemented and it passes the full suite **and
all 9 fuzz differentials** (the earlier 8/9 attempts failed on chain-ordering
and then on a fixed-size head buffer overflowing; the working version reverses
only the *order of chains*, not the contents of each chain). Result:

| dataset | baseline exp/leg | whole-chain flush |
|---|---|---|
| words100 | 1.261–1.271 | **1.508–1.511** |
| words5k | 1.387–1.480 | **1.649–1.664** |
| words15k | 1.395–1.411 | **1.661–1.696** |
| words100k | 1.173–1.207 | **1.574–1.683** |
| unidic | 0.952–0.964 | **1.030–1.037** |

It is **much slower everywhere**. The reason is instructive: the original
one-emit-per-call design is not waste, it is *lazy*. Emitting one match and
returning lets `next()` hand it to the caller immediately; the caller often
stops iterating right there (`.count()`, `.take()`, a first-match query), and
the remaining chain is never walked. Bulk-flushing walks and materialises the
entire chain even when the caller will never ask for it, and it adds the
scratch-vector bookkeeping on every unmapped character. The 12.9% of repeated
decodes is cheaper than the work it would save.

This is the fifth loop-level attempt, and the same lesson again: the apparent
inefficiency is load-bearing.

**Candidate 4: co-locate fail-chain successors** so a hop is a same-line access.
This is a *memory layout* change, not PGO. It is infeasible: `states[base ^ c]`
must be a direct index (the double-array invariant), so reordering states for
fail-chain locality would destroy O(1) transition lookup — the property that
makes the automaton fast in the first place. Not attempted.

So the flush walk is close to irreducible for this semantics.

## Bottom line

- The gap is real, reproducible, and **fully explained**: unmapped-character
  flush walks, 301,881 extra random state loads per pass at 645k states.
- On mapped characters the matchers are at parity (3.4%).
- The experimental matcher is already **faster** on dense, fully-mapped text
  (unidic 0.94–0.97).
- No loop-level fix exists; six attempts failed for the same structural reason.
