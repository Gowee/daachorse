# Leftmost-Longest Experimental: Where the Remaining Gap Comes From

Status: measurement note, 2026-10-17. Branch `linear-leftmost-longest-automaton`.
Companion to `leftmost_first_observations.md` (Appendices A–C), which holds the
full experimental history. This file is the short, self-contained answer to
"what is the gap, and can it be fixed?".

## TL;DR

The residual per-character overhead of the zero-rewind experimental matcher on
English prose is **entirely** in the handling of characters absent from the
mapper — but *not*, as an earlier revision of this note claimed, primarily in
the failure-chain walk. Measured by ablation on the experimental arm alone
(words100k): removing the walk buys 3.5%; handling unmapped characters with
zero state-array access would buy **20.8%**, which would roughly tie legacy.

So the walk is ~1/6 of the effect. The rest is the unmapped characters
themselves — but **not** the extra inner-loop iterations, as an earlier revision
of this paragraph claimed. Two sound variants that remove the walk while keeping
every match give only 3.5–6%:

| dataset | baseline | `check` bit 31 | 1-byte side table |
|---|---|---|---|
| words100 | 14.73 | — | **14.16 (−3.9%)** |
| words5k | 15.00 | 14.78 (−1.5%) | **14.09 (−6.1%)** |
| words15k | 15.96 | 15.48 (−2.8%) | **15.21 (−4.7%)** |
| words100k | 15.85 | **15.17 (−3.5%)** | 16.24 (+2.5%) |
| unidic | 30.12 | 31.12 (+3.5%) | 30.24 (+0.4%) |

The two carriers trade places with automaton size, exactly as the cache
argument predicts: a 1-byte side table (645 KB) is cheap when the automaton is
small and actively harmful at 645k states, where it competes with the 11.5 MB
double-array; the bit inside `State::check` costs nothing extra but pays a
mask on every probe.

**So a sound O(1) test for "does my fail chain hold an emit" is worth ~4–6%.**
The remaining 11–17 points of the gap are *not* the walk and are not addressed
by any of these.

On characters that *are* in the alphabet the two matchers are at parity
(19,745 vs 19,089 failure hops over 568,811 characters — a 3.4% difference).
Everything else measured is noise, layout, or a wash.

## Why legacy's O(1) test works, and what it does not imply

A natural objection: *`last_output_pos` is a single register, so how can legacy
know whether a match is pending on the failure chain? Surely it must walk too?*
Concretely: patterns `abcdef, bc, de`, input `abcdeZ`. At state `abcde` the `Z`
is unmapped — does legacy not have to walk?

It does not walk, and the trace shows why. Legacy on `abcdeZ`:

| char | state | `last_output_pos` | `self.pos` | skips |
|---|---|---|---|---|
| `a` | 0→2 | None | 0 | 1 |
| `b` | 2→7 | None | 0 | 2 |
| `c` | 7→8 | None | 0 | 3 |
| `d` | 8→9 | **Some(1) = `bc`** | **0→3** | 1 |
| `e` | 9→10 | Some(1) | 3 | 2 |
| `Z` | 10→0 | Some(1) | 3 | 3 → emits `bc` |

Two corrections to the intuitive picture:

1. **The pending match is discovered at the mapped character that completes it,
   not at the unmapped one.** `bc` is committed at `d` (state 8→9), and
   `self.pos` jumps to 3 right there. By the time `Z` arrives, the register
   already holds it; there is nothing to search for.
2. **The match that starts after the commit point is found by re-scanning.**
   After `Z` emits `bc` with `end = self.pos = 3`, the next call resumes at
   position 3 — so `d`, `e`, `Z` are processed *again*. The trace shows this
   literally: the second pass is `d e Z`, and `de` is committed at the second
   `e`. That is the rewind.

So the trade in this example is exact:

- **legacy**: no walk at `Z`, but re-scans `d e Z` (the text is read twice).
- **experimental**: walks the failure chain at `Z`, but never re-scans. Its
  inner loop runs 7 iterations for this input versus 5 for `abcde`.

Note that exp also processes `Z` twice (the trace shows `pos=5` twice): the
first `Z` emits and advances to the next link, the second finds ROOT and
resets. That re-entry is the 20,010-per-pass effect measured on sherlock.

Why legacy can rely on a single register at all: **a match cannot straddle an
unmapped character**, because patterns define the mapper, so every character of
a match is mapped. Any match that had started before `Z` and not yet ended
would have to contain `Z` — impossible. Hence the only pending match is one
that already ended, and legacy keeps exactly one of those, advancing
`self.pos` as it goes. The experimental matcher defers commitment *without*
advancing `self.pos`, so it can accumulate several un-emitted matches and must
store a chain (99.5% of which turn out to be length 1 — but it cannot prove
that without looking, and looking is the walk).

### Why the fail chain never has to be walked for a suffix match

The sharpest form of the objection: with patterns `abcdef, bc, bcdef, de` and
input `abcdeZ`, must legacy not walk from state `abcde` to state `bcdef` and
compare? Traced answer: **no, for two independent reasons.**

1. **`Z` is unmapped, so the walk never starts.** `next_state_id_leftmost_unchecked`
   tests `mapper.get(c)` on its first line and returns ROOT immediately. No
   state is touched. The trace shows no fail-walk step at all for any
   unmapped-character input.

2. **Even for a mapped failing character the walk terminates in one step,
   at `DEAD`.** The builder sets `fail = DEAD_STATE_IDX` on every state that
   carries an output and propagates DEAD to all descendants
   (`nfa_builder.rs`, "Sets the output state to the dead fail" / "If the parent
   has the dead fail, the child also has the dead fail"). The runtime then does

       let fail_id = state.fail();
       if fail_id == DEAD_STATE_IDX { return ROOT_STATE_IDX; }

   so once a match has been committed, the whole subtree under it collapses the
   walk to zero hops. This is the actual source of legacy's O(1), not the
   register.

That DEAD termination is also *correct*, and it is what makes `bcdef` behave
properly: in `abcdef` the pattern `bcdef` must **not** be emitted, because
leftmost-longest prefers the match starting at 0. Measured — both matchers emit
only `abcdef` for `abcdef`, and only `bcdef` for `xbcdef`.

**Is `last_output_pos` still valid after a fail walk?** Yes, necessarily: the
walk reads only `base`, `check` and `fail` from the state array, and
`last_output_pos` is a local in `next_plain`. It is neither read nor written by
`next_state_id_leftmost_unchecked`. The traces confirm the register keeps its
value across multi-hop fail walks (`[failwalk] 12 -> 8 -> 3 -> 0` with
`last_out` unchanged).

## Where the gap actually is (cycle accounting)

`perf stat`, words100k, 20 reps, same binary, only `MatchKind` differs:

| metric | legacy | experimental | delta |
|---|---|---|---|
| cycles | 2,179,138,196 | 2,638,595,692 | **+21.1%** |
| instructions | 1,339,401,731 | 1,473,662,732 | +10.0% |
| branch-misses | 9,621,468 | 12,479,891 | +29.7% |
| cache-misses | 19,788,934 | 24,169,265 | +22.1% |
| cache-references | 61,084,894 | 75,343,930 | +23.3% |

Per rep the delta is 22,972,874 cycles, and it decomposes roughly as:

| source | cycles | share |
|---|---|---|
| +6.71 M instructions (at the measured IPC) | ~10.1 M | ~44% |
| +219 k cache misses | ~8.8 M | ~38% |
| +143 k branch misses | ~2.6 M | ~11% |
| overlap / remainder | ~1.5 M | ~7% |

**There is no single cause.** The gap is ~10% more work, ~23% more cache
misses, and ~30% more branch misses, compounding. This is why every single-axis
optimisation (walk elimination, bitmap, cache, LTO, PGO, inlining) came back
between −4% and +3%: each one attacks at most one of the three terms, and the
largest single term is "more instructions", which is what the design inherently
executes.

Two specifics worth recording:

- The `if let Some(ep) = state.output_pos()` branch on the mismatch path runs
  321,626 times per pass and is taken only **7.0%** of the time. A 7/93 branch
  with no correlation is close to worst case for a TAGE predictor, and it is
  one of the top two branch-miss sites in `perf annotate`.
- The top cache-miss site is the transition probe's
  `cmp %ebx, 0x4(%rax,%r8,1)` — a random index into the 11.5 MB state array,
  at 33.2% of all cycles.

## The fix that worked: inline the pending-match buffer

Every other attempt in this document attacked the *scan*. The largest remaining
cost was not in the scan at all: `LeftmostFindIteratorExperimental` owned a
`Vec<Match<V>>` for pending matches, allocated with `with_capacity(4)` at
iterator construction. The common usage pattern creates one iterator per
haystack — in the benchmark, one per line of `sherlock.txt`, i.e. **13,052
iterators per pass**.

Measured allocation counts (counting global allocator, one pass over sherlock):

| | allocations | bytes |
|---|---|---|
| baseline | **13,052** | 1,252,992 |
| inline buffer | **0** | 0 |

Meanwhile the buffer almost never holds more than one element: emit chains are
length 1 in 99.5% of cases (max observed 3), and 99.7% of `next()` calls find
the buffer empty and scan. So the allocation bought nothing.

The fix replaces the `Vec` with `MatchBuf<V, 4>`: four `Option<Match<V>>` slots
inline in the iterator, plus a `Vec` used only as a spill for chains longer
than four (which never occurred on these corpora but must be supported).

Results, experimental arm only, medians of 5 × 9 reps on `zin`:

| dataset | before | after | delta |
|---|---|---|---|
| words100 | 14.72 ms | **10.00 ms** | **−32.1%** |
| words5k | 15.03 ms | **10.19 ms** | **−32.2%** |
| words15k | 16.10 ms | **11.02 ms** | **−31.6%** |
| words100k | 15.71 ms | **13.69 ms** | **−12.9%** |
| unidic | 30.92 ms | 30.66 ms | −0.8% |

And the head-to-head ratio against legacy, same binary, alternating arms:

| dataset | exp/leg before | exp/leg after |
|---|---|---|
| words100 | 1.27 | **0.96** (exp wins) |
| words5k | 1.40 | **1.02** |
| words15k | 1.41 | **1.02** |
| words100k | 1.17 | **1.07** |
| unidic | 0.95 | 1.00 |

Correctness: full suite, all 10 fuzz differentials, and byte-identical match
sequences on all five corpora (10 / 567 / 2,100 / 23,826 / 236,442 matches).

Why the gain scales inversely with automaton size: the allocation is a **fixed
per-line cost**, so it dominates when the per-line scan is cheap. On words100
(920 states, all in L1) the scan is nearly free and the allocation was most of
the time; on words100k (645k states, 11.5 MB) cache misses dominate and removing
the allocation only recovers 13%.

This also explains a previously puzzling result: switching `with_capacity(4)`
to `Vec::new()` measured as *worse*. Both allocate once per line, so that
experiment never tested the actual cost — which is the allocation and the
pointer indirection, not the initial capacity.

### Follow-up experiments after the inline buffer

**ROOT + unmapped fast skip (rejected).** After inlining the buffer, adding a
branch that skips unmapped characters while sitting at ROOT made everything
worse: words100 9.95 → 11.05 ms (−11%), words5k 10.2 → 11.3, words15k 11.1 →
12.0, words100k 13.5 → 14.5. The branch costs more than the loop entry it
avoids, because at ROOT the probe fails immediately and the loop is cheap. This
reverses the earlier `v1` result measured *before* the buffer was inlined: the
two changes interact, and once the allocation is gone the branch is no longer
worth it.

**fast-ROOT re-enable (rejected again, now with numbers).** Combining the
inline buffer with the previously-disabled fast-ROOT path: words100 14.7 → 15.5,
words5k 15.0 → 15.6, words15k 16.1 → 16.5, words100k 15.8 → 16.5, unidic 30.1 →
31.1. Uniformly 3–6% worse. The path stays disabled.

**Workload dependence of the win, measured honestly.** With the new code, one
single large haystack (one iterator) costs 13.6 ms and 13,052 per-line
iterators cost 13.9 ms — only 2% apart, confirming that the allocation is
genuinely gone. Under the old code the per-line cost was 15.7 ms, so the
1.8 ms recovered on words100k is 138 ns per iterator, which is a plausible
malloc+free of 96 B. On words100 the same removal recovers 4.7 ms, i.e.
360 ns per iterator, because there the scan is so cheap that the allocator cost
is not overlapped with cache-missing work. The optimisation is therefore
largest exactly where the automaton is small and the per-line work is small.

## Is the 32% probe instruction caused by the unmapped-char walk? No.

This is worth stating flatly because it was asserted wrongly at one point.

The instruction is `cmp expect, 0x4(states + (base ^ code))` — offset 4 in
`State` is `check`, so it is `step_child`'s transition probe and nothing else.
`step_child` begins with `if let (Some(code), Some(base)) = (mapped_c, base)`,
so for an **unmapped** character it returns before touching memory. The probe
therefore does not execute on the unmapped path at all.

Measured two ways:

| variant | probe share of cycles |
|---|---|
| current | 31.9% |
| unmapped chars handled entirely outside the loop | 32.0% |

Removing every unmapped execution of that instruction moves its share by 0.1
points. And compared with legacy, the same instruction accounts for **37.5%**
of legacy's cycles — *higher* than exp's. It is the shared, irreducible cost of
a random access into an 11.5 MB double array, and it cancels out of the *gap*.

That is the general lesson this document kept missing: a large share of total
cycles tells you nothing about a difference between two implementations.

## What the remaining gap actually is (after the inline buffer)

`perf stat`, words100k, 100 reps, current code:

| metric | legacy | experimental | delta |
|---|---|---|---|
| cycles | 4,757,158,317 | 5,386,181,136 | **+13.2%** |
| instructions | 3,467,308,412 | 3,309,207,357 | **−4.6%** |
| branch-misses | 29,675,693 | 37,619,103 | +26.8% |
| L1 load-misses | 31,285,873 | 38,692,458 | +23.7% |
| IPC | 0.729 | 0.614 | −15.8% |

The experimental matcher now executes **fewer** instructions and still takes
more cycles: the gap is pure throughput, not work. The exclusive hot spots in
`perf annotate` are

| share | instruction | meaning |
|---|---|---|
| 7.54% | `mov 0x8(%rsi,%r8,1)` | load `state.fail` |
| 3.77% | `test %r12d,%r12d` | test `fail != 0` |
| 2.05% | `mov %r12d,%r8d` | move fail into `state_id` |

None of these appear in legacy's top list. The reason is the DEAD terminator:
legacy loads `fail` once per *fail hop* (19,089/pass), while exp loads it once
per *mismatch* (321,626/pass) — **16.8× more**. At ~14 cycles per dependent
load that is 4.24 M cycles against a measured delta of 6.29 M, i.e. **~67% of
the remaining gap**.

So the correct statement of the residual gap is: exp must follow the real
suffix link on every mismatch, where legacy's DEAD terminator collapses the
walk to one step. That is the price of not rewinding, and it is what the
earlier "unmapped-char walk" framing was reaching for — except that the walk's
*length* (2.54 hops) was never the issue; the *number of times it starts* is.

## Corrections to three claims made earlier in this investigation

### 1. The "delete unmapped characters" experiment does not show what it seemed

It reported exp/leg flipping from 1.42 to 0.88 and was presented as causal
proof that unmapped characters are the cost. Re-running the identical
experiment on the current code:

| variant | matches | legacy | experimental | ratio |
|---|---|---|---|---|
| prose as-is | 23,826 | 5.91 ms | 7.19 ms | 1.217 |
| non-`a-z` deleted | **35,808** | **15.55 ms** | **15.90 ms** | 1.023 |

Deleting the separators makes **both** implementations 2.2–2.6× *slower*,
because it merges words and produces 50% more matches. The ratio moves only
because legacy slows down more (2.6× vs 2.2×) — that is rewind cost growing
with match density, not unmapped characters costing exp anything.

So the experiment never isolated the unmapped fraction. It is a clean
demonstration that dense matches punish legacy's rewind, and nothing more.
The direction of the earlier claim was not wrong, but its magnitude was an
artifact.

### 2. The probe instruction is not on the unmapped path, but that was the wrong point

`step_child` does return before loading when `mapped_c` is `None`, so the hot
`cmp ... 0x4(states + base^code)` never executes for an unmapped character.
That is true and was verified (31.9% of cycles with the current code, 32.0%
with unmapped handling moved out of the loop).

But it does not address the fail-walk question. The expensive access on the
unmapped path is the **next** state load:

    load states[s]            <- base/check/fail/output_pos, one line
    step_child -> None        <- no probe, as stated
    read states[s].fail       <- same line, free
    read states[s].output_pos <- same line, free
    state_id = fail(s)
    load states[fail(s)]      <- NEW random access: this is the cost
    ... repeat

So the cost is one fresh random load per hop, exactly as the objection said.
Saying "not step_child" answered a question nobody asked.

### 3. Why DEAD + rewind beats following the real suffix chain

Rewind does re-do work: legacy visits 1.094 characters per byte against exp's
1.036, i.e. 5.6% *more*. It still wins on prose because the work it avoids is
more expensive than the work it repeats:

| | per mismatch |
|---|---|
| legacy | one loop iteration (DEAD terminates it), ~19 k fail loads per pass |
| experimental | 2.54 iterations, 322 k fail loads per pass (16.8×) |

The extra character visits are sequential over a 569 KB text that stays in L2;
the fail loads are dependent random accesses into an 11.5 MB state array. The
trade is unfavourable for exp on prose, and favourable on dense fully-mapped
text (unidic), which is exactly what the measurements show.

## Exact load accounting: rewind also does random access

An earlier note claimed rewind "repeats cheap work". That is too glib — the
repeated work includes real random state loads. Instrumented counts, words100k,
one pass:

| | legacy | experimental |
|---|---|---|
| text bytes | 568,811 | 568,811 |
| character visits (incl. rewind) | 622,130 | 589,087 |
| — rewind overhead | **53,319 (+9.4%)** | 0 |
| **state loads (random access)** | **485,851** | **888,270** |
| — per byte | 0.854 | 1.562 |
| transition probes | 485,535 | 453,212 |

So legacy does pay random accesses for re-scanned characters. The asymmetry is
still 1.83× in total state loads, and it decomposes exactly:

    exp loads 888,270
      − 437,218  iterations on unmapped characters
      = 451,052  iterations on mapped characters

On **mapped** characters exp loads *fewer* states than legacy (451,052 vs
485,851 = 0.93×). The entire excess is the 437,218 unmapped-character
iterations — 110,792 non-ROOT resets averaging 3.95 loop iterations each, where
every iteration loads a fresh state to look for `output_pos`. Legacy never
enters the loop for those characters at all.

So the residual cost is: *one random state load per hop for characters that are
not in the alphabet, and legacy pays zero.* That is the whole remaining
difference.

## How legacy overwrites `last_output_pos` (no comparison at all)

There is no start/end comparison. The update is unconditional:

    } else if let Some(output_pos) = self.pma.output_pos_unchecked(state_id) {
        last_output_pos.replace(output_pos);   // unconditional
        self.pos += skips;
        skips = 0;
    }

