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
