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

The one loop-level idea from Appendix B that was never implemented is inlining
`step_child` into the legacy four-line form to remove the double-`Option`
destructure. It was listed as "promising, expected a few %" but never measured.
That is the honest remaining gap in coverage.

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