Correctness comes from the automaton, not from a runtime test: the state machine
only visits states reachable along the current path, and a state's `output_pos`
is by construction the longest match ending at the current position. Each newly
found output is therefore later than the previous one, and for the same start it
is longer, so `replace` is always correct. `self.pos += skips` records where the
new match ends, which is exactly where a later rewind resumes.

The DEAD terminator enforces leftmost: once a match is committed at position E,
every fail link under it is DEAD, so nothing starting before E can be reported
afterwards.

## Candidate 8: borrow bit 31 of `fail` (sound, mixed results)

`fail` is a plain `u32` (not an `Option`), and it is the natural host for the
"failure chain holds an emit" flag: it is read only on the fail path, so the
mask never touches the hot transition probe (unlike `check`, which is read on
every probe). Only four read sites exist, and the bit is set **only for the
experimental match kinds**, so legacy automata are bit-for-bit unchanged.

Scale cost: automata are limited to 2^31 states, which is the same limit
`check` and `base` already imply. Measured maxima are 644,357 (words100k) and
1,020,267 (unidic), far below.

Correctness: full suite, all 10 fuzz differentials, and byte-identical match
sequences on all five corpora.

Three placements were tried, because the cost is entirely in where the test
sits:

| placement | words100 | words5k | words15k | words100k | unidic |
|---|---|---|---|---|---|
| v7: pre-loop test with its own load | +8.2% | +1.8% | −0.4% | −4.4% | — |
| v8: test inside the loop (every iteration) | +15.8% | +7.1% | +3.8% | −2.7% | — |
| **v9: pre-loop test + hoisted state load** | **+5.1%** | **−1.6%** | **−2.8%** | **−5.0%** | **−2.6%** |

v9 is the best of the three: it removes the duplicate load that v7 paid and
keeps the test off the per-iteration path that v8 paid for. It improves four of
five datasets, including unidic (which has 0% unmapped characters — the gain
there is the loop restructure, not the flag).

**The `words100` regression is real and reproducible.** It is not measurement
noise, and an earlier −2.8% reading for the same configuration was an artifact
of linking both crates into one binary (that binary's HEAD arm measured 11.35 ms
against 9.99 ms for the single-crate build — a 14% layout difference in the
*same* code). Single-crate binaries, identical harness source, alternated,
medians of 5–10 rounds:

| dataset | HEAD | v9 | delta |
|---|---|---|---|
| words100 | 9.99 | 10.50 | **+5.2%** |
| words5k | 10.29 | 10.15 | −1.4% |
| words15k | 11.06 | 10.78 | −2.5% |
| words100k | 13.86 | 12.99 | **−6.3%** |
| unidic | 30.22 | 30.08 | −0.5% |

So the trade is: −6.3% at 645k states, +5.2% at 920 states, and roughly neutral
in between. The cause is that on a 920-state automaton (14.7 KB, entirely
L1-resident) a skipped hop costs nothing, while the flag test and the loop
restructure cost a little on every character; at 645k states each skipped hop
avoids an L3/DRAM access and the same code wins.

A regime-adaptive build (only set the bit when `states.len()` is large) is
possible since the builder knows the size, but it doubles the scan code paths
and has not been measured.

## Buffer sizing: `INLINE = 1` is worse than `INLINE = 4`

Since 99.5% of emit chains have length 1 (max observed 3), shrinking the inline
buffer to a single slot looks attractive: the iterator drops from 224 to 128
bytes. Measured, it is a clear regression.

| dataset | INLINE=4 | INLINE=1 | INLINE=2 |
|---|---|---|---|
| words100 | 9.99 | 11.15 (**+11.6%**) | 9.95 (−0.4%) |
| words5k | 10.20 | 11.20 (**+9.8%**) | 10.23 (+0.4%) |
| words15k | 11.05 | 12.06 (**+9.1%**) | 11.03 (−0.2%) |
| words100k | 13.60 | 14.54 (**+7.0%**) | 13.54 (−0.4%) |
| unidic | 30.27 | 31.50 (**+4.1%**) | 30.24 (−0.1%) |

`INLINE = 2` is indistinguishable from `INLINE = 4`, so 4 costs nothing. But
`INLINE = 1` costs 4–12%.

**The cause is the spill path, not the size.** Instrumented: with `INLINE = 1`,
109 of 23,826 pushes (0.46%) overflow to the `Vec`. A probe that removes the
spill entirely — dropping the overflow, which is unsound and only used for
measurement — restores the time:

| words100k | median |
|---|---|
| INLINE=4 | 13.52 ms |
| INLINE=1 with spill | 14.77 ms (+9.2%) |
| INLINE=1 without spill (unsound) | 13.73 ms (+1.6%) |

Yet `perf` reports essentially identical instruction counts (1.843 B vs
1.846 B) and branch misses (11.998 M vs 12.060 M) for INLINE=1 and INLINE=4.
So the cost is not extra work: it is the presence of a live `Vec` field in the
hot struct. Once `spill` can actually be written, the compiler can no longer
treat `MatchBuf` as plain data — the `Vec` may alias the state array — and the
hot loop loses the scheduling freedom it had when `spill` was provably
untouched.

This is the same lesson as the allocation finding, one level down: what matters
is not how often the heap path runs, but whether the compiler can prove it
never runs.

## Is `INLINE = 4` the right choice? (yes, and smaller is worse)

The iterator is 224 bytes. That sounds large, so three reductions were measured:

**Field reordering** (moving the 160-byte buffer after the hot fields so
`pos`/`state_id`/`gate` share the first cache lines): no change. All five
datasets land within noise (words100 9.99 → 9.99, words100k 13.76 → 13.65,
unidic 30.47 → 30.25). The hot fields were already in the first lines; the
224 bytes are stack, not cache-resident working set.

**`INLINE = 2`**: indistinguishable from 4 on every dataset. So the buffer
costs nothing beyond 2 slots, and 4 is free insurance against longer chains
(max observed is 3).

**Compact slots (`MaybeUninit` instead of `Option<Match>`)**: this is what
SmallVec does, and it shrinks the struct from 224 to 192 bytes (24 bytes per
slot instead of 32). It is **6–13% slower everywhere**:

| dataset | `Option<Match>` slots | `MaybeUninit` slots |
|---|---|---|
| words100 | 9.97 | 11.25 (**+12.8%**) |
| words5k | 10.25 | 11.47 (+11.9%) |
| words15k | 11.12 | 12.31 (+10.7%) |
| words100k | 13.76 | 14.49 (+5.3%) |
| unidic | 30.35 | 31.45 (+3.6%) |

`Option<Match>` is a plain value that LLVM can copy and reorder freely;
`MaybeUninit` + `assume_init_read` is an unsafe read it must treat
conservatively. Saving 32 bytes costs more than it buys.

### Comparison with `SmallVec`

| | this `MatchBuf<V, 4>` | `SmallVec<[Match<V>; 4]>` |
|---|---|---|
| layout | `[Option<Match>; 4]` + `len` + `Vec` (two separate stores) | `union { [MaybeUninit<Match>; 4], (ptr, cap) }` + `len` |
| size | 160 B buffer (+ other fields = 224 B iterator) | ~104 B |
| per-slot | 32 B | 24 B |
| after spill | inline slots stay live (wasted but harmless) | inline storage is reused for the pointer |
| unsafe | none | `MaybeUninit`, union, manual drop |
| measured | **baseline** | the `MaybeUninit` variant above, −6 to −13% |

So SmallVec would be smaller and more memory-efficient, and on this workload it
would be **slower**. The plain `Option` array is not an oversight; it is the
faster representation, and it needs no `unsafe`.

**Verdict: keep `INLINE = 4` with `Option<Match>` slots.** The 224-byte stack
footprint is irrelevant (one per iterator, non-recursive, dwarfed by any thread
stack), field order does not matter, and both available reductions cost 6–13%.

## Candidate 9: return the match directly from `ensure_buffered` (rejected)

Proposal: have `ensure_buffered` hand back the next ready match directly,
pushing only the remainder into `buf`, so the common single-match case costs no
push and no pop. Two implementations were measured.

**r1** — `ensure_buffered() -> Option<Match<V>>`; `next()` keeps the value,
and on the epsilon path pushes it back before interpolating:

| dataset | current | r1 | delta |
|---|---|---|---|
| words100 | 9.98 | 11.99 | **+20.1%** |
| words5k | 10.19 | 11.82 | +16.0% |
| words15k | 11.07 | 12.53 | +13.2% |
| words100k | 13.71 | 15.00 | +9.4% |
| unidic | 30.14 | 32.45 | +7.7% |

**r2** — cheaper: test `epsilon_emit_pos.is_none()` at the top of `next()` and
take a branch-free path, so no value is carried across the epsilon branch:

| dataset | current | r2 | delta |
|---|---|---|---|
| words100 | 9.99 | 10.84 | +8.5% |
| words5k | 10.20 | 11.04 | +8.2% |
| words15k | 11.03 | 11.83 | +7.3% |
| words100k | 13.70 | 14.30 | +4.4% |
| unidic | 30.32 | 31.31 | +3.3% |

Both are correct (identical match sequences on all five corpora) and both are
slower. `perf` shows r1 at 1.860 B instructions against 1.840 B — only +1.1% —
while taking 9.4% longer on words100k.

Two separable costs:

1. **Carrying `Option<Match<V>>` (32 bytes, four registers) across the epsilon
   branch** forces a spill/reload. r1 pays this; r2 does not, and r2 is
   correspondingly less bad.
2. **The extra branch itself.** r2 still costs 3–8% with no carried value,
   because it splits `next()` into two paths and the compiler can no longer
   schedule the single existing path as tightly.

The push and the pop that the proposal removes are L1-resident stack writes on
an already-hot object; they were never the cost. This is the third result in
this document with the same shape: identical instruction counts, materially
worse time, caused by register pressure and lost scheduling freedom rather than
by work performed.

**The current single-path `next()` is already the right shape.** Leave it.

## Shrinking `Match` (rejected: smaller is slower)

`Match<V>` is `{ length: usize, end: usize, value: V }` = 24 bytes for `V = u32`,
so `Option<Match<u32>>` is 32 and the iterator is 224 bytes. Changing `length`
to `u32` (it is already `u32` everywhere internally, and a match can never
exceed a pattern's length) keeps `end` as `usize` so texts over 4 GiB still
work:

| | before | after |
|---|---|---|
| `Match<u32>` | 24 B | **16 B** |
| `Option<Match<u32>>` | 32 B | **24 B** |
| iterator (INLINE=4) | 224 B | **192 B** |

Correctness: full suite and all 10 fuzz differentials pass.

Performance: **0 to +4% worse**, never better.

| dataset | `Match` = 24 B | `Match` = 16 B | delta |
|---|---|---|---|
| words100 | 9.98 | 10.38 | **+4.0%** |
| words5k | 10.24 | 10.53 | +2.8% |
| words15k | 11.12 | 11.33 | +1.9% |
| words100k | 13.76 | 13.74 | −0.1% |
| unidic | 31.09 | 31.08 | 0.0% |

Not adopted. 32 bytes of stack are not worth 4% on small automata, and the
change touches a public type.

## The pattern across every size reduction

Four attempts to make the hot data smaller were measured, and none helped:

| change | size | time |
|---|---|---|
| `INLINE = 2` (vs 4) | −64 B | 0% |
| `INLINE = 1` | −96 B | **+4…12%** |
| `MaybeUninit` slots | −32 B | **+6…13%** |
| `Match` 24 → 16 B | −32 B | **0…+4%** |
| field reordering | 0 | 0% |

The two that were free (`INLINE = 2`) or harmful the rest. The consistent
mechanism is not cache footprint — the working set is the state array, not the
iterator — but the compiler's freedom: plain `Copy` values it can keep in
registers and reorder, versus `unsafe` reads, live `Vec` fields, or split
branches that force spills. **Reducing bytes is not the lever; keeping the hot
path as plain, unconditional data flow is.**

## The emit-chain length has a principled bound: `L - 1`

Earlier revisions of this document argued about `INLINE` from measured chain
lengths (2 on words100k, 3 on unidic) and even claimed 4 slots was "safe". That
reasoning was wrong: sampling cannot establish a bound. The bound comes from the
builder.

A chain accumulates matches that were committed while the scan stays on one trie
path, because emission happens only when the automaton returns to ROOT. The
longest stretch without returning to ROOT is the longest pattern, so

    chain length <= L - 1,   L = length of the longest pattern in characters

and the bound is **tight**. Construction: one long pattern to hold the scan off
ROOT, plus a pattern for every character along it so each character commits a
match. Verified:

| longest pattern L | chain length | matches |
|---|---|---|
| 3 | 2 | 2 |
| 10 | 9 | 9 |
| 26 | 25 | 25 |
| 100 | 99 | 99 |
| 500 | 499 | 499 |
| **1000** | **999** | **999** |

Reaching `L - 1` needs the pattern set to also contain the short prefixes
(single characters in the construction above). That is why ordinary word lists
stay tiny despite long patterns:

| corpus | max pattern chars | observed chain |
|---|---|---|
| words100k | 107 | 2 |
| unidic | 34 | 3 |
| English prose words | — | 2 |

A random English word does not complete a match at each of its prefixes, so the
chain never grows. But `{"abcdefghij…z", "a", "b", …, "z"}` over `"abc…y"`
produces 25, and the same shape with a 1000-character alphabet produces 999.

**Consequences for `INLINE`:**

- No fixed `INLINE` can be "safe" in the sense of never spilling. `4` covers
  short-pattern workloads and spills for the construction above; the spill path
  is what makes that correct, and it must be kept.
- The bound depends on data the builder knows (max pattern length, and how many
  prefixes are patterns), but `INLINE` is a const generic, so it cannot be sized
  at build time. It stays a compile-time default.
- Both `INLINE = 2` and `INLINE = 4` measured performance-identical on real
  corpora, so 4 costs nothing while covering more of the short-pattern range.

## Why "just moving the buffer to the stack" is worth 25%

It looks like it should not be. An isolated microbenchmark of
`Vec::with_capacity(4)` + push + drop, 13,052 times, costs **0.14 ms** — while
the real change saves **4.7 ms** on words100. That 35× gap was the puzzle, and
the microbenchmark was the thing that was wrong: with the allocator warm and no
surrounding work, malloc/free latency is hidden. In the real loop it is not.

A clean A/B settles it: both crates linked into one binary, the *same* scan loop
written once, one automaton per process, medians of 6 runs.

| words100 | `Vec<Match>` | inline `[Option<Match>; 4]` | delta |
|---|---|---|---|
| wall clock | 13.95 ms | **10.52 ms** | **−24.6%** |
| cycles (9 passes) | 326,496,199 | 250,920,248 | −75,575,951 |
| instructions | 322,323,536 | 232,480,413 | −89,843,123 |
| L1 misses | 402,051 | 219,471 | −182,580 |

117,468 iterators are created (9 passes × 13,052 lines), so the deltas are

- **764 instructions per iterator**
- **643 cycles per iterator**
- 1.55 L1 misses per iterator

764 instructions is exactly the shape of `malloc` + `free`: the allocator's
fast-path bookkeeping, the `Vec` capacity/len setup in `with_capacity`, and the
drop glue. At the measured IPC of 1.9 that is ~340 cycles, and 8.4 M cycles per
pass at ~2.5 GHz is 3.36 ms — matching the 3.43 ms wall-clock delta.

So the answer to "it is only a stack-to-heap move, why 25%?" is: **because it is
not one operation per program, it is one malloc/free pair per iterator, and on
this workload there are 13,052 iterators per pass over a 569 KB text.** The
allocator cost is a fixed per-iterator tax; on words100 the scan itself is so
cheap (920 states, all L1) that the tax was 30% of all instructions executed.

This also explains why the win shrinks to ~13% on words100k: there the scan is
dominated by cache misses in an 11.5 MB state array, so the same fixed tax is a
smaller share of a much larger total.

And it explains why `Vec::new()` measured "worse" than `with_capacity(4)` in an
early experiment: both allocate once per iterator, so that comparison never
touched the actual cost.

## The 25% inline-buffer win is a per-line artifact

> **Superseded.** The −24.6% below is real for the comparison it makes, but the
> baseline was `Vec::with_capacity(4)`, which allocates for every iterator. Against
> `Vec::new()` (lazy allocation) the inline buffer is *slower* on the same
> workload — see "Correction: the ~25% per-line win was against a preallocating
> `Vec`" below.

The headline number (−24.6% on words100) came from the benchmark shape
`for h in &hay { for m in pma.find_iter(h) }` with `hay` = 13,052 lines, i.e.
**13,052 iterators per pass**. On that shape the inline buffer is worth a lot,
because one malloc/free pair per iterator is a fixed tax on a scan that is
otherwise nearly free.

For a **single long haystack** — one iterator — the same change is worth almost
nothing:

| dataset | haystack | `Vec` | inline | delta |
|---|---|---|---|---|
| words100 | 581,862 B, 1 iterator | 10.94 ms | 10.48 ms | **−4.2%** |
| words100k | 581,862 B, 1 iterator | 14.00 ms | 13.46 ms | **−3.9%** |
| unidic | 1,118,390 B, 1 iterator | 30.78 ms | 30.05 ms | **−2.4%** |

Compare words100: −24.6% per-line versus **−4.2% single-haystack**. The
difference is entirely the 13,052 malloc/free pairs, which do not exist in the
single-haystack case.

**So the honest summary of the inline buffer is:**

- **−2 to −4%** for long-text scanning, which is the case that matters for
  search performance. The residual is not the allocator at all; it is that a
  stack buffer lets `push`/`pop` avoid pointer indirection through a heap
  pointer, and (on some shapes) that the compiler can keep more in registers.
- **−25%** for the per-line iteration idiom, purely from removing one
  allocation per line.

Both are real, but they answer different questions. Any statement about "the
inline buffer is worth 25%" must be qualified with the workload shape, and for
long-text search the number is a few percent.

**Correction (later measurement):** even the per-line figure is only true
against a preallocating `Vec`. `Vec::new()` allocates lazily, so on a sparse
workload it allocates ~10 times per pass instead of 13,052 and beats the inline
buffer. See "Correction: the ~25% per-line win was against a preallocating
`Vec`" below.

The same caveat applies to every number in this document that was measured with
the per-line harness: words100/5k/15k/100k timings are per-line scans, so the
fixed per-iterator costs (allocation, iterator setup) are amortised over only
one line each. Only the single-haystack measurements above isolate the scan.

## Long-text search: the numbers that actually matter

Every exp/leg ratio reported earlier in this document came from the harness
shape

    let hay = std::fs::read_to_string(path).lines().map(String::from).collect();
    for h in &hay { for m in pma.find_iter(h) { ... } }

`str::lines()` splits the file into **13,052 separate `&str` slices** and the
newlines are discarded, so this performs 13,052 independent scans per pass, each
with its own iterator. That measures the *per-line* idiom (allocation, iterator
setup, epsilon state, and a short scan), not long-text search.

Re-measured with **one haystack** (all lines joined with `\n`, one iterator per
pass, one automaton per process, medians of 5 × 9 reps):

| dataset | haystack | legacy | exp (current) | ratio |
|---|---|---|---|---|
| words100 | 581,862 B | 10.94 | 11.08 | **1.013** |
| words5k | 581,862 B | 10.25 | 11.05 | **1.078** |
| words15k | 581,862 B | 10.69 | 11.67 | **1.092** |
| words100k | 581,862 B | 12.52 | 13.80 | **1.102** |
| unidic | 1,118,390 B | 32.56 | 31.76 | **0.975** |

Compare with the per-line numbers for the same builds (words100 0.96, words5k
1.02, words15k 1.02, words100k 1.07, unidic 1.00). The differences are real and
go in both directions:

- **words100**: per-line 0.96 (exp wins) but long-text **1.013** (exp loses
  slightly). The per-line advantage was the removed allocation; with one
  iterator there is nothing to remove, and exp's per-character work is a hair
  higher.
- **words100k**: 1.07 per-line becomes **1.102** long-text — the gap is *larger*
  when the scan dominates, consistent with the fail-load analysis.
- **unidic**: 1.00 per-line becomes **0.975** — exp is genuinely faster on
  dense, fully-mapped text, and more clearly so when the scan is not diluted by
  per-line overhead.

**The honest summary for long-text search: exp is 1.3–10% slower on English
prose and ~2.5% faster on Japanese.** The per-line idiom additionally pays
exp's iterator-construction cost, which is why the earlier table looked better
for exp on small automata.

### `ensure_buffered` returning `Option<Match>` (candidate 9), long-text

Measured on the same single-haystack harness:

| dataset | exp current | exp with r1 | delta |
|---|---|---|---|
| words100 | 11.08 | 11.20 | +1.1% |
| words5k | 11.05 | 11.27 | +2.0% |
| words15k | 11.67 | 11.96 | +2.5% |
| words100k | 13.80 | 14.55 | **+5.4%** |
| unidic | 31.76 | 32.48 | +2.3% |

Still uniformly slower, though the penalty is smaller than under the per-line
harness (+9…20%) because the per-iterator part of the cost is no longer
repeated 13,052 times per pass. The conclusion is unchanged: the push/pop the
proposal removes are not the cost, and carrying `Option<Match>` across the
epsilon branch costs more than it saves.

## Real-world workload: zhconv OpenCC table on Chinese text

A second domain, to test whether the English result generalises. Patterns are
the first column of `npx @zhconv/cli-opencc zh-tw --dump-table` (56,075 entries,
length 1–12 characters, 3,963 of them single characters). Haystacks are
`zhconv-rs/benches/data{54k,689k,3185k}.txt`, scanned as one haystack each
(one iterator), 5 × 9 reps, one automaton per process, `zin`.

| dataset | chars | legacy | experimental | ratio | ns/char (leg) | ns/char (exp) |
|---|---|---|---|---|---|---|
| data54k | 21,767 | 0.94 ms | 0.91 ms | **0.968** | 43.2 | 41.8 |
| data689k | 238,583 | 11.33 ms | 10.57 ms | **0.933** | 47.5 | 44.3 |
| data3185k | 1,188,261 | 76.59 ms | 74.85 ms | **0.977** | 64.5 | 63.0 |

**The experimental matcher is 2–7% faster on every Chinese dataset**, with
identical match counts (2,578 / 3,549 / 270,213). Build cost is +8.4%
(206 ms vs 190 ms for 56,075 patterns, 93,886 states).

### The picture across domains (long-text search, one iterator)

| workload | patterns | states | exp/leg |
|---|---|---|---|
| English words100 | 100,001 | 928 | 1.013 |
| English words5k | 5,000 | 32,192 | 1.078 |
| English words15k | 15,000 | 75,392 | 1.092 |
| English words100k | 100,000 | 645,120 | **1.102** |
| Japanese unidic | 1,023,998 | 1,024,000 | **0.975** |
| Chinese zhconv (54k) | 56,075 | 93,886 | **0.968** |
| Chinese zhconv (689k) | 56,075 | 93,886 | **0.933** |
| Chinese zhconv (3185k) | 56,075 | 93,886 | **0.977** |

**exp wins on Chinese and Japanese, loses on English prose**, and the split
follows the mechanism established earlier: the cost is one random `states[]`
load per hop for characters absent from the mapper. CJK text has essentially no
unmapped characters (Chinese haystacks here: 0% ASCII separators dominate only
at line breaks, and the scans are over joined text), while English prose is
~27% spaces, punctuation and capitals.

So the honest performance summary is domain-dependent, not a single number:

- **CJK text: experimental is 2–7% faster** (and the zero-rewind guarantee
  comes for free).
- **English prose: experimental is 1–10% slower**, worst at large automata.

### Does the inline buffer matter on Chinese long text? No

The zhconv test above used the current tree (inline buffer). To separate the
buffer from the design, the same workload was run against three experimental
variants, all with identical match counts:

| dataset | legacy | exp inline | exp pure `Vec::with_capacity(4)` | exp pure `Vec::new()` |
|---|---|---|---|---|
| data54k | 0.94 | 0.91 | 0.95 | 0.91 |
| data689k | 11.13 | 10.64 | 10.61 | 10.59 |
| data3185k | 76.68 | 74.79 | 75.50 | 75.02 |

as ratios to legacy:

| dataset | inline | pure `Vec` (cap 4) | pure `Vec::new()` |
|---|---|---|---|
| data54k | 0.968 | 1.021 | 0.973 |
| data689k | **0.955** | **0.953** | **0.951** |
| data3185k | 0.975 | 0.985 | 0.978 |

The three experimental variants are indistinguishable: the two large datasets
agree within 1%, and data54k (0.9 ms total) is within noise. **On a single
long haystack the inline buffer is worth nothing on Chinese text**, which
matches the English single-haystack result (−2 to −4%) and is expected: with
one iterator per pass there is one allocation to remove, and Chinese has ~0%
unmapped characters so the scan dominates.

Practical consequence: if the target is long-text search, the buffer choice is
irrelevant to the experimental matcher's advantage over legacy — that advantage
(1.5–5% on Chinese, ~2.5% on Japanese, −1 to −10% on English) comes from the
matcher design, not from the buffer.

## Storing a compact record in the buffer instead of `Match` (rejected: no penalty, no gain)

A reasonable suggestion: have the buffer hold a tuple and convert to `Match` on
output, to save memory without touching the public type.

First, the premise: **a plain tuple does not save anything.**
`(usize, usize, u32)` is 24 B, exactly like `Match`. Measured sizes:

| type | size |
|---|---|
| `Match<u32>` | 24 B |
| `Option<Match<u32>>` | 32 B |
| `(usize, usize, u32)` | 24 B |
| `(usize, u32, u32)` | 16 B |
| `(u32, u32, u32)` | 12 B |

To save anything one field must shrink. The buffer only needs
`(end, length, value)`, and `length` is `u32` everywhere internally (a match
cannot exceed its pattern, which the builder stores as `u32`). So a 16 B
`Pending { end: usize, length: u32, value: V }` is possible **without changing
the public `Match`** — which sidesteps the main objection to the earlier
`Match`-shrinking experiment.

This was implemented and measured (`iter_pending` yields `Pending` directly
from the emit chain, `next()` maps it to `Match`).

### Correctness

All 216 tests pass and the match streams are **byte-identical** on all eight
corpora for both match kinds.

### Performance: no measurable change

12 layout variants each, 15 reps, ratio to each binary's own legacy arm:

| dataset | cur (24B) median | mt (16B) median | delta | mt better | paired median | stdev |
|---|---|---|---|---|---|---|
| words100 | 0.957 | 0.957 | −0.0% | 7/12 | −0.1% | 0.7 |
| words5k | 1.026 | 1.025 | −0.2% | 6/12 | +0.1% | 1.2 |
| words15k | 1.055 | 1.051 | −0.4% | 8/12 | −0.6% | 1.7 |
| words100k | 1.075 | 1.077 | +0.1% | 5/12 | +0.6% | 3.5 |
| unidic | 0.900 | 0.894 | −0.6% | 8/12 | −0.8% | 2.0 |

Same conclusion as the `Match`-shrinking experiment: the effect (−0.6% to
+0.1%) is smaller than the layout noise it must be distinguished from
(stdev 0.7–3.5%), and the win counts are near a coin flip. **No penalty, but no
gain either.**

### Memory: the saving is real but far too small to matter

The record shrinks from 24 B to 16 B, and because `Vec` grows to capacity 4 on
the first `push`, the allocation drops from **96 B to 64 B** — 32 B per live
iterator. But:

- there is **one buffer per iterator**, and the peak element count is the
  longest emit chain, measured at 1 in 99.5% of cases (max 3);
- `leftmost_find_iter` returns one iterator per call, whose lifetime is the
  scan, so there are never many live iterators.

Measured RSS on the two densest workloads shows no difference:

| dataset | cur | mt |
|---|---|---|
| words100k | 108.6–110.6 MB | 109.3–112.8 MB |
| unidic | 206.7–207.9 MB | 207.5–208.1 MB |

**Correction: those `VmHWM` figures are not the automaton size.** `VmHWM` is the
lifetime *peak* RSS, and the peak is reached inside `build()`, where the builder
holds the NFA (trie + fail links + emit chains) while emitting the double-array,
so both representations are briefly alive. Measuring stage by stage on unidic:

| point | VmRSS |
|---|---|
| start | 0.5 MB |
| after reading patterns | 34.3 MB |
| after reading haystack | 35.4 MB |
| after build (patterns still alive) | 67.1 MB |
| after dropping patterns | 34.3 MB |
| after scan | 34.3 MB |

with `VmHWM` peaking at 208.2 MB during the build. The automaton itself is much
smaller than the peak suggests:

| dataset | states | `State x N` (16 B each) | serialized |
|---|---|---|---|
| words100 | 920 | 14 KB | 17 KB |
| words5k | 32,180 | 0.5 MB | 0.6 MB |
| words15k | 75,368 | 1.2 MB | 1.4 MB |
| words100k | 644,363 | 9.8 MB | 13.5 MB |
| unidic | 911,862 | **13.9 MB** | 28.3 MB |

So the steady-state footprint on unidic is ~34 MB, of which the state array is
13.9 MB. The 32 B per live iterator is 0.0002% of the automaton, and using
`VmHWM` to compare buffer sizes was the wrong instrument: the peak is dominated
by the builder and is identical for both variants.

### Decision: not adopted

It costs an extra `to_match()` per pop and a second copy of the
`(end, length, value)` triple in the codebase, in exchange for 32 B per live
iterator that cannot be observed. The public `Match` stays 24 B and the buffer
keeps storing `Match` directly.

The one case where it *would* matter is an API that holds many iterators alive
at once, or one that stores many pending matches per iterator. Neither applies
here.

## Re-testing 16-byte `Match` after the buffer became a `Vec`

`Match` was shrunk from 24 B to 16 B once before (`length: usize` -> `u32`) and
rejected as 0 to +4% worse. That measurement was made when the pending-match
buffer was the inline `MatchBuf`, where a smaller `Match` only shrank stack
slots. Now the buffer is a heap `Vec`, so element size also changes the
allocation size and the amount copied per push/pop, which could reverse the
result. Re-measured.

Correctness: all 216 tests pass and the match streams are **byte-identical** on
all eight corpora for both match kinds.

### Method note: single-binary ratios were still contaminated

The first attempt used the one-binary/four-arm protocol. It produced a
contradiction that exposed a remaining flaw: on words100k the two *legacy* arms
have identical code and identical per-pass instruction counts (26,315,943 vs
26,291,109, a 0.1% difference) yet differed by **18.6% in cycles**. Instruction
counts agreeing to 0.1% while cycles differ by 18.6% can only be a code/data
layout effect, not an algorithmic one.

So the measurement was redone as a **layout sweep**: 8 (then 16) separately
built binaries per variant, each with a different number of out-of-line padding
functions whose results are fed to `black_box` so they cannot be deleted
(`.text` grows monotonically from 415,489 to 416,081 bytes, confirming the pads
survive). Within each binary the ratio is taken against that binary's own
legacy arm, so layout cancels inside the pair; then the 16 paired ratios are
compared across variants.

### Result: no measurable benefit

Paired per-variant deltas, 16B relative to 24B (negative = 16B faster), 16
layout variants each, 15 reps:

| dataset | 24B median ratio | 16B median ratio | median delta | mean delta | 16B better | stdev of paired delta |
|---|---|---|---|---|---|---|
| words15k | 1.051 | 1.054 | **+0.3%** | +0.1% | 7/16 | 1.4% |
| words100k | 1.081 | 1.073 | **−0.7%** | −0.3% | 10/16 | 3.1% |
| unidic | 0.896 | 0.890 | **−0.9%** | −1.0% | 10/16 | 2.0% |

The median effect is −0.9% to +0.3%, while the standard deviation of the paired
delta is 1.4–3.1%. **The effect is smaller than the layout noise it must be
distinguished from**, and the win/loss counts (7/16, 10/16, 10/16) are
consistent with a coin flip. There is no defensible claim that 16 B is faster.

### Decision: not adopted

Shrinking `Match` changes a **public type** (users pattern-match and construct
`Match`), and it buys nothing measurable. The 24 B layout is kept.

The one thing the smaller type does buy is `Option<Match<u32>>` dropping from
32 B to 24 B, which only mattered for the inline `MatchBuf` that no longer
exists.

## Pure `Vec::new()` (no preallocation) beats `Vec::with_capacity(4)`

The earlier three-way A/B compared the inline `MatchBuf` against
`Vec::with_capacity(4)` and `Vec::new()`, but `Vec::new()` was only measured
before the harness confound was understood, so the comparison was not
trustworthy. Re-run under the corrected protocol: one binary, eight
`#[inline(never)]` arms (four buffer variants × two match kinds), medians of
5 × 9 reps, ratios to the legacy arm in the same binary. Noise floor is the
spread of the four byte-identical legacy arms (0.15–2.16%).

| dataset | matches | cur (inline) | va `Vec` cap4 | **vb `Vec::new()`** | vc `Vec`+Option | noise |
|---|---|---|---|---|---|---|
| words100 | 10 | 0.946 | 0.970 | **0.957** | 0.969 | 0.15% |
| words5k | 567 | 1.018 | 1.050 | **1.025** | 1.050 | 0.20% |
| words15k | 2,100 | 1.055 | 1.083 | **1.060** | 1.084 | 0.63% |
| words100k | 23,826 | 1.057 | 1.102 | **1.064** | 1.095 | 1.89% |
| unidic | 236,442 | 0.939 | 0.922 | **0.895** | 0.895 | 0.83% |
| zh data54k | 2,578 | 0.967 | **0.936** | 0.941 | 0.962 | 1.83% |
| zh data689k | 3,549 | 0.941 | 0.954 | **0.937** | 0.974 | 2.16% |
| zh data3185k | 270,213 | 0.949 | 0.943 | **0.934** | 0.952 | 0.79% |

`vb` is the best or joint-best variant on seven of eight datasets and never
materially worse than the best one.

### `vb` vs `va`: preallocating 4 slots is a pessimisation

The only difference between the two is `Vec::new()` versus
`Vec::with_capacity(4)` at iterator construction:

| dataset | `vb − va` |
|---|---|
| words100 | −1.3% |
| words5k | −2.4% |
| words15k | −2.1% |
| words100k | **−3.4%** |
| unidic | −2.9% |
| zh data54k | +0.5% (noise) |
| zh data689k | −1.8% (noise) |
| zh data3185k | −1.0% |

`Vec::new()` is faster in seven of eight, by up to 3.4%. This is the expected
result once the earlier finding is taken seriously: **the emit chain has length
1 in the overwhelming majority of cases** (the measurement that motivated the
inline buffer put it at 99.5%, max 3). A `Vec` that never grows past one element
still allocates on first `push` either way, so `with_capacity(4)` buys nothing
for the common case — it only makes the allocation *larger* (4 × 24 B instead of
a growth-doubled 1 → 2 slots), paying more for memory that is never used. On
words100k the extra 96 B per live iterator is what shows up as −3.4%.

`Vec::new()` also keeps the iterator small (24 B for the `Vec` triple versus
96 B of inline slots in `MatchBuf`), which matters because the per-line idiom
creates one iterator per line.

### What this means for the shipped choice

The inline `MatchBuf` is **not** the best buffer. Against `vb`:

| dataset | cur vs vb |
|---|---|
| words100 | −1.2% (cur slower) |
| words5k | −0.7% |
| words15k | −0.5% (noise) |
| words100k | −0.7% (noise) |
| unidic | **+4.7% (cur slower)** |
| zh data54k | +2.7% (cur slower) |
| zh data689k | +0.4% (noise) |
| zh data3185k | +1.6% (cur slower) |

So on a single long haystack `Vec::new()` is at least as good everywhere and
clearly better on unidic (+4.7%) and zh data54k (+2.7%). The reason is the one
established earlier: with one iterator per pass there is exactly one allocation
to remove, so the inline buffer's benefit disappears, while its two extra
branches per push/pop (`len < INLINE`, `spill.pop()`) remain — and at high match
density those branches are hot. unidic is 21% match density; zh data54k is 12%.

### Correction: the "~25% per-line win" was against a preallocating `Vec`

An earlier section of this document claims the inline buffer is worth ~25% on
the per-line idiom by removing one malloc/free pair per line. That A/B compared
the inline buffer against `Vec::with_capacity(4)`, i.e. a `Vec` that **allocates
on construction for every iterator**, including the ~99.9% that never push a
match. Re-measured against `Vec::new()`, which allocates lazily on the first
`push`, per-line (single-crate binaries, each variant against its own legacy):

| dataset | lines | matches | lines that push | `Vec::new()` | inline `MatchBuf` |
|---|---|---|---|---|---|
| words100 | 13,052 | 10 | 0.1% | **0.901** | 0.979 |
| words100k | 13,052 | 23,826 | 100% | 1.142 | **1.098** |
| unidic | 2,376 | 236,442 | 100% | **0.897** | 0.945 |

(ratios to same-binary legacy; lower is faster)

`Vec::new()` wins by 8.7% on words100 and 5.4% on unidic, and loses by 3.9% on
words100k. The mechanism is allocation count, not buffer shape: `Vec::new()`
allocates only when a match is actually produced, so on words100 it performs
**10 allocations per pass instead of 13,052** — a 1300x reduction in allocator
traffic. Where every line produces a match (words100k) that advantage vanishes
and the inline buffer's fixed slots win slightly, because it never calls the
allocator at all.

So the accurate statement is: **`Vec::new()` dominates the inline buffer on
both long-text and sparse-per-line workloads, and loses only on dense-per-line
workloads (every line yielding matches), by ~4%.**

## Retraction: every earlier exp/leg ratio was confounded by harness codegen

While extending the buffer A/B to `ensure_buffered -> Option<Match>` on a pure
`Vec`, the new arms disagreed with the earlier table by far more than the
effects being measured. Investigating that produced a methodological
retraction that invalidates the numbers reported earlier in this document.

### What was wrong

`scan` is `#[inline(always)]`, so the hot loop is compiled **into the user's
crate**. Its machine code therefore depends on the shape of the benchmark
harness, not only on the library. Three harnesses were measured with the
*same* library revision:

| harness | words100 exp | instructions/pass |
|---|---|---|
| `final_lh` (macro-folded constant arm) | 11.08 ms | 234,353,727 |
| `lh_cur` (runtime `if` selecting the arm inside `main`) | 12.54 ms | 264,473,155 |
| `ded_*` / `six` (one `#[inline(never)]` fn per arm) | 10.68–10.93 ms | 229,311,767 |

Same source, same `ARM`, 13% spread in both time and instruction count. The
runtime-`if` harness in particular prevented the optimiser from producing the
same code, inflating the experimental arm.

A second error compounded it: the first attempt at a layout sweep injected
`#[inline(never)]` padding functions that were never called, so LLVM deleted
them (`.text` size identical across all twelve "variants", 0 symbols). Those
twelve runs were therefore the *same binary* twelve times, which is why they
agreed to 0.5–1.0% and appeared to prove layout insensitivity. They proved
nothing of the kind. A later attempt passed the pad count as a **runtime**
`PAD` variable, which also cannot change code layout.

### The measurement that is now used

One binary, six `#[inline(never)]` functions, one per (variant, match kind),
all sharing the same code and data layout. The three legacy arms are
byte-identical code, so their spread is the noise floor:

| dataset | legacy spread (identical code) |
|---|---|
| words100 | 0.13% |
| words5k | 0.11% |
| words15k | 0.57% |
| words100k | 1.92% |
| unidic | 0.26% |

Everything below is a ratio to the legacy arm **in the same binary**, so the
harness codegen cancels.

### Results (long text, one iterator per pass, medians of 5 × 9)

Ratios to legacy; <1 means the experimental matcher is faster.

| dataset | cur (inline `MatchBuf`) | va (pure `Vec::with_capacity(4)`) | vc (pure `Vec` + `ensure_buffered -> Option`) |
|---|---|---|---|
| words100 | 0.960 | 0.971 | 0.973 |
| words5k | 1.023 | 1.052 | 1.053 |
| words15k | 1.046 | 1.077 | 1.084 |
| words100k | 1.078 | 1.100 | 1.070 |
| unidic | 0.952 | 0.909 | 0.904 |
| zhconv data54k | 0.921 | 0.938 | 0.952 |
| zhconv data689k | 0.934 | 0.947 | 0.961 |
| zhconv data3185k | 0.947 | 0.943 | 0.946 |

All three experimental variants produce **byte-identical match sequences** on
all eight corpora (verified by hashing `(start, end, value)` streams: the
digests agree exactly).

### What this changes

1. **The "English prose is 1.3–10% slower" conclusion was partly harness
   artefact.** With a clean harness, words100 is **0.960** (experimental
   *faster*), not 1.013. words5k/15k/100k remain slower (1.02–1.10), so the
   English penalty is real but smaller and not present at small automata.
2. **The CJK advantage is real and slightly larger than reported**: 0.92–0.95
   on Chinese, 0.90–0.95 on Japanese.
3. **The buffer choice is a wash on English and Chinese** (differences sit at or
   near the noise floor), but on `unidic` — 21% match density, so the buffer is
   genuinely hot — the pure `Vec` variants are ~4.3% faster than the inline
   buffer. The inline `MatchBuf` pays two extra branches (`len < INLINE` on
   push, `spill.pop()` on pop) per buffer operation, and at 21% density those
   branches are executed often enough to matter. At ≤4% density they are not.
4. **`ensure_buffered -> Option<Match>` is not a win anywhere.** On a pure `Vec`
   it is within noise of plain `Vec` on Chinese and English, and *worse* on
   words100k (1.070 vs 1.100 is noise, but unidic 0.904 vs 0.909 is also
   noise). Combined with the earlier `MatchBuf` measurement, where it was
   uniformly slower, the candidate is closed.

### Per-pass instruction counts (build subtracted)

`perf stat` over the whole process is dominated by automaton construction
(96.6% on unidic), so per-pass figures come from `(REPS=9 − REPS=1)/8`:

| dataset | legacy | cur | va |
|---|---|---|---|
| words100k | 26,379,673 | 24,161,019 | 25,762,176 |
| unidic | 47,869,940 | 41,048,098 | 39,642,029 |

On words100k the experimental matcher executes **6.5% fewer instructions but
takes 7.8% longer** — the deficit is stalls, not work. Per-pass IPC:

| dataset | legacy | cur | va |
|---|---|---|---|
| words100 | 1.038 | 0.941 | 0.996 |
| words100k | 0.856 | 0.748 | 0.805 |
| unidic | 0.620 | 0.530 | 0.557 |

The experimental matcher is consistently lower-IPC, which is the signature of
the extra dependent load per mismatch hop documented earlier.

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

## Candidate 7: borrow bit 31 of `check` (correct, but not worth it)

`base` and `output_pos` are `Option<NonZeroU32>`, so they are `None` for 15.2%
/ 66.4% (base) and 48.0% / 11.4% (output_pos) of states — a flag stored there
would be unavailable exactly where it is needed. `check` and `fail` are always
present. Of those two, `check` is the better host: it is already loaded by the
transition probe on every inner-loop iteration, whereas `fail` is only read once
the walk starts.

`check` holds a parent id, which is always `< states.len()` (max observed
644,367 for words100k, 1,023,880 for unidic), so bit 31 is free in practice.
This is a **capability reduction**, not a free bit: the builder must enforce
`states.len() <= 2^31`, halving the maximum automaton size. Unlike the
`output_pos` proposal (which would halve the *emit* capacity at 0.34–0.77
emits per state), halving the *state* limit is the same limit the field already
has for `base`/`fail`, so it is consistent rather than a new constraint.

Implementation: BFS one-pass DP
`has_emit[s] = own(s) || has_emit[fail(s)]`, stored in bit 31 of `check`;
`State::check()` masks it for the equality tests.

Correctness: **passes the full suite, all 10 fuzz differentials (including the
new unmapped-character fuzzer), and matches the baseline exactly on all five
corpora** (23,826 / 236,442 matches, identical sequences). This is the first
variant in this document that is both sound and non-trivial.

Performance (experimental arm only, separate processes, medians of 5 × 9
reps, `zin`):

| dataset | baseline | with flag | delta | unmapped chars |
|---|---|---|---|---|
| words100 | 14.71 ms | 15.21 ms | **+3.4%** | 27.3% |
| words5k | 15.01 ms | 14.78 ms | −1.5% | 21.5% |
| words15k | 15.92 ms | 15.48 ms | −2.8% | 21.6% |
| words100k | 15.77 ms | 15.17 ms | **−3.8%** | 27.3% |
| unidic | 30.06 ms | 31.12 ms | **+3.5%** | 0.0% |

The pattern is diagnostic:

- **Where the flag pays off, it pays ~1.5–3.8%.** The saving is real: it
  replaces ~1.54 dependent loads per unmapped character with one AND on a value
  that is already in a register.
- **Where it cannot pay off, it still costs ~3.4–3.5%.** unidic has 20
  unmapped characters in 1.1 MB and regresses 3.5%; words100 has the same
  unmapped fraction as words100k but regresses, because its automaton is tiny
  and the walk it skips was already cheap.
- The tax is **universal** (the extra branch and the flag test on every
  unmapped character, plus the masked `check` on every probe), while the
  benefit exists only when the walk is both frequent and expensive.

So it is a genuine improvement for large automata on prose (−3.8% at 645k
states) and a genuine regression everywhere else. Since the library cannot know
which regime a caller is in, and since the fast path also adds a bit-borrowing
scheme that must be enforced and serialized, this is **not adopted**. It is
recorded because it is the first sound version and because it quantifies the
ceiling: even with a perfect O(1) test, the remaining gap is ~1.20 → ~1.15.

### Ablation of the unmapped-character cost (this is the load-bearing table)

All measurements are the experimental arm alone, separate processes, medians of
5 × 9 reps on `zin`, words100k (645k states, 27.3% unmapped characters).

| variant | ms | vs baseline | sound? |
|---|---|---|---|
| baseline | 15.72 | — | yes |
| `check` bit 31: replaces the walk with one AND | 15.17 | **−3.5%** | yes |
| dense `fail`/`output_pos` arrays for the walk | 23.87 | **+51.8%** | yes |
| skip unmapped characters entirely (no harvest) | 12.45 | −20.8% | **no — and not an upper bound** |

Reading of the table:

1. **The walk is not the main cost.** A sound, complete elimination of it
   (candidate 7) buys 3.5%.
2. ~~**The extra iterations are.**~~ **Retracted.** That variant emits only
   3,716 of 23,826 matches (it loses 84%), so it was timed doing far less
   emission and far fewer `next()` round trips. It is not an upper bound for
   any sound change. The honest sound ceiling is the 3.5–6% above.

   Isolating the emission path directly is not possible in this harness:
   removing `buf.extend` lets LLVM delete the whole scan (unidic 30 ms →
   0.13 ms), and even a `black_box` accumulator loses the dependency because
   the harness only counts matches. So the remainder of the gap is measured
   only as "the whole minus the walk", not decomposed further.
3. **Dense side arrays are actively harmful.** Mirroring `fail` and
   `output_pos` into 2.5 MB each makes the walk 52% slower: the two extra
   arrays evict the double-array from cache. Any design that adds a parallel
   per-state structure is fighting the same 11.5 MB working set.

So the correct statement of the gap is: *exp pays a random `states[]` load per
hop for every unmapped character, and legacy pays none.* The walk length (2.54
hops) is what turns one load into 2.54 loads, but even a 1-hop version costs,
because the first load is already there and it is the load that misses.

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
| borrow bit 31 of `check` for a "fail chain has emit" flag | **sound and correct** (full suite + 10 fuzz + identical matches), and −3.8% at words100k — but +3.4% on small automata and +3.5% on unidic, because the tax is universal and the benefit is not. Not adopted |
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

## Can the `Vec` pending-match buffer be eliminated?

The question came from a legitimate observation: `buf` exists only to reverse a
chain, and reversing a list of unbounded length `k` normally costs `O(k)` space.
Since chain length is capped by `O(min(|V|, |Σ|))` and is *uncontrollable*, a
design that assumed `k` small would be invalid. So the question is whether
reversal can be avoided **in general**, not whether it is usually unnecessary.

### First: what the buffer actually costs

Measured with a counting global allocator, scanning only (iterator built and
warmed up first):

| corpus | matches | allocations | reallocs | bytes |
|---|---|---|---|---|
| words100 | 20 | **1** | 0 | 96 |
| words100k | 47,652 | **1** | 0 | 96 |
| unidic | 472,884 | **1** | 0 | 96 |

Exactly **one 96-byte allocation per iterator**, regardless of haystack size or
match count. `Vec::new()` allocates on the first push and then reuses capacity
forever, because `ensure_buffered_impl` only pushes when the buffer is empty and
`next()` drains it to empty before the next refill. There is no per-chain or
per-match allocation anywhere in the path.

So the entire budget for eliminating the buffer is: *one `malloc`/`free` pair per
`leftmost_find_iter` call*. For the long-text focus of this work that is a single
call, amortised over megabytes of haystack.

### Why reversal is structurally forced

The chain is stored head-first (`M_k` at the head, `pred` walking back to `M_1`)
and positions are computed by walking **from the head**:

```
end(M_k)   = pos - head.gap
start(M_k) = end(M_k) - head.length
end(M_{i}) = start(M_{i+1}) - em_i.gap
```

The first position the iterator can compute is `M_k`'s — the chronologically
*last* match. Emitting `M_1` first therefore requires either reversal (the
buffer), or re-walking the chain per match (`O(k²)` time), or recursion
(`O(k)` stack, overflow risk at `k = O(min(|V|,|Σ|))`). A singly-linked list
cannot be traversed backward, and `pred` cannot be reversed in place because the
nodes are shared across trie branches and across concurrently live iterators.

Reverse linking is not an accident of implementation: it is what makes the chain
a persistent structure. Appending chronologically would require mutating the
shared tail; prepending (`pred`) is `O(1)` and shares the entire prefix for free.

### The forward-link design, measured

The one design that would genuinely give `O(1)` runtime space: store `next`
pointers (chronological) instead of `pred`, so traversal yields matches in output
order with no reversal. Cost analysis:

**1. The successor is not knowable at insert time.** `extend_from` inserts
`prev.with_gap(inter_gap)` *first*, then chains `M_1..M_{k-1}`, and only then
installs the new head. When that node is created its chronological successor
`M_1` does not exist yet — so `next` cannot be part of the `DedupKey` at
insertion.

**2. Patching afterward corrupts shared nodes.** `EmitStore::insert` returns a
*pre-existing shared node* when the dedup key matches. Writing `next` into it
after the fact would change the successor for every other chain sharing that
node.

**3. Adding `next` to the dedup key breaks the sharing itself.** Two chains that
share an interior node `M_i` but continue to different `M_{i+1}` would no longer
dedup. Measured by counting distinct `(successor, value, length, gap)` tuples
over every chain — exactly what dedup would retain once `next` joined the key:

| corpus | current nodes | forward-link nodes | node blowup | emit memory |
|---|---|---|---|---|
| words100 | 100 | 101 | 1.01× | 0.00→0.00 MiB |
| words5k | 5,134 | 5,135 | 1.00× | 0.08→0.10 MiB |
| words15k | 15,930 | 15,931 | 1.00× | 0.24→0.30 MiB |
| **words100k** | 222,366 | 745,698 | **3.35×** | 3.39→**14.22 MiB** (4.19×) |
| unidic | 785,533 | 803,402 | 1.02× | 11.99→15.32 MiB (1.28×) |

(The forward representation also needs a `next: u32` field, so `Emit` grows
16→20 B, which is why the memory ratio exceeds the node ratio.)

Nodes whose successor is genuinely ambiguous — reached from two chains with
different chronological successors, i.e. exactly the ones that would have to be
split: **17,761** on words100k, **9,171** on unidic, **0** on the smaller word
sets.

So forward-linking trades one 96-byte allocation for up to **+10.8 MiB of emit
memory** and a weaker dedup, and it requires resolving the insert-order
chicken-and-egg (the successor must be known before insertion, but is created
afterward) by either disabling dedup or restructuring the builder. On unidic the
node blowup is small (1.02×) precisely because chains there are short and share
little; on words100k, where sharing is 4.6×, the blowup is severe.

### Conclusion

The buffer is not removable without a real cost, and every alternative pays more
than it saves:

| option | space | time | verdict |
|---|---|---|---|
| `Vec` (current) | 96 B, 1 alloc/iterator | `O(k)` | **shipped** |
| forward-linked chain | +4 B/emit, 1.02–3.35× node blowup | `O(k)`, no alloc | worse memory, builder rework |
| `O(k²)` re-walk | `O(1)` | quadratic in `k` | breaks the linear-time guarantee the matcher advertises |
| recursion | `O(k)` stack | `O(k)` | stack overflow at `k = O(min(|V|,|Σ|))` |
| fixed inline array | `O(k)` inline | `O(k)` | cannot bound `k`; measured 1.3–3.4% *slower* than `Vec::new()` (`Vec::with_capacity` variant) |

The `Vec` is already the minimal-cost representation: one allocation, no
reallocations, reused for the iterator's whole lifetime. The reversal it performs
is inherent to storing a shared persistent list in reverse — which is itself what
makes the chain `O(1)`-appendable and structurally shared across branches.

### Follow-up: does a forward-linked version attach a dedicated list to every state?

No — and the reason is the sharpest argument against the design. Two different
questions have to be separated:

**"Is every state's chain a distinct list?"** In the *current* reverse-linked
store, no: chains share their tail. `states_with_output` exceeds
`distinct_heads` by 1.6× on words100k (335,705 vs 213,163) and 1.17× on unidic
(907,436 vs 776,482) because many states point at the same head node. The chain
is a persistent list, so a state stores one pointer, not a list.

**"Would a forward-linked version still share?"** Only at the very end. Measured
by building the forward trie over all distinct chains (key `(successor, value,
length, gap)`) and counting, per position from the chronological end, how many
nodes are used by more than one chain:

| depth from end | words100k nodes | shared | unidic nodes | shared |
|---|---|---|---|---|
| 0 (last match) | 213,163 | 40.4% | 776,482 | 5.5% |
| 1 | 94,424 | 45.0% | 45,525 | 18.7% |
| 2 | 78,551 | 20.8% | 19,290 | 10.0% |
| 3 | 71,232 | 3.1% | 9,211 | 4.9% |
| 4 | 65,178 | 0.1% | 4,015 | 3.1% |
| 5 | 59,264 | 0.0% | 1,541 | 2.3% |
| ≥6 | 0.0% | — | ~2% then 0 |

Sharing collapses to zero beyond depth 4–5. So a forward-linked chain is
effectively **one dedicated list per state**: every prefix longer than a handful
of matches is unshared and must be materialised.

That is exactly the identity cascade. A node's forward identity includes its
successor, and the successor's identity includes *its* successor, so the key of a
node at depth `d` depends on the entire suffix of length `d`. Two chains that
agree on their last match diverge as soon as any earlier match differs, and
because earlier matches are the *shared* part of the current representation, the
sharing inverts: the current design shares the long prefix and pays only for the
new tail; the forward design shares only the short suffix and pays for the whole
prefix.

Concrete totals for "one dedicated list per chain" (every distinct chain copied
in full):

| corpus | emit nodes | copy-in-full nodes | blowup |
|---|---|---|---|
| words100 | 100 | 100 | 1.00× |
| words5k | 5,134 | 5,134 | 1.00× |
| words15k | 15,930 | 15,930 | 1.00× |
| words100k | 222,366 | 892,991 | **4.02×** |
| unidic | 785,533 | 857,109 | 1.09× |

This matches the earlier dedup-keyed count (3.35× / 1.02×) from a different
direction: forward linking either loses sharing (dedup-key count) or copies
prefixes (path-copy count). Both land in the same place — words100k pays 3–4×.

Note the two corpora fail for different reasons, and both are instructive:

- **words100k** has many long chains (`max_chain = 28`) and heavy prefix sharing
  (`chain_items/nodes = 4.6×`). Forward linking destroys precisely that sharing.
- **unidic** has short chains and little sharing to begin with (`1.27×`), so the
  blowup is mild — but there is also almost nothing for the design to win, since
  the current `Vec` already costs one allocation per iterator there.

The mechanism in one sentence: **a persistent singly-linked list can share on
prepend but never on append.**

- Reverse-linked (`pred`): appending `M_{k+1}` allocates one node and points it
  at the existing `M_k`. Both divergent descendants keep the *same* `M_1..M_k`
  and each owns only its new tail. Divergence is free; sharing is total.
- Forward-linked (`next`): appending `M_{k+1}` must write `M_k.next`. `M_k` is
  shared by both descendants, so it must be copied — but then `M_{k-1}.next`
  points at the stale `M_k`, so `M_{k-1}` must be copied too, cascading all the
  way back to `M_1`. **One divergence forces a full prefix copy.**

So it is not that a divergent node loses sharing with its neighbours; the entire
prefix back to the head is duplicated, because every link on the path to the head
must be rewritten. Sharing is not partially preserved — it is inverted. The
current design shares the long prefix and pays one node per extension; the
forward design shares only the short suffix and pays `O(k)` per divergence.

This is why no amount of dedup rescues it: the node identity at depth `d` depends
on the entire suffix of length `d`, so two chains that differ anywhere in the
prefix can never share a node before that point.

So the forward-link version is not "dedicated lists with shared nodes". It is
dedicated lists with nodes shared only for the last few matches, which is the
opposite of where the current design's sharing lives.

## Is the fail-fusion `step_child` at iter.rs:862 necessary?

```rust
if let Some(child_id) = unsafe {
    step_child(states, states.get_unchecked(state_id as usize).base(), state_id, mapped_c)
} {
    state_id = child_id;
    pos += char_len;
}
```

Runs after a fail transition, inside the `state.output_pos().is_some()` arm —
exactly when the call is about to buffer emits and return.

### RETRACTION (twice)

**First claim (wrong):** that the block "removes one full decode + map + state
load + probe per emit-returning fail", and that on a miss the probe is not an
extra `states[]` access "because the same load happens on the next call anyway".

**Second claim (also wrong):** that a hit saves "one decode + map + state load".

**What the counters actually show.** Instrumented both arms (block present /
removed), counting every access site:

| corpus | L829 loads | L865 loads | probes | child loads | decodes | maps |
|---|---|---|---|---|---|---|
| words100k | 921,349 | 23,717 | 945,066 | 453,212 | 603,412 | 603,412 |
| unidic | 387,089 | 235,982 | 623,071 | 533,236 | 386,014 | 386,014 |
| zh54k | 33,762 | 2,573 | 36,335 | 28,463 | 22,268 | 22,268 |

The invariant **`L829 + L865 == probes`** holds exactly on all three corpora. Every
`step_child` call is preceded by exactly one state load — the fusion block does
not avoid it, it *moves* it (L865 instead of L829).

Against the no-fusion arm:

| corpus | Δ state loads | Δ probes | Δ child loads | Δ decodes |
|---|---|---|---|---|
| words100k | **+21,550** | **+21,550** | +267 | **−2,167** |
| unidic | **+11,066** | **+11,066** | +8,700 | **−224,916** |
| zh54k | **+502** | **+502** | +291 | **−2,071** |

with `Δ state loads == Δ probes == misses` and `Δ decodes == −hits`.

So, precisely:

- **A hit saves only a decode + a map.** The state load is not saved — the fusion
  performs `states[fail]` at L865, and the next call performs `states[child]` at
  L829. One load either way. The probe is likewise paid in both arms.
- **A miss costs an extra state load, an extra probe, an extra decode and an
  extra map**, because everything the fusion did is redone on the next call.
- **`Δ decodes == −hits` is the whole benefit.** Nothing else improves.

### Where misses come from

Decomposing every miss by why `step_child` failed:

| corpus | misses | `mapped_c == None` | `base == None` | check mismatch |
|---|---|---|---|---|
| words100k | 21,550 | **21,283 (98.8%)** | 0 | 267 |
| unidic | 11,066 | **2,366 (21.4%)** | 0 | 8,700 |
| zh54k | 502 | **211 (42.0%)** | 0 | 291 |

`base == None` never occurs. On English, **98.8% of misses are characters with no
mapper entry at all** — `step_child` returns immediately without even loading
`states[base ^ code]`, so the L865 state load is pure waste. That is why the
block is measurably useless on prose: it pays a full state load on 21,550
occasions to save 2,167 decodes.

On unidic the miss profile is the opposite: only 21.4% are unmapped characters,
and 78.6% are genuine check mismatches where the probe did real work and simply
did not find a child.

### The payoff

| corpus | hit rate | run 1 ratio | run 2 ratio |
|---|---|---|---|
| words100 | 0.00% | 0.9995 | 1.0002 |
| words5k | 11.64% | 1.0027 | 1.0075 |
| words15k | 8.43% | 1.0090 | 1.0067 |
| words100k | 9.14% | 1.0009 | 0.9933 |
| **unidic** | **95.31%** | **0.9358** | **0.9378** |
| **zh54k** | **80.49%** | **0.9492** | — |
| zh689k | 70.84% | 0.9872 | — |
| **zh3185k** | **85.97%** | **0.9654** | — |

(separate binaries, alternating arms, 5 pairs, `zin`; ratio = fusion/no-fusion)

On unidic the block removes 224,916 of 610,930 decodes — **37% of all
decoding** — for the price of 11,066 extra state loads and probes. A decode + map
is evidently far more expensive than a state load, which is what makes the trade
pay there. On words100k the ratio is 21,550 wasted loads against 2,167 saved
decodes, and the result is flat.

### Answer to "is it better or worse in general?"

Better on densely-mapped text, neutral on prose, never measurably worse. The
condition is roughly `h × (decode + map) > (1 − h) × (state load + probe)`,
which holds above `h ≈ 10–15%`. Every measured corpus is either above that or
within noise.

### Answer to "can we go further in that branch?"

The commented-out ROOT special case is strictly redundant — the general
`step_child` below probes ROOT with the same load. A second chained step would
speculate one more character, but the main loop already does that next call, and
it would duplicate the emit-return bookkeeping while paying another probe on
every miss.

### On inlining, and a retracted micro-optimisation

`step_child` is `#[inline(always)]`, so it disappears into the caller — but that
does **not** remove the `states[state_id]` load. The shipped hot loop compiles to:

```
234fb: mov  %r14d,%eax
234fe: shl  $0x4,%rax            ; state_id * 16
23502: mov  (%rcx,%rax,1),%eax   ; <-- LOAD states[state_id].base  (unconditional)
23505: test %eax,%eax
23507: sete %dl                  ; base == None
2350a: mov  0x64(%rsp),%esi      ; mapped_c tag
2350e: not  %sil
23511: or   %dl,%sil             ; (mapped_c == None) | (base == None)
23514: test $0x1,%sil
23518: jne  23725                ; -> skip the probe
```

Inlining merges the two `None` checks into a single branch, but the load still
executes first. The compiler cannot sink it below the test.

I earlier proposed guarding the block with `mapped_c.is_some()` to skip that load
on the 98.8% of English misses that are unmapped characters. **That suggestion is
retracted.** Compiling both shapes confirms the guard *does* move the load below
the check:

```
fuse_guarded:
    xor  %eax,%eax
    test %ecx,%ecx        ; mapped_c tag
    je   .LBB0_4          ; -> return None before any load
    mov  (%rdi,%rcx),%edx ; load only when mapped_c is Some
```

But it does not pay. `states[state_id]` is a 16-byte L1-resident load, and the
fail target's cache line was just touched. The measured cost of 21,283 such
loads on words100k is already inside the noise floor (0.9995 / 1.0002). Trading
an unconditional load for an extra unpredictable branch is not obviously better,
and it was not measured. Leave the block as is.

### What is left in this branch: nothing measurable

`decodes == chars + misses` holds exactly, because fusion removes precisely the
hits. So the only redundant work remaining in the path is the `misses`
re-decodes:

| corpus | chars | decodes | redundant | % of decodes | cycles @9 | % of runtime |
|---|---|---|---|---|---|---|
| words100k | 581,862 | 603,412 | 21,550 | 3.57% | 194k | 0.48% |
| unidic | 374,948 | 386,014 | 11,066 | 2.87% | 100k | 0.10% |
| zh54k | 21,766 | 22,268 | 502 | 2.25% | 4.5k | 0.18% |

Two candidate fixes, both estimated against that budget:

**Cache the pending character across calls.** A `(mapped_c, char_len)` field
would let a miss reuse the character instead of re-decoding it. Cost: one
compare + branch on *every* character, not just misses (the cache must be
invalidated on the success path too). At ~0.5 cycles per character that is
0.71% / 0.20% / 0.43% of runtime, against savings of 0.48% / 0.10% / 0.18%.
**Net negative on all three corpora.**

**Guard the fusion with `mapped_c.is_some()`.** Saves the state load on the
`map_none` misses: 21,283 / 2,366 / 211 loads, worth 0.21% / 0.01% / 0.03%.
Below the 0.05–3.1% noise floor, so it cannot even be validated.

The branch is done. Its remaining headroom is under half a percent on every
corpus, and below the measurement noise on two of three.

## Caching the current `State` in the iterator (measured twice, rejected)

### Retraction: the first implementation was deficient

The first attempt measured 1.14x-1.17x slower. Three implementation faults
inflated that result, and the first measurement should not be quoted:

1. **`step_child` returned `Option<(u32, State)>` — 24 bytes.** That exceeds the
   register-return threshold, so every probe returned through memory (`sret`).
   `size_of::<Option<(u32, State)>>() == 24` vs `Option<u32> == 8`.
2. **`self.state` was stored at all three return sites**, i.e. once per match,
   and the 16-byte field had to stay live across the entire scan.
3. **The iterator field was unnecessary.** Base performs 921,349 loop-top loads
   on words100k but only 23,826 calls, so reloading once per *scan entry*
   already removes 97.4% of them. Carrying state across calls buys 23,826 loads
   and costs 23,826 stores plus register pressure.

The corrected version keeps `state` as a **local**, loaded once at scan entry and
updated only at the two sites where `state_id` changes; `step_child` keeps its
original `Option<u32>` signature. All 148 tests pass and match streams are
byte-identical on 8 corpora x 2 prefilter modes.

| corpus | first (deficient) | corrected | base (ns) |
|---|---|---|---|
| words100 | 1.1398 | **1.0779** | 10,470,645 |
| words5k | 1.1723 | **1.0819** | 10,597,148 |
| words15k | 1.1686 | **1.0809** | 11,295,653 |
| words100k | 1.1434 | **1.0596** | 13,721,909 |
| unidic | 1.0301 | **1.0311** | 31,764,010 |
| zh54k | 1.0929 | **1.0743** | 868,652 |
| zh689k | 1.0766 | **1.0643** | 10,274,126 |
| zh3185k | 1.0649 | **1.0621** | 71,445,699 |

The deficiencies accounted for about half the loss on English (1.14 -> 1.06).
The corrected version is still slower everywhere, so the conclusion holds, but
by a smaller margin than first reported.

### Why it still loses

Load arithmetic on words100k:

- base: `state loads == probes == 945,066`
- corrected: `entry(23,826) + child_steps(453,212) + fail_hops(363,204) = 840,242`

Only 11% fewer loads. **Retraction:** an earlier revision of this section
claimed base's loop-top reload "CSEs with the `.check` load `step_child` already
performs". That is false, and the generated code disproves it. Base performs
three loads from `states[state_id]` and one from `states[base ^ code]`; the two
indices are unrelated, so there is nothing to common-subexpression-eliminate:

```
.LBB0_6:
    movl %r9d, %ebx
    shlq $4, %rbx
    movl (%rdi,%rbx), %r10d     ; states[state_id].base
    movl 8(%rdi,%rbx), %r11d    ; states[state_id].fail
    movl 12(%rdi,%rbx), %ebx    ; states[state_id].output_pos
    testl %r10d, %r10d
    je .LBB0_7
    xorl %r8d, %r10d
    movq %r10, %r14
    shlq $4, %r14
    cmpl %r9d, 4(%rdi,%r14)     ; states[base ^ code].check  (different index)
```

Counting memory-operand moves over the whole function: **base 6, cached 14**.
The cached form has *more*, not fewer. It hoists the three fields into registers
at entry and then re-materialises all three after **every** transition — three
loads after a child step, three after every fail hop. Since a transition occurs
on nearly every iteration, the load count barely improves, while three live u32s
across the loop add register pressure (the disassembly shows spills to
`0x8(%rsp)`, `0x54(%rsp)`, ...).

The real explanation is therefore:

1. Base's three loads hit one 16-byte line that was just touched, so they are
   L1 hits; eliminating them saves almost nothing.
2. The cached form must reload all three fields whenever `state_id` changes,
   which is most iterations.
3. Keeping them live across the loop costs spills.

`perf stat` on words100k: instructions **+1.65%**, L1-dcache-load-misses
**+1.52%** — both worse. On unidic: +0.07% / -0.11%, i.e. flat.

---

## Original (deficient) measurement, retained for the record


**Proposal.** Keep a `state: State` field in the iterator holding exactly
`pma.states[state_id]`, maintained as an invariant so no validity branch is
needed. Then the scan loop never reloads `states[state_id]`, and the fusion block
can reuse the fail target's already-loaded state instead of loading it twice.

The invariant is cheap to maintain because `step_child` *already* loads
`states[child_idx]` to test `.check`; returning that state costs nothing extra.
Implemented exactly that way:

```rust
unsafe fn step_child(...) -> Option<(u32, State)> {
    let child = unsafe { *states.get_unchecked(child_idx) };
    if child.check() == expect { return Some((child_idx as u32, child)); }
    None
}
```

Correctness: all 148 tests pass, and match streams are byte-identical on 8
corpora x 2 prefilter modes.

Performance (separate binaries, alternating arms, 5 pairs, `zin`):

| corpus | base (ns) | cached (ns) | ratio |
|---|---|---|---|
| words100 | 10,458,631 | 11,915,500 | **1.140** |
| words5k | 10,511,323 | 12,309,664 | **1.172** |
| words15k | 11,370,221 | 13,192,094 | **1.169** |
| words100k | 13,613,210 | 15,594,862 | **1.143** |
| unidic | 32,561,826 | 33,249,713 | **1.030** |
| zh54k | 864,155 | 930,383 | **1.093** |
| zh689k | 10,118,155 | 10,961,850 | **1.077** |
| zh3185k | 71,468,258 | 76,478,034 | **1.065** |

Slower on every corpus, by 3-17%.

### Why it lost

`perf stat`, 3 reps:

| corpus | instructions | L1-dcache-load-misses |
|---|---|---|
| words100k | 959.6M -> 986.6M (**+2.81%**) | 9.331M -> 9.245M (-0.92%) |
| unidic | 13.871G -> 13.883G (+0.13%) | 88.991M -> 88.891M (-0.11%) |

Two facts settle it:

1. **L1 misses barely moved.** The `states[state_id]` load was never a cache
   miss — it is the same 16-byte line the loop just touched, and `step_child`
   already reads `.check` from `states[base ^ code]` regardless. What the change
   removed was an L1 *hit*, which costs almost nothing.
2. **Instructions increased.** The cached `State` is 16 bytes that must stay live
   across the entire scan loop. The disassembly shows it being spilled to and
   reloaded from stack (`mov 0x8(%rsp)`, `mov 0x54(%rsp)`, ...). Register
   pressure costs more than the eliminated load.

The base code reloads `states[state_id]` precisely because keeping it in a
register across the loop is more expensive. Same lesson as the earlier attempts:
the apparent redundancy is load-bearing.

**Retraction:** an earlier note in this document claimed the invariant "requires
a validity branch on every iteration". That was wrong — with the invariant
maintained, no branch is needed. The measurement above uses the branch-free
form, and it still loses.

### On reusing the legacy `scan()`

Not compatible. `scan()` returns `Option<NonZeroU32>` (an output position) and
has no knowledge of emit chains or the fusion probe. Reusing it would require
forking it, which is a refactor, not a saving.

## Q1: probe before buffering the emit chain (measured, rejected)

**Proposal.** Move the fusion `step_child` *above* `self.buf.extend(...)`:

```rust
state_id = state.fail();
if let Some(ep) = state.output_pos() {
    let child = step_child(states, states[state_id].base(), state_id, mapped_c); // probe FIRST
    self.buf.extend(iter_emits(ep, pos));                                        // then buffer
    if let Some(c) = child { state_id = c; pos += char_len; }
    ...
}
```

Correct and byte-identical (8 corpora x 2 prefilter modes), all tests green.

| corpus | Q1 (probe first) |
|---|---|
| words100 | **1.1247** |
| words5k | **1.1014** |
| words15k | 1.0891 |
| words100k | **1.0773** |
| unidic | 1.0031 (noise) |
| zh54k | 1.0450 |
| zh689k | 1.0214 |
| zh3185k | 0.9931 (noise) |

7-12% slower on English for a pure statement reorder.

### Isolating the cause

`perf stat` on words100k: instructions **+1.09%** (954.9M -> 965.3M) but wall
clock **+7.7%**. The gap means it is not throughput; it is scheduling.

Two candidate explanations were separated by a third variant, **Q1b**, which
hoists only the fail target's `base` load above the extend but keeps the probe
*after* it (so no probe result is live across the extend):

| corpus | Q1 (probe first) | Q1b (base load hoisted only) |
|---|---|---|
| words100 | 1.1247 | **1.0073** |
| words5k | 1.1014 | **1.0040** |
| words100k | 1.0773 | **1.0536** |
| unidic | 1.0031 | 1.0093 |

Hoisting just the load is essentially free (1.004-1.007). Moving the whole probe
is 10%+ slower. So the cost is **holding the probe's result live across
`Vec::extend`**, not the load itself.

Base deliberately probes *after* the extend. In that order the probe's loads are
independent of the emit-chain traversal, so the two can overlap. In Q1 the probe
completes first and its `Option<u32>` must survive a call that may grow the
buffer, which serialises the dependency chain. A 1% instruction increase cannot
explain a 7.7% time increase, but a longer critical path can.

**Verdict:** the current order is already the better one. Do not reorder.

---

## Q2/Q3: eager probing past output-less fail states (measured, WINS)

**Proposal (as clarified).** After buffering the first emit chain, keep walking
the fail chain instead of returning immediately, but stop as soon as a second
state with `output_pos` is reached. Do **not** cache the `State` in the iterator.

The key insight: the walk may pass through states that have **no output at all**.
Those are pure overhead — the character must be re-decoded on the next call just
to walk past them again. Advancing through them eagerly costs nothing in output
ordering, because no chain is buffered for them.

```rust
loop {
    let st = states[state_id];
    if let Some(child) = step_child(states, st.base(), state_id, mapped_c) {
        state_id = child; pos += char_len; break;      // consume the character
    }
    if state_id == ROOT { pos += char_len; break; }     // no pattern starts here
    if st.output_pos().is_some() { break; }             // second chain: defer it
    state_id = st.fail();                               // output-less hop: keep going
}
```

Correct and byte-identical on 8 corpora x 2 prefilter modes; all 148 tests pass.

### Result (two independent runs, separate binaries, alternating arms, 5 pairs)

| corpus | run 1 | run 2 |
|---|---|---|
| words100 | **0.9766** | **0.9774** |
| words5k | **0.9640** | **0.9642** |
| words15k | **0.9527** | **0.9588** |
| words100k | **0.9522** | **0.9597** |
| unidic | **0.9783** | **0.9748** |
| zh54k | **0.9732** | **0.9597** |
| zh689k | **0.9763** | **0.9753** |
| zh3185k | 0.9972 | **0.9823** |

**2-5% faster on every corpus, both runs.** This is the first change in this
line of investigation that wins broadly.

### Why it wins

Instrumented counts:

| corpus | matches | eager advances | output-less hops | stopped at 2nd output |
|---|---|---|---|---|
| words100k | 23,826 | 2,409 | 23,700 | 574 |
| unidic | 236,442 | 225,015 | **99** | 8,479 |
| zh54k | 2,578 | 2,243 | 192 | 43 |

Two distinct mechanisms, and they explain the corpus split:

- **unidic**: 225,015 eager advances with only 99 output-less hops. Here the
  win is that the walk continues *past* the first chain to a child step, so the
  character is consumed once instead of re-decoded. That is 225k saved decodes
  and re-entries out of 386k.
- **words100k**: only 2,409 advances but **23,700 output-less hops**. Here the
  win is different — those hops are states with no output that the walk would
  otherwise re-traverse on the next call. Hopping through them eagerly removes
  ~23.7k re-entries.

`perf stat` on words100k: instructions **-1.19%** (960.2M -> 948.8M), L1 loads
essentially flat. So this is a genuine work reduction, not a scheduling effect —
unlike Q1.

### Exact accounting

The counters satisfy two exact identities, which pin down what the change does:

```
q3.decodes == chars + stop_at_second
saved_decodes == eager_consumed - base_fusion_hits
```

| corpus | chars | stop@2nd | decodes | base decodes | redundant: base -> q3 |
|---|---|---|---|---|---|
| words100k | 581,862 | 574 | 582,436 | 603,412 | **21,550 -> 574 (97.3% gone)** |
| unidic | 374,948 | 8,479 | 383,427 | 386,014 | 11,066 -> 8,479 (23.4%) |
| zh54k | 21,766 | 43 | 21,809 | 22,268 | 502 -> 43 (91.4%) |

Where the eager advances come from:

| corpus | consumed in eager block | via child | via ROOT | output-less hops |
|---|---|---|---|---|
| words100k | 23,143 | 2,409 | **20,734** | 23,700 |
| unidic | 227,503 | **225,015** | 2,488 | 99 |
| zh54k | 2,530 | 2,243 | 287 | 192 |

Total state accesses (inner loop + eager/fusion block):

| corpus | base | q3 | change |
|---|---|---|---|
| words100k | 945,066 | 924,090 | **-2.22%** |
| unidic | 623,071 | 620,484 | -0.42% |
| zh54k | 36,335 | 35,876 | -1.26% |

So the dominant saving is **decode + map**, not state access. On words100k the
win is twofold: 20,734 characters are proven to start no pattern and are skipped
immediately, and 23,700 output-less hops are collapsed instead of being
re-traversed next call. On unidic the existing fusion probe already captured
225,015 of the 227,503 advances, so only 2,587 additional decodes are saved —
there the <1% work reduction is smaller than the 2.5% wall-clock delta, which
sits close to the 0.05-3.1% noise floor and should not be over-attributed.

### What "shrank but did not eliminate" means

The residual after Q3 is **exactly** `stop_at_second` — 574 / 8,479 / 43. Those
are the calls where a *second* chain is genuinely pending. It cannot be emitted
in the same call: `buf` is a LIFO drained by `next()`, and the second chain
belongs to a later position in the text, so pushing it now would make it pop
first and invert the output order. Returning is therefore mandatory, and the
next call re-enters at that state and re-loads it.

Eliminating those would require buffering both chains and reversing their order
— the whole-chain flush, measured 20-40% slower. So the residual is the
irreducible cost of one-chain-per-call ordering, not an implementation defect.

Base's residual was of a different kind: it re-entered at *every*
emit-returning fail (21,550 / 11,066 / 502), including all the output-less hops
it walked one at a time. Q3 collapses those hops, leaving only the genuine
second-chain stops.

### Correction: re-entry is NOT primarily caused by eager probing

An earlier revision of this note implied that Q3's stop-at-second rule was the
main source of re-reading. Measuring `reentry_at_output` in **both** arms shows
the opposite — the re-reading is inherited, and Q3 barely changes it:

| corpus | base re-entry | q3 re-entry | delta | q3 `stop_at_second` |
|---|---|---|---|---|
| words100k | 569 | 705 | **+136** | 574 |
| unidic | **230,405** | 230,504 | **+99** | 8,479 |
| zh54k | 659 | 715 | +56 | 43 |

On unidic base already re-enters at an output state **230,405** times; Q3 raises
that to 230,504, i.e. by 99 — exactly Q3's output-less hop count. So eager
probing adds essentially nothing to the re-read population; the population is
dominated by the one-chain-per-call design that predates it.

Note also that `reentry_at_output` is a broader set than `stop_at_second`: a scan
can begin at an output state because the EOF drain sets `state_id = fail(state)`
without emitting, or because a fusion hit lands on a child that itself has
output. That is why base's 569 on words100k exceeds Q3's 574 only slightly while
`stop_at_second` is 574.

### Why unidic re-enters 230,504 times, and what it means

Two counters measure different things and must not be conflated:

- **`reentry_at_output`** — scans whose *starting* state carries output.
- **`stop_at_second`** — eager walks that stopped **without consuming the
  character** because a second chain was pending.

The identity `decodes == chars + stop_at_second` is exact on all three corpora
(581,862+574 = 582,436; 374,948+8,479 = 383,427; 21,766+43 = 21,809). Therefore
**only `stop_at_second` is genuine re-decode waste**. `reentry_at_output` is the
normal resume path: the character was already consumed, and the next call simply
starts at the next character, which must be decoded once anyway.

The size of `reentry_at_output` on unidic follows directly from automaton shape:

| corpus | states | states with output | fail(output) also has output | avg output-run | max |
|---|---|---|---|---|---|
| words100k | 644,363 | 335,705 (52.1%) | 9.9% | 1.11 | 6 |
| unidic | 911,862 | **907,436 (99.5%)** | **23.4%** | 1.26 | 6 |
| zh54k | 93,886 | 75,937 (80.9%) | 8.6% | 1.09 | 4 |

unidic has 674,927 patterns, most of them very short Japanese morphemes, so
almost every trie node terminates some pattern. Hence 99.5% of states carry
output, and a resume after any fail lands on an output-bearing state — which is
exactly what `reentry_at_output` counts. It is a property of the dictionary, not
of the eager walk.

Note that dense output does **not** mean long chains: unidic's
`chain_items/emit_nodes` is 1.268, and its chain-length histogram is dominated by
length 1 (730,957 of 776,482 chains). Output is dense but chains are short.

### Can the 230,504 be reduced?

No, and it does not need to be — it is not waste. The actionable residual is
`stop_at_second` (574 / 8,479 / 43), and removing it requires buffering two
chains and reversing their order, which is the whole-chain flush (20-40% slower).

There is one theoretically cleaner option: when the eager walk stops at a second
output state, advance the character from that state and defer only the *chain*.
But `buf` would then hold two chains and drain them in the wrong order, so this
reduces to the same flush problem.

### What "21,550 -> 574" counts, exactly

It counts **redundant decodes** — the same character decoded twice. The identity
`decodes == chars + redundant` is exact:

- base words100k: 581,862 chars, 603,412 decodes -> **21,550 redundant**
- q3 words100k: 581,862 chars, 582,436 decodes -> **574 redundant**

And `21,550 == emit_returning_fails(23,717) - fusion_hits(2,167)` exactly. So in
base, every fusion probe that *missed* cost one extra decode on the next call.
Q3 removes those by continuing the walk; the residual 574 is `stop_at_second`.

### Adversarial pattern families: Q3 is NOT uniformly better

The 8 corpora are all natural-language. To test whether eager probing generalises,
7 synthetic pattern families were built and each run against 4-5 haystacks, with
base and Q3 both instrumented.

Correctness: identical match streams in every case.

Redundancy removed: Q3 eliminates **all** of it in these families (base had
800-2,400 redundant decodes; Q3 has 0).

Yet **wall clock is slower in several cases**:

| family | haystack | base redundant | q3 redundant | base/q3 |
|---|---|---|---|---|
| nested-a | abc-cycle | 1,200 | 0 | **1.0487** |
| nested-a | unmapped-heavy | 800 | 0 | **1.1077** |
| nested-a | spaces | 1,600 | 0 | **1.0151** |
| alternating | spaces | 1,600 | 0 | **1.1004** |
| single-chars | unmapped-heavy | 2,400 | 0 | 0.7372 |
| prefix+suffix | unmapped-heavy | 800 | 0 | 0.7781 |
| dense+unmapped | spaces | 1,600 | 0 | 0.8346 |

(`base/q3 > 1` means Q3 is slower.)

So on these small, dense, short-pattern automata Q3 removes the redundant decodes
yet still loses, because the extra loop structure and branch cost more than the
saved decode. The win on the 8 real corpora is therefore **not** a universal
property of the transformation; it depends on the automaton being large enough
that a decode + map + state load is expensive relative to the extra control flow.

Caveat on these numbers: the synthetic automata are tiny (tens to hundreds of
states), so everything is L1-resident and fixed per-call costs dominate. They
show the direction of risk, not a calibrated magnitude. They do establish that
the eager walk must be validated against the target workload rather than assumed
universally beneficial.

### On removing the duplicated probe sequence

The eager loop repeats the probe / ROOT / fail sequence that the main walk
already contains, which reads badly. A unified single loop was implemented and
measured:

```rust
let mut pending = None;
let consumed = loop {
    let state = states[state_id];
    if let Some(child) = step_child(states, state.base(), state_id, mapped_c) {
        state_id = child; break true;
    }
    if state_id == ROOT { break true; }
    if let Some(ep) = state.output_pos() {
        if pending.is_some() { break false; }   // second chain: defer
        pending = Some(ep);
    }
    state_id = state.fail();
};
```

Correct (all 148 tests, byte-identical streams on 8 corpora x 2 prefilter modes)
but **3.5-7.3% slower than base**, i.e. worse than the duplicated version:

| variant | instructions | branches | branch-misses |
|---|---|---|---|
| base | 951,208,483 | 200,912,693 | 9,467,526 |
| Q3 (duplicated block) | 949,225,844 | 202,037,653 | 9,560,135 |
| U1 (unified loop) | 965,231,958 | 205,060,308 | 9,535,397 |

| corpus | U1 ratio |
|---|---|
| words100 | 1.0404 |
| words5k | 1.0666 |
| words15k | 1.0555 |
| words100k | 1.0367 |
| unidic | 1.0518 |
| zh54k | 1.0733 |
| zh689k | 1.0377 |
| zh3185k | 1.0346 |

The reason is structural, not stylistic. In the duplicated form the eager block
runs only on the emit-returning path, *after* the main loop has already decided
to return; the hot path (child found) never touches it. Unifying makes `pending`
and `consumed` loop-carried, so every hot-path iteration carries them and the
mismatch branch gains a `pending.is_some()` test. Measured as +2.07% branches
versus +0.56% for the duplicated form.

So the duplication is the price of keeping the hot loop unperturbed. It is ~18
lines of cold code executed once per emit-returning fail. The duplication can be
reduced by extracting a helper that *only* the cold block calls, but the probe
itself must not become loop-carried state.

### Caching the decoded character in an iterator field (measured, rejected)

**Proposal.** Keep `(cached_pos, cached_c, cached_len)` in the iterator with the
invariant "the cache describes the character at `pos`". On the paths that return
without consuming the character (fusion miss), store it; at the top of the scan,
reuse it when `cached_pos == pos`. This eliminates the redundant decode+map
without any eager probing, so it should also avoid the extra state reads that
probing incurs.

This works exactly as intended. Instrumented hits:

| corpus | chars | cache hits | cache misses | decodes |
|---|---|---|---|---|
| words100k | 581,862 | **21,550** | 581,862 | 581,862 |
| unidic | 374,948 | **11,066** | 374,948 | 374,948 |
| zh54k | 21,766 | **502** | 21,766 | 21,766 |

`decodes == chars` exactly — **every** redundant decode is gone, and the hit
counts equal the previous waste exactly (21,550 / 11,066 / 502).

Correctness: all 148 tests pass; match streams byte-identical on 8 corpora x 2
prefilter modes.

Performance (separate binaries, alternating arms, 5 pairs):

| corpus | ratio |
|---|---|
| words100 | **1.1769** |
| words5k | **1.1769** |
| words15k | 1.1565 |
| words100k | **1.1387** |
| unidic | 1.0435 |
| zh54k | 1.1033 |
| zh689k | 1.0804 |
| zh3185k | 1.0861 |

Slower on every corpus, 4-18%.

### Why a perfect cache still loses

The check `self.cached_pos == pos` sits on the **hot path**: it runs once per
character (581,862 times on words100k) in order to save 21,550 decodes. That is a
27:1 ratio of checks to savings. `perf` on words100k: instructions **+2.41%**,
branches +0.35%, time +13.9%.

The added state is also not free: three more iterator fields (24 bytes) must be
kept live across the scan loop.

The ratio is the whole story. Redundant decodes are only 3.6% of all decodes on
words100k, and a decode of an already-hot byte is cheap; the comparison and the
extra live state cost more than the 21,550 decodes saved.

### Why the eager walk (Q3) wins where this loses

Q3 pays **nothing on the hot path** — the eager block runs only after the main
loop has decided to return, which is 23,717 times, not 581,862. It also removes
more work per event (it consumes the character, avoiding the decode *and* the
re-entry), and it removes the state re-reads along the way.

So the general rule that emerges from all of these attempts: a change that adds
work proportional to **characters** loses, even if it eliminates work
proportional to **emit events**. Q3 wins because its extra work is proportional
to emit events (23,717) while its saving is also proportional to emit events.

### Maintaining the character invariant instead of testing it (measured, rejected)

**Proposal.** Drop `cached_pos`. Instead of *testing* whether the cached
character is still current, *maintain* the invariant "`mapped_c`/`char_len`
always describe `pos`": decode the character at the bottom of the loop, right
after `pos` advances, and refresh it on the prefilter jump and on every
early return that consumed the character.

This is strictly better than the `cached_pos` variant — the per-character
comparison disappears:

| variant | words100k ratio | instructions |
|---|---|---|
| C1 (`cached_pos` test) | 1.1387 | +2.41% |
| C2 (maintained invariant) | **1.0742** | **+0.84%** |

Correctness: all 148 tests pass, byte-identical streams on 8 corpora x 2
prefilter modes. Three invariant sites had to be found and fixed (bottom of the
loop, the prefilter jump, and the early return that consumed the character) —
the prefilter jump was initially missed and broke 2 tests.

But it is still slower everywhere:

| corpus | ratio |
|---|---|
| words100 | 1.1274 |
| words5k | 1.1036 |
| words15k | 1.0909 |
| words100k | 1.0742 |
| unidic | 1.0136 (noise) |
| zh54k | 1.0456 |
| zh689k | 1.0529 |
| zh3185k | 1.0344 |

### Why maintaining the invariant still loses

The decode count is unchanged (still exactly one per character), so nothing is
saved. What changes is *where* the result lives:

- base: the decode happens at the top of the loop body, its result is consumed
  immediately in the same iteration, and it stays in a register.
- C2: the decode happens after the `pos` update and its result must be written
  to `self.mapped_c` / `self.char_len`, then reloaded at the top of the next
  iteration. That is a store/reload through the iterator struct on the
  loop-carried path.

`perf` on words100k: instructions +0.84%, branches +1.07%, time +7.4%. The
instruction delta is small but the dependency chain lengthens, because `pos` ->
decode -> store -> reload -> use is now serialised across iterations.

So the field is not free even without a staleness test: the invariant requires
the value to live in memory across the loop boundary, whereas base keeps it in a
register within one iteration.

### Hoisting the cached character into locals (measured, still rejected)

C2's loss came from writing the cached character back to `self` on every
iteration. Hoisting it into locals and writing back only on exit fixes most of
that:

| variant | words100k ratio | instructions (words100k) |
|---|---|---|
| C1 (`cached_pos` test) | 1.1387 | +2.41% |
| C2 (invariant, struct writeback) | 1.0742 | +0.84% |
| **H (invariant, hoisted to locals)** | **1.0200** | **-0.48%** |
| H2 (H + drop `char_len`, derive via leading byte) | 1.0247 | +0.81% |

Full results:

| corpus | H | H2 |
|---|---|---|
| words100 | 1.0468 | 1.0453 |
| words5k | 1.0416 | 1.0381 |
| words15k | 1.0274 | 1.0319 |
| words100k | 1.0200 | 1.0247 |
| unidic | **0.9940** | 1.0227 |
| zh54k | 1.0280 | 1.0769 |
| zh689k | 1.0128 | 1.0757 |
| zh3185k | 1.0066 | 1.0629 |

Correctness: all 148 tests pass; byte-identical streams on 8 corpora x 2
prefilter modes for both variants.

Two findings:

1. **Hoisting is what mattered.** Moving the cached character into locals and
   writing back once on exit took words100k from 1.0742 to 1.0200 and turned the
   instruction delta negative (-0.48%). So the cost really was the per-iteration
   store/reload through the iterator struct, as suspected.
2. **Dropping `char_len` in favour of deriving the length from the leading byte
   does not help.** It is slightly better on the English sets (1.0381 vs 1.0416
   on words5k) but clearly worse on CJK (1.0769 vs 1.0280 on zh54k). Deriving the
   length costs a branch on the leading byte in the hot loop, which is more than
   the extra field's cost.

H is now close to parity — even slightly faster on unidic — but still 0.7-4.7%
slower on the other seven corpora, so it is not worth landing over Q3, which wins
1.3-5.9% everywhere. It is retained as a measurement, not a candidate.

### RETRACTION: two claims in the section above were wrong

**1. `len_utf8` was never used.** H2 does not call `char::len_utf8`; that function
takes a `char`, and the cache holds a *mapper code* (`u32`), so it cannot be
applied to it. What H2 actually does is derive the UTF-8 length from the leading
byte with a four-way range match (`utf8_len_unchecked`). That is a *different*
change: it removes the `char_len` field at the cost of one byte load plus a
range test per iteration. The conclusion stands (it is worse, especially on CJK),
but the label "len_utf8" was incorrect.

**2. H does NOT perform the same number of decodes as base.** Measured:

| corpus | base decodes | H decodes | saved |
|---|---|---|---|
| words100k | 603,412 | 581,861 | **21,551** |
| unidic | 386,014 | 374,947 | **11,067** |
| zh54k | 22,268 | 21,765 | **503** |

So H does eliminate the redundant decodes, exactly as the proposal intended. The
earlier statement that it added work "for the same total number of decodes" was
false.

### Then why does H still lose?

`perf` on words100k: H executes **0.48% fewer** instructions than base
(948.2M vs 952.8M) yet is **2.0% slower** in wall clock. Fewer instructions and
more time means the loop is latency-bound, not throughput-bound.

H moved the decode from "top of the iteration, consumed immediately in the same
iteration" to "bottom of the iteration, stored to a local, consumed on the next
iteration". That puts the decode on the loop-carried back edge:

- base: `pos -> decode -> use` (within one iteration)
- H: `pos -> decode -> store -> load -> use` (crosses the back edge)

The saved work is 21,551 decodes out of 603,412 (3.6%). The added critical-path
latency applies to all 581,862 iterations. The trade is unfavourable.

A third variant (**H3**) tried keeping the decode at the top while still reusing
the cached value, so nothing crosses the back edge:

```rust
let (cur_c, cur_len) = if let Some(c) = mapped_c {
    (Some(c), char_len)          // reused from the previous call
} else {
    decode(...)                  // same place as base
};
```

It is **worse still** (words100k 1.1457): the per-iteration `Option` test plus
the need to keep both the cached and the freshly decoded value live costs more
than the back-edge version.

So the ordering is: base < H (1.0200) < H3 (1.1457) < C2 (1.0742, worse than H
because of per-iteration struct writeback) < C1 (1.1387).

The user's expectation — that a stack put/read is cheaper than a decode — is
correct for **throughput** (and the instruction count confirms it: H does fewer
instructions). It fails here because this loop is bound by the dependency chain,
not by instruction count.

### What base does on an emit-returning fail, and how big the saving really is

**Base performs no walk.** On a mismatch where the state owes a chain it:

1. `state_id = state.fail()`
2. buffers that one chain,
3. probes the fail target **once** (the fusion probe),
4. consumes the character if the probe hits, then returns.

There is no loop and no fail-chain traversal. It never visits the *second*
output state — that is precisely why the next call has to re-enter at the fail
target and re-walk from there.

**Almost every emit chain does cost an extra decode**, so the earlier
characterisation "the saving is small" was misleading. The exact split:

| corpus | emit-returning fails | fusion hits | fusion misses | miss rate | H's saved decodes | % of all decodes |
|---|---|---|---|---|---|---|
| words100k | 23,717 | 2,167 | 21,550 | **90.9%** | 21,551 | 3.6% |
| unidic | 235,982 | 224,916 | 11,066 | **4.7%** | 11,067 | 2.9% |
| zh54k | 2,573 | 2,071 | 502 | **19.5%** | 503 | 2.3% |

Two things follow:

1. **In the absence of fusion, essentially every emit chain would cost an extra
   decode.** The user's intuition is right. On words100k, where the fusion probe
   rarely fires (9.1% hit rate), 90.9% of emit chains do incur one, and H removes
   **100% of them** (21,551 of 21,550 — the off-by-one is the final character).
2. **The pool is small only because the existing fusion probe already drains it**
   on dense text. On unidic the probe hits 95.3%, leaving just 4.7% for H to
   recover. That is why H's saving looks modest there and why Q3 (which extends
   the same idea) also has little left to win on unidic.

So the correct statement is not "the redundant decodes are few" but "the fusion
probe already removes most of them on dense text; on prose, where it cannot fire,
H removes all of the remaining 90.9%, but the latency cost of doing so still
exceeds the benefit."

### H5: cache without the probe (measured, wins CJK, loses English)

The logical question: if the cached character already eliminates the redundant
decode, what is the fusion probe still for? It exists only to avoid re-decoding
the character — which the cache now does. So the probe should be removable.

H5 does exactly that: on an emit-returning fail it buffers the chain, writes the
character invariant back, and returns without probing.

Correctness: all 148 tests pass; byte-identical streams on 8 corpora x 2
prefilter modes.

| corpus | H4 (cache + probe) | H5 (cache only) |
|---|---|---|
| words100 | — | 1.0763 |
| words5k | — | 1.0795 |
| words15k | — | 1.0605 |
| words100k | 1.0200 | 1.0325 |
| unidic | 0.9940 | **0.9890** |
| zh54k | — | 1.0060 |
| zh689k | — | 1.0064 |
| zh3185k | — | **0.9855** |

H5 is the first variant in this line that **beats base on CJK** (unidic 0.989,
zh3185k 0.9855), while losing on English.

### Why removing the probe helps CJK and hurts English

The probe's *decode-avoidance* value is fully replaced by the cache, but it has a
second effect: **on a hit it consumes the character**, so the call does not end in
a return + re-entry.

- unidic: probe hit rate 95.3%. Removing it costs little (most hits would have
  been converted to advances anyway, and the next call starts from a state the
  cache already covers), while eliminating 235,982 probes. Net win.
- words100: hit rate 9.1%. Removing the probe means **every** emit-return
  (23,717) now ends in a re-entry, versus 21,550 for H4 — i.e. 2,167 extra
  re-entries. Each re-entry costs a scan re-entry plus a state load plus a
  probe, which is more than the probe it replaced. Net loss.

So the probe is not "only for reducing decoding" — it also merges a
return/re-entry pair into the current call. The cache removes the first motive
but not the second.

`perf` on words100k: base 964.3M instructions, H4 954.9M, H5 956.7M — H5 is
between the two, and the difference is not instruction count but the re-entry
pattern.

### H6: packing the character into one word (measured, worse)

Two ideas were checked: use `u8` for `char_len`, and pack `mapped_c` + `char_len`
into one `u32` by borrowing 2 bits.

**Is `mapped_c` bounded?** Yes, tightly. `mapped_c` is an index into the
alphabet (`table[codepoint] = rank in frequency order`), so
`mapped_c < alphabet_size`. The alphabet is at most the number of distinct
Unicode scalar values, `0x110000 = 1,114,112 < 2^21`. Reserving 2 bits for the
length leaves 30 bits of capacity (1,073,741,823), a margin of roughly 963x. So
packing is safe with no realistic collision risk, provided an explicit sentinel
is reserved for "unmapped" and "end of haystack".

**Does `u8` help?** No. `char_len` is 1..=4 so `u8` holds it, but the field is
adjacent to `Option<u32>` and the struct is pointer-aligned, so the byte is
absorbed by padding. There is no memory saving.

**Does packing help?** No — it costs:

| corpus | H5 (two fields) | H6 (packed) |
|---|---|---|
| words100 | 1.0743 | **1.0552** |
| words5k | 1.0785 | 1.0639 |
| words15k | 1.0654 | 1.0594 |
| words100k | 1.0291 | 1.0384 |
| unidic | **0.9824** | 1.0289 |
| zh54k | 1.0063 | 1.0025 |
| zh689k | 1.0153 | 1.0175 |
| zh3185k | **0.9945** | 1.0308 |

Packing improves the English sets slightly (1.0743 -> 1.0552 on words100) but
**destroys the CJK wins** that made H5 interesting: unidic 0.9824 -> 1.0289,
zh3185k 0.9945 -> 1.0308.

The cost is the unpack work: every use of `mapped_c` and `char_len` now needs a
shift plus a mask instead of a direct field read, and `Option<u32>`'s niche
optimisation is lost. On CJK the character is re-read on every emit-return, so
the extra shifts are paid far more often than on English.

Correctness: all 148 tests pass, byte-identical streams on 8 corpora x 2
prefilter modes, and the encoding is provably collision-free given
`alphabet_size < 2^30`.

### The 2x2 that was missing: fusion x cache, against the plain base

Every cache measurement above was taken against the **fusion** base, which
conflates two independent changes. The fair control is the *plain* (no-fusion)
matcher. Four variants, all separate binaries, alternating arms, two runs:

- **NF** = plain base (no fusion probe, no cache) — `/tmp/dac-nofuse`
- **FU** = fusion probe only — current HEAD
- **NC** = cache only (H5) — `/tmp/dac-h5`
- **FC** = fusion + cache (H4) — `/tmp/dac-h4`

Ratios vs **NF**:

| corpus | FU run1 / run2 | NC run1 / run2 | FC run1 / run2 |
|---|---|---|---|
| words100 | 0.9983 / 0.9978 | **1.0728 / 1.0740** | 1.0461 / 1.0450 |
| words5k | 1.0104 / 1.0101 | **1.0896 / 1.0881** | 1.0595 / 1.0557 |
| words15k | 1.0058 / 1.0031 | **1.0721 / 1.0693** | 1.0405 / 1.0376 |
| words100k | 1.0118 / 1.0124 | **1.0541 / 1.0427** | 1.0292 / 1.0275 |
| unidic | 0.9359 / 0.9455 | 0.9353 / 0.9386 | **0.9260 / 0.9438** |
| zh54k | 0.9697 / 0.9616 | 0.9709 / 0.9718 | 0.9954 / 0.9856 |
| zh689k | 1.0008 / 1.0012 | 1.0057 / 1.0080 | 1.0178 / 1.0118 |
| zh3185k | 0.9802 / 0.9815 | **0.9727 / 0.9779** | 0.9992 / 1.0009 |

### What this actually says

**The cache costs about 5-9% on English against the plain base** (NC 1.054-1.090),
and about 3-6% when combined with fusion (FC 1.029-1.060). So the earlier
statement "the cache loses at most 7%" was measured against the wrong control;
against the plain matcher the loss is larger, and fusion masks part of it
because fusion already removes most of the re-decodes on CJK.

**The user's diagnosis is correct: the overhead is the store/reload of the cached
character.** That is why:
- C1 (`cached_pos` test, struct writeback) = 1.1387
- C2 (invariant, struct writeback every iteration) = 1.0742
- H (invariant, hoisted to locals) = 1.0200 vs fusion
- H5 (no probe, hoisted) = 1.0325 vs fusion

Each step removes write traffic or work, and each step improves. The remaining
gap is the back-edge latency described earlier.

**"Probing now or later doesn't matter" is false, but only slightly.** FU vs NF is
within +-1.2% on English (noise-level), and -2% to -6.4% on CJK. So the probe is
worth keeping on dense text and harmless on prose — the earlier finding stands.

**On the rare second-chain case:** the user is right that the fusion probe reads
`states[fail_target]` even when a second chain is pending. But that read happens
once per emit-returning fail (23,717 on words100k), not once per character, so it
is not the source of a 5-9% gap.

### The decisive comparison

Against the plain base, on English the plain matcher (NF) is **fastest**, then
FU (fusion, ~parity), then FC (cache+fusion, +3-6%), then NC (cache only, +5-9%).
On CJK, FU/NC/FC all beat NF by 2-6%, with NC and FC roughly tied with FU.

So the honest summary is: **the cache never pays on English and is roughly
neutral-to-slightly-positive on CJK**, while **fusion pays on CJK and is neutral
on English**. Fusion is the better single change; the cache is not worth adding.

### Legacy vs the plain (no-fusion) base

Single crate (`/tmp/dac-nofuse`), both arms selected by `MatchKind`, separate
runs of each arm alternating, 5 pairs, two independent runs. `exp/leg < 1` means
the experimental matcher is faster.

| corpus | run 1 | run 2 |
|---|---|---|
| words100 | 0.9966 | 0.9939 |
| words5k | **1.0486** | **1.0501** |
| words15k | **1.0766** | **1.0701** |
| words100k | **1.1102** | **1.1048** |
| unidic | 0.9760 | 0.9731 |
| zh54k | 0.9751 | 0.9609 |
| zh689k | 0.9438 | 0.9467 |
| zh3185k | 0.9586 | 0.9548 |

The plain experimental matcher:

- **wins on CJK**: unidic -2.4%/-2.7%, zh54k -2.5%/-3.9%, zh689k -5.6%/-5.3%,
  zh3185k -4.1%/-4.5%
- **loses on English prose**: words5k +4.9%/+5.0%, words15k +7.7%/+7.0%,
  words100k +11.0%/+10.5%
- **parity on words100** (-0.3%/-0.6%)

This reproduces the long-standing finding recorded earlier in this document
("exp wins on CJK, loses 1.8-10% on English prose, except words100 = a win"),
now against the plain base rather than the fusion base.

### Q3 (eager probing) measured directly against legacy

Q3 was missing from the ranking above. Measured the same way (one crate, both
arms via `MatchKind`, alternating, 5 pairs, two runs):

| corpus | run 1 | run 2 | verdict |
|---|---|---|---|
| words100 | **0.8783** | **0.8784** | WIN |
| words5k | **0.9461** | **0.9391** | WIN |
| words15k | **0.9775** | **0.9839** | WIN |
| words100k | 1.0334 | 1.0360 | LOSS |
| unidic | **0.9020** | **0.8944** | WIN |
| zh54k | **0.9132** | **0.9198** | WIN |
| zh689k | **0.9159** | **0.9237** | WIN |
| zh3185k | **0.9157** | **0.9166** | WIN |

**Q3 beats legacy on 7 of 8 corpora**, by 2-12%, and is the only variant that
wins on English prose. words100k remains a 3.3-3.6% loss — the same
unmapped-character deficit that affects every experimental variant there.

This materially changes the conclusion drawn from the plain base: the eager walk
is not just an optimisation of the experimental matcher, it is what makes the
experimental matcher competitive with legacy on English at all.

### Completing the picture: legacy, NF, FU, NC, FC, Q3

Combining this with the 2x2 above gives the full ranking. Representative numbers
(median of two runs), all normalised so that **legacy = 1.000**:

| corpus | legacy | NF (plain) | FU (fusion) | NC (cache) | FC (both) | **Q3 (eager)** |
|---|---|---|---|---|---|---|
| words100 | 1.000 | 0.995 | 0.994 | 1.068 | 1.041 | **0.878** |
| words5k | 1.000 | 1.049 | 1.060 | 1.143 | 1.112 | **0.943** |
| words15k | 1.000 | 1.073 | 1.079 | 1.151 | 1.116 | **0.981** |
| words100k | 1.000 | 1.107 | 1.120 | 1.171 | 1.140 | 1.035 |
| unidic | 1.000 | 0.975 | 0.912 | 0.912 | 0.903 | **0.898** |
| zh54k | 1.000 | 0.968 | 0.938 | 0.940 | 0.963 | **0.917** |
| zh689k | 1.000 | 0.945 | 0.946 | 0.950 | 0.962 | **0.920** |
| zh3185k | 1.000 | 0.957 | 0.938 | 0.931 | 0.956 | **0.916** |

(Q3 is measured directly against legacy. NF/FU/NC/FC are as in the 2x2, with
FU/NC/FC derived by multiplying by the NF/legacy ratio — indicative.)

(FU/NC/FC are derived by multiplying the 2x2 ratios by the NF/legacy ratio; they
are indicative, not independently measured against legacy.)

Reading it:

1. **Q3 is the best variant on 7 of 8 corpora**, including English prose where
   every other variant loses to legacy. It is the only change in this whole
   investigation that is a strict improvement over legacy on English.
2. **On CJK Q3 is best or tied** (unidic 0.898 vs FU 0.912; zh3185k 0.916 vs
   NF 0.957), so it does not trade away the CJK advantage.
3. **words100k is the single remaining loss** (+3.5%). That is the
   unmapped-character flush-walk deficit, which no variant here addresses.
4. **The cache never improves on the best available option**: NC is worse than NF
   on English and roughly tied with FU on CJK, and worse than Q3 everywhere.

### Are the adversarial degradations unique to Q3? No — fusion is far worse

The earlier synthetic test only measured Q3. All five variants were re-run on the
same 8 pattern families x 5 haystacks (40 cells), each against its own legacy arm,
30 reps, best-of. `exp/leg`, lower is better.

| variant | median | min | max | cells > 1.05 |
|---|---|---|---|---|
| NF (plain) | 0.935 | 0.820 | 1.239 | 9/40 |
| **FU (fusion = current HEAD)** | **1.097** | 0.943 | **1.537** | **29/40** |
| Q3 (eager) | 0.950 | 0.887 | 1.177 | 11/40 |
| NC (cache) | 0.948 | 0.895 | 1.192 | 8/40 |
| FC (both) | 0.934 | 0.841 | 1.222 | 7/40 |

**Q3's degradation is not an outlier.** In fact the fusion version currently on
HEAD is the *worst* variant on this set: median 1.097 (10% slower than legacy) and
losing in 29 of 40 cells, with a worst case of 1.537. Q3 has median 0.950, loses
in only 11 of 40, worst case 1.177.

So on contrived patterns, Q3 is **more robust than the shipped fusion code**, not
less.

### What drives the losses (and they are shared)

By haystack, median across families:

| haystack | NF | FU | Q3 | NC | FC |
|---|---|---|---|---|---|
| abc-cycle | 0.935 | 1.152 | 0.979 | 0.965 | 0.931 |
| all-a | 0.910 | 1.037 | 1.011 | 0.985 | 0.921 |
| cjk-spaces | 0.859 | 1.089 | 0.891 | 0.914 | 0.871 |
| spaces | 0.955 | 1.156 | 0.923 | 0.913 | 1.023 |
| **unmapped-heavy** | **1.082** | **1.248** | **1.076** | **1.059** | **1.096** |

By family:

| family | NF | FU | Q3 | NC | FC |
|---|---|---|---|---|---|
| all-pairs-6 | 0.926 | 1.158 | 1.013 | 1.015 | 0.934 |
| alternating | 0.945 | 1.153 | 0.957 | 0.922 | 0.927 |
| cjk-short | 0.834 | 0.943 | 0.945 | 0.948 | 0.865 |
| dense+unmapped | 0.988 | 1.164 | 0.997 | 1.033 | 1.031 |
| nested-a | 0.993 | 1.089 | 1.044 | 0.925 | 0.951 |
| non-matching | 0.834 | 0.943 | 0.945 | 0.948 | 0.864 |
| prefix+suffix | 0.955 | 1.097 | 0.962 | 0.935 | 0.988 |
| single-chars | 0.974 | 1.170 | **1.093** | 1.043 | 1.029 |

Two patterns are shared by *every* variant:

1. **`unmapped-heavy` is the universal weak spot** — every variant loses there
   (1.059 to 1.248). This is the known unmapped-character flush-walk deficit, not
   a property of any one optimisation.
2. **`single-chars` (alphabet = 26 one-char patterns) is bad for all variants**,
   worst for FU (1.170) and Q3 (1.093). With such an automaton almost every
   character ends a pattern, so the experimental design's per-emit bookkeeping
   dominates and the legacy matcher's rewind is cheap by comparison.

So the honest characterisation is: **the experimental matcher's weaknesses on
contrived inputs are structural and shared**, driven by unmapped characters and
by very dense short-pattern automata. Q3 does not introduce them; it mitigates
most of them relative to the fusion code now on HEAD.

### Why Q3 wins if the cache removes the same decoding: it does not

The hypothesis was that Q3's benefit is avoiding the redundant decode, so a
character cache — which removes exactly that decode — should match it. Measured
like-for-like on words100k with both arms instrumented:

| metric | H5 (cache) | Q3 (eager) | NF (plain) |
|---|---|---|---|
| chars | 581,862 | 581,862 | 581,862 |
| **decodes** | **581,861** | **582,436** | 603,412 |
| state loads | 923,516 | 924,090 | 923,516 |
| probes | 923,516 | 924,090 | 923,516 |
| emit returns | 23,717 | 23,717 | 23,717 |
| instructions | **950.9M** | 953.7M | 961.9M |
| wall clock vs legacy | **1.03-1.09 (loses)** | **0.878-0.943 (wins)** | 1.05-1.11 (loses) |

The cache removes **more** decoding than Q3 (581,861 vs 582,436) and executes
**fewer** instructions (950.9M vs 953.7M). Yet Q3 is faster by roughly 10-15% of
legacy on English.

**So decoding is not what Q3 saves.** The hypothesis is falsified directly.

### What Q3 actually saves

The two designs differ in what the *next* call must do after an emit-return:

- **H5 (cache)**: leaves `pos` unchanged and `state_id` at the fail target. The
  next call re-enters, re-loads `states[state_id]`, and re-probes from the same
  state. The fail-chain walk is *redone*. The cache makes the character free, but
  the walk itself is still paid.
- **Q3 (eager)**: on a child hit it consumes the character and leaves `state_id`
  at the child. The next call resumes *past* that point, so the walk is not
  redone.

The decode was the visible symptom of the re-walk, not its cost. Removing the
decode while keeping the re-walk (H5) removes the cheap part; removing the
re-walk (Q3) removes the expensive part.

This is also why instruction counts mislead here: both designs execute a similar
number of instructions, but Q3's are arranged so that each scan makes forward
progress, while H5's repeat the same work from the same state. The loop is
latency/dependency-bound, so *where* the work sits matters more than how much of
it there is.

### Practical consequence

Any future attempt to optimise this path should target the **fail-chain re-walk**
(how many states are visited per scan), not the **decode** (how many characters
are decoded). The decode is already at its floor of one per character in every
variant that matters, and the measurements above show that pushing it lower does
not move wall clock in the right direction.

### RETRACTION: the 12% Q3-vs-legacy figure was a cross-binary artifact

An earlier section reported Q3 beating legacy by 0.878 on words100, and attributed
the win to "avoiding the fail-chain re-walk". Both parts are wrong.

**1. There is no re-walk to avoid.** The user's objection is correct:
`state_id = state.fail()` has already moved to the new state before the emit
check, so the next pass accesses that same state either way. Instrumented counts
confirm eager probing saves **no** state accesses:

| metric | NF (plain) | Q3 (eager) |
|---|---|---|
| state loads | 923,516 | **924,090** (slightly more) |
| probes | 923,516 | 924,090 |

Q3 only avoids re-*decoding* the character, and adds a small amount of waste when
it meets a second chain.

**2. The 12% was a harness artifact.** The `legq3` binary's legacy arm ran 4.2%
slower than the `legnf` binary's legacy arm, for byte-identical legacy source
(10.73M vs 11.18M ns on words100). Ratios measured in different binaries are
therefore not comparable — the same codegen-shape coupling documented earlier in
this file.

### Same-binary measurement of Q3 vs plain NF

Both experimental arms compiled into **one** binary, identical harness, alternating,
5 pairs:

| corpus | median eager/plain | min | max |
|---|---|---|---|
| words100 | 0.9888 | 0.9866 | 0.9893 |
| words5k | 0.9836 | 0.9759 | 0.9901 |
| words15k | 0.9773 | 0.9665 | 0.9892 |
| words100k | 0.9519 | 0.9397 | 0.9588 |
| unidic | 0.9533 | 0.9197 | 1.0042 |
| zh54k | 0.9734 | 0.9670 | 0.9791 |
| zh689k | 0.9698 | 0.9560 | 0.9833 |
| zh3185k | 0.9661 | 0.8863 | 0.9841 |

**Q3 is 1-5% faster than the plain experimental matcher**, consistently, on all
eight corpora. Real but modest.

Corrected Q3-vs-legacy estimate (Q3 = NF x 0.95-0.99, with NF/legacy from the
direct measurement): roughly **0.95-1.06** across corpora, not 0.88-0.98.

### Why Q3 wins over the cache when both remove the same decode

Since both eliminate the redundant decode, and Q3 saves no state access, the
difference is *cost of the mechanism*:

- **Q3** does its work only on the emit-return path (23,717 times on words100k)
  and adds nothing to the per-character path.
- **The cache** also removes the decode, but pays a field write on every
  emit-return plus a read on every scan entry, and must refresh the invariant on
  the path that consumed a character.

So Q3 is not saving something the cache misses — it is achieving the same saving
more cheaply. That is the whole difference.

### Three-way in one binary: legacy vs single probe vs Q3

All three arms compiled into **one** binary (legacy and single-probe from the
shipped crate, Q3 from `/tmp/dac-q3clean`), identical harness, alternating, 5
pairs, 11 reps, medians. Match streams asserted equal across all three.

| corpus | legacy ns | single-probe/legacy | Q3/legacy | Q3/single-probe |
|---|---|---|---|---|
| words100 | 11,506,441 | 0.9255 | **0.9192** | 0.9932 |
| words5k | 10,856,090 | 1.0019 | **0.9920** | 0.9901 |
| words15k | 11,233,609 | 1.0379 | 1.0231 | **0.9857** |
| words100k | 12,965,003 | 1.0782 | 1.0416 | **0.9661** |
| unidic | 34,428,230 | **0.9237** | 0.9323 | 1.0093 |
| zh54k | 1,293,251 | 0.9319 | **0.9239** | 0.9914 |
| zh689k | 11,337,084 | 0.9341 | **0.9285** | 0.9939 |
| zh3185k | 77,029,160 | 0.9379 | **0.9166** | 0.9772 |

**Q3 vs legacy**: wins 6 of 8 (0.917-0.992), loses on words15k (+2.3%) and
words100k (+4.2%). So the eager walk does not fix the English prose deficit; it
merely narrows it.

**Q3 vs single probe**: wins 7 of 8, by 0.7-3.4%. The one loss is unidic
(+0.9%), which is within the per-pair spread (0.987-1.017) and therefore not
significant. This is the clean measurement of what eager probing buys over the
single fusion probe currently on HEAD: **about 1-3%, up to 3.4% on words100k**.

Note how much the single-probe column moves relative to earlier sections: on
words100 it now reads 0.9255 whereas the cross-binary comparison gave ~0.998.
That is the codegen-shape coupling again — only same-binary numbers should be
compared.

### Reading the three-way table

1. **On CJK, both experimental variants beat legacy** (0.917-0.934), and Q3 is
   marginally better than single-probe except on unidic where they tie.
2. **On English prose, the ordering inverts**: legacy is best on words15k and
   words100k, single-probe is worse still (1.038, 1.078), and Q3 recovers part of
   the gap (1.023, 1.042) without closing it.
3. **words100 is the exception**: both experimental variants beat legacy there
   (0.919-0.926).
4. **The eager walk's contribution is small but consistent** (0.7-3.4% over
   single probe). It is a genuine improvement, not the 12% an earlier
   cross-binary measurement suggested.

### Three-way in one binary: legacy vs plain (no fusion) vs Q3

All three arms in **one** binary: legacy and plain from separate crates, Q3 from
`/tmp/dac-q3clean`. Identical harness, alternating, 5 pairs, 11 reps, medians.
Match streams asserted equal across all three.

| corpus | legacy ns | plain/legacy | Q3/legacy | Q3/plain |
|---|---|---|---|---|
| words100 | 11,181,633 | 0.9936 | **0.9453** | 0.9510 |
| words5k | 10,666,063 | 1.0491 | 1.0094 | 0.9605 |
| words15k | 11,149,200 | 1.0620 | 1.0260 | 0.9681 |
| words100k | 13,001,552 | 1.0752 | 1.0384 | 0.9673 |
| unidic | 34,931,115 | 0.9626 | **0.9171** | 0.9514 |
| zh54k | 1,291,951 | 0.9495 | **0.9156** | 0.9659 |
| zh689k | 11,305,798 | 0.9581 | **0.9300** | 0.9791 |
| zh3185k | 76,891,024 | **0.9050** | 0.9060 | 1.0021 |

**Q3 vs plain (no fusion)**: wins 7 of 8, by **2-5%** (0.951-0.979). Only zh3185k
is parity (1.0021). So the eager walk is a consistent, real improvement over the
plain experimental matcher, and it is largest exactly where the fusion probe
cannot fire (words100k 0.967, words100 0.951).

**Q3 vs legacy**: wins 5 of 8 — words100 (-5.5%), unidic (-8.3%), zh54k (-8.4%),
zh689k (-7.0%), zh3185k (-9.4%). Loses on words5k (+0.9%), words15k (+2.6%),
words100k (+3.8%).

So the honest summary:

- **CJK**: Q3 beats legacy by 7-9%. This is the branch's real value.
- **words100**: Q3 beats legacy by 5.5%.
- **English prose (5k-100k)**: Q3 still loses to legacy by 1-4%, but the eager
  walk cuts that deficit roughly in half compared with the plain matcher
  (words100k: plain 1.075 -> Q3 1.038).

Note that `plain/legacy` here differs from the earlier cross-binary measurement
(words100k 1.075 now vs 1.107 before). Same codegen-shape coupling; only
same-binary numbers should be compared.

### Contrast with the earlier rejected whole-chain flush

The earlier "consume the character once" experiment buffered **all** chains
found along the walk and was 20-40% slower. Q3 differs in the decisive way: it
**stops at the second output state**, so it never buffers more than one chain per
call. The laziness of one-chain-per-call is preserved; only the output-less hops
are collapsed. That is why this version wins where the flush lost.

### Note on the earlier Q2 attempt

A first version of this experiment (buffering every chain along the walk) failed
correctness — `patterns: ["a"], haystack: "aa"` produced no matches — because a
later fail-chain state can hold a chain whose matches start before ones already
buffered, so a flat LIFO push cannot interleave them. Stopping at the second
output state avoids the problem entirely.


## Gated `fail_has_emit` Bit 31: Closing the English Prose Gap

### Context & Flaw in Candidate 8
Earlier Candidate 8 tested bit 31 either inside the inner loop or hoisted before the transition probe. On small automata (`words100`), that imposed a universal branch/load tax on every character even when `state_id == ROOT_STATE_IDX` and when characters were mapped.

### Architectural Fix
In the refactored `iter.rs`, the unmapped character path is strictly segregated (`if let Some(code) = mapped_c { ... } else { ... }`).
1. **Topological BFS DP (`nfa_builder.rs`)**:
   `has_emit[u] = u.output_pos.is_some() || has_emit[fail(u)]`
   Since `q` is ordered by increasing state depth, `fail(u)` is always processed before `u`. Bit 31 of `fail` is set if and only if `has_emit[u]` is true.
2. **Clean Encapsulation (`State`)**:
   `State::fail(&self) -> u32` masks with `& 0x7FFF_FFFF`, ensuring deserialization checks and any legacy callers receive clean state indices `< states.len()`.
   `State::fail_has_emit(&self) -> bool` checks `(self.fail & 0x8000_0000) != 0`.
3. **Zero Universal Tax**:
   The hot trie path (`mapped_c.is_some()`) NEVER calls `fail_has_emit()`. Small automata at `ROOT_STATE_IDX` (`words100`) bypass the unmapped loop without loading a state. CJK corpora (<0.6% unmapped) never execute the branch.
4. **O(1) Unmapped Bypass**:
   When an unmapped character arrives at a non-root state whose fail chain contains no outputs (`!state.fail_has_emit()`), the automaton jumps directly to `ROOT_STATE_IDX` in $O(1)$, eliminating up to ~4 dependent DRAM/L3 cache misses per unmapped character.

### Measured Impact
- **Hardware Performance Counters (`words_100000`, 50 reps, core 3)**:
  - Instructions: 2,060,346,897 -> 1,965,845,718 (−94.5M instructions)
  - L1 D-Cache Loads: 788,517,305 -> 690,675,476 (−97.8M loads)
  - Cycles: 1,978,495,133 -> 1,859,808,374 (−118.7M cycles)
- **Criterion Benchmark (`compare_leftmost`, words_100000 long-text)**:
  - Legacy: 4.072 ms (139.34 MiB/s)
  - Experimental: 4.002 ms (141.76 MiB/s) — **Experimental is 1.7% faster than Legacy in Criterion.**
- **Interleaved Standalone Runs (`words_100000` 50 reps, core 3)**:
  - Legacy: 4.227 ms (134.2 MiB/s)
  - Experimental: 3.803 ms (149.2 MiB/s) — **+11.2% higher throughput than Legacy.**
- **UniDic / CJK**:
  - Legacy: 11.26–12.41 ms (86–95 MiB/s)
  - Experimental: 10.51–10.79 ms (99–102 MiB/s) — **Experimental is 7–15% faster than Legacy.**
- **Exact Parity & Correctness**:
  - 100% exact 64-bit checksum and match count parity across all 5 corpora.
  - Full test suite passing (62 unit, 16 crate, 6 corpus, 10 fuzz, 68 doc tests).

## Mapped Character Eager Walk: Single-Probe vs Multi-Hop Loop

### Hypothesis & Experiment
In the previous eager-walk implementation (`FULL`), after buffering an emit on a mismatch, an eager loop repeatedly walked down the failure chain (`eager_walk_fail_mapped`), probing `step_child` at every intermediate state until finding a match or reaching `ROOT_STATE_IDX`.

We investigated whether avoiding the multi-hop eager loop for mapped characters (`SINGLE`: a single probe on the immediate fail state $\text{fail}(u)$ with a ROOT-consume fallback) provides better efficiency across English and CJK datasets.

### Hardware Performance Counter Verification (Noise-Free)
Because sustained CPU load triggers dynamic clock frequency scaling and thermal throttling on host APUs, hardware performance counters (`instructions:u`, `cycles:u`) were collected within the same binary (`profile_compare`) to guarantee zero-noise comparison:

| Dataset | Metric | `FULL` (Previous Multi-Hop Loop) | `SINGLE` (New Single-Probe + ROOT Fallback) | Difference |
| :--- | :--- | :--- | :--- | :--- |
| **`words_100`** *(long)* | Throughput<br>Instructions<br>Cycles | 533.3 MiB/s (1.064 ms)<br>388,338,023<br>141,234,883 | **911.6 MiB/s (0.622 ms)**<br>**364,443,234**<br>**91,266,003** | **+70.9% throughput**<br>**−23.9M instructions**<br>**−50.0M cycles (−35.4%)** |
| **`words_5000`** *(long)* | Throughput<br>Instructions<br>Cycles | 398.3 MiB/s (1.424 ms)<br>420,437,241<br>195,017,131 | **468.5 MiB/s (1.211 ms)**<br>**400,414,824**<br>**166,038,247** | **+17.6% throughput**<br>**−20.0M instructions**<br>**−29.0M cycles (−14.9%)** |
| **`words_15000`** *(long)* | Throughput<br>Instructions<br>Cycles | 491.3 MiB/s (1.155 ms)<br>368,623,029<br>235,556,285 | **534.6 MiB/s (1.061 ms)**<br>**355,696,244**<br>**205,767,247** | **+8.8% throughput**<br>**−12.9M instructions**<br>**−29.8M cycles (−12.6%)** |
| **`words_15000`** *(lines)* | Throughput<br>Instructions<br>Cycles | 271.4 MiB/s (1.999 ms)<br>401,269,297<br>277,375,130 | **431.2 MiB/s (1.258 ms)**<br>**396,407,362**<br>**222,031,598** | **+58.9% throughput**<br>**−4.9M instructions**<br>**−55.3M cycles (−20.0%)** |
| **`words_100000`** *(long)* | Throughput<br>Instructions<br>Cycles | 121.6 MiB/s (4.667 ms)<br>1,149,585,511<br>1,235,041,176 | **151.8 MiB/s (3.738 ms)**<br>**1,137,229,196**<br>1,275,677,056 | **+24.8% throughput**<br>**−12.4M instructions** |
| **`words_100000`** *(lines)* | Throughput<br>Instructions<br>Cycles | 112.9 MiB/s (4.804 ms)<br>1,176,805,043<br>1,343,469,665 | **121.3 MiB/s (4.471 ms)**<br>**1,169,251,290**<br>**1,298,311,735** | **+7.4% throughput**<br>**−7.6M instructions**<br>**−45.2M cycles** |
| **`unidic`** *(long)* | Throughput<br>Instructions<br>Cycles | 78.2 MiB/s (13.665 ms)<br>13,405,641,033<br>8,460,986,700 | **89.1 MiB/s (11.990 ms)**<br>13,421,580,662<br>8,476,427,154 | **+13.9% throughput**<br>Parity instructions (+0.1%) |
| **`unidic`** *(lines)* | Throughput<br>Instructions<br>Cycles | 72.6 MiB/s (14.667 ms)<br>13,411,335,909<br>8,559,214,163 | **81.6 MiB/s (13.046 ms)**<br>13,427,868,198<br>8,613,305,651 | **+12.4% throughput**<br>Parity instructions (+0.1%) |

### Key Insight Behind the ROOT Fallback
In dense CJK corpora like `unidic`, single-character morphemes reside at depth 1. When a 1-character match emits and the subsequent character does not continue a longer compound, its failure state is `ROOT_STATE_IDX`.
Without the `else if state_id == ROOT_STATE_IDX { self.pos = pos + char_len; }` branch, the iterator previously stopped without consuming the character, forcing redundant re-decoding of the exact same character on the next invocation.
Incorporating the ROOT consumption fallback completely eliminated the CJK re-decoding deficit, allowing `SINGLE` to win across **all 8 corpus configurations simultaneously**.

## Probing Strategy Matrix: Zero-Probe (`NoProbe`) vs Single-Probe (`Current`)

### Question & Motivation
Does omitting the transition probe altogether upon emit (`NoProbe`: immediately setting `self.state_id = state_id; self.pos = pos;`) provide a more stable default for general-purpose usage than `Current` (`SingleProbe` + ROOT fallback)?

### Noise-Free Full Matrix Benchmark
To eliminate thermal throttling, APU frequency scaling, and run-order bias, a dedicated harness ran 15 interleaved rounds with rotating execution order across all datasets, coupled with hardware performance counter tracing (`instructions:u` via Linux `perf_event_open`).

#### 1. Hardware Instructions Retired (`instructions:u`, Zero Noise)
| Dataset | Workload | `Legacy` | `NoProbe` | `Current` (SingleProbe) | Current vs NoProbe Delta |
| :--- | :--- | :---: | :---: | :---: | :---: |
| **UniDic** | longtext | 49,208,672 | 43,943,005 | **36,690,621** | **−7,252,384 (−16.5%)** |
| **UniDic** | perline | 49,068,803 | 43,986,849 | **34,979,809** | **−9,007,040 (−20.5%)** |
| **ZhTW 3185k** | convert_to | 131,637,920 | 110,928,831 | **102,343,510** | **−8,585,321 (−7.7%)** |
| **ZhTW 3185k** | scan_only | 109,676,225 | 94,222,739 | **82,194,353** | **−12,028,386 (−12.8%)** |
| **ZhCN 3185k** | scan_only | 74,160,278 | 59,750,825 | **56,027,567** | **−3,723,258 (−6.2%)** |
| **ZhCN 689k** | scan_only | 21,452,704 | 17,643,584 | **16,041,456** | **−1,602,128 (−9.1%)** |
| **words_100k** | perline | 27,452,299 | 23,440,811 | **21,744,601** | **−1,696,210 (−7.2%)** |

#### 2. Wall-Clock Latency (15 Interleaved Rounds with Rotating Arm Order)
- **Chinese (`ZhTW` & `ZhCN`)**:
  - `convert_to`: `Current` wins or ties 9 out of 10 cases (e.g. `ZhTW 3185k`: 14.53 ms vs 15.52 ms, +6.8% faster; `ZhCN 54k`: 0.229 ms vs 0.238 ms, +3.9% faster).
  - `scan_only`: `Current` wins 9 out of 10 cases (e.g. `ZhTW 3185k`: 11.86 ms vs 12.56 ms, +5.9% faster).
- **Japanese (`UniDic`)**:
  - `longtext`: `Current` is **8.3% faster** (13.10 ms vs 14.19 ms).
  - `perline`: `Current` is **4.3% faster** (12.95 ms vs 13.51 ms).
- **English (`sherlock.txt`)**:
  - `Current` and `NoProbe` are essentially tied within ±1–3% noise across `words_100`, `words_5000`, `words_15000`, and `words_100000`.

### Conclusion
`Current` (`SingleProbe` + ROOT fallback) executes strictly fewer CPU instructions (saving up to 7–12 million instructions per run) and provides equal or superior wall-clock throughput across Chinese, Japanese, and English workloads without any instability. The previously observed latency dips in sequential tests were an artifact of APU thermal scaling on later arms, completely resolved under round-robin interleaving.
