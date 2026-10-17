# Theoretical Observations on Leftmost-First Zero-Rewind Automata

## 1. Executive Summary

This document formalizes why **Leftmost-First** (`MatchKind::LeftmostFirst`) and **Leftmost-Longest** (`MatchKind::LeftmostLongest`) cannot share the same precomputed failure links (`fail`) or match emit chains (`output_pos`).

Although both semantics share the primary leftmost criterion ($\min(\text{start})$), their tie-breaking policies at identical start positions produce **different committed prefix matches**, which establish **different commitment boundaries**, thereby **enabling or blocking completely different failure suffixes**.

---

## 2. The Decisive Counterexample

Consider the pattern set registered in the following order:

$$\mathcal{P} = \{ \text{"a"}, \text{"ab"}, \text{"abcd"}, \text{"bcd"}, \text{"d"} \}$$

Suppose the automaton consumes the path string $W = \text{"abcd"}$ and encounters a mismatch on the subsequent character (e.g., text `"abcdx"` where `"abcd"` fails on `'x'`):

```text
Offsets:       0   1   2   3   4   5
Text:          a   b   c   d   x
Path W:        [ a   b   c   d ]
Candidates:    ├─ "a" ─┤
               ├──── "ab" ────┤
               ├──────── "abcd" ────────┤
                   ├───── "bcd" ────┤
                           ├── "d" ──┤
```

### Case A: Leftmost-Longest Resolution
1. Along the prefix $W = \text{"abcd"}$, patterns starting at offset $0$ are `"a"`, `"ab"`, and `"abcd"`.
2. When `"abcd"` fails on `'x'`, the tie-breaker at offset $0$ evaluates `"a"` (length 1) vs `"ab"` (length 2).
3. Under **Leftmost-Longest**, the longer pattern wins:
   $$\text{"ab"} \text{ commits at } [0, 2] \implies \text{Commitment Boundary } E = 2$$
4. Under non-overlapping matching, no future match may start before offset $E = 2$:
   - Suffix `"bcd"` starts at offset $1 < 2 \implies$ **BLOCKED** (overlaps with committed match `"ab"`).
   - Suffix `"cd"` does not exist in $\mathcal{P}$.
   - Suffix `"d"` starts at offset $3 \ge 2 \implies$ **VALID** (starts after `"ab"`).
5. **Resulting Automaton State**:
   $$\text{fail}(\text{"abcd"}) = \mathbf{\text{"d"}}, \quad \text{emit}(\text{"abcd"}) = \mathbf{\text{"ab"}}$$

---

### Case B: Leftmost-First Resolution
1. Along the prefix $W = \text{"abcd"}$, patterns starting at offset $0$ are `"a"` (registered 1st) and `"ab"` (registered 2nd).
2. When `"abcd"` fails on `'x'`, the tie-breaker at offset $0$ evaluates `"a"` vs `"ab"`.
3. Under **Leftmost-First**, the earliest-registered pattern wins:
   $$\text{"a"} \text{ commits at } [0, 1] \implies \text{Commitment Boundary } E = 1$$
4. Under non-overlapping matching, the uncommitted space is $W[1..] = \text{"bcd"}$:
   - Suffix `"bcd"` starts at offset $1 \ge 1 \implies$ **VALID** (does not overlap with `"a"`).
5. **Resulting Automaton State**:
   $$\text{fail}(\text{"abcd"}) = \mathbf{\text{"bcd"}}, \quad \text{emit}(\text{"abcd"}) = \mathbf{\text{"a"}}$$

---

### Comparison Matrix

| Property | Leftmost-Longest | Leftmost-First |
| :--- | :--- | :--- |
| **Committed Match $M_1$** | `"ab"` (longer) | `"a"` (earlier registered) |
| **Commitment Boundary $E$** | $2$ | $1$ |
| **Available Suffix Window** | $W[2..] = \text{"cd"}$ | $W[1..] = \text{"bcd"}$ |
| **Is `"bcd"` Permitted?** | **No** (overlaps with `"ab"`) | **Yes** (starts after `"a"`) |
| **$\text{fail}(\text{"abcd"})$** | $\mathbf{\text{"d"}}$ | $\mathbf{\text{"bcd"}}$ |
| **$\text{output\_pos}(\text{"abcd"})$** | $\mathbf{\text{"ab"}}$ | $\mathbf{\text{"a"}}$ |

$$\text{fail}_{\text{Longest}}(\text{"abcd"}) \ne \text{fail}_{\text{First}}(\text{"abcd"})$$

$$\text{emit}_{\text{Longest}}(\text{"abcd"}) \ne \text{emit}_{\text{First}}(\text{"abcd"})$$

---

## 3. The Foundational Law: Suffix-Commitment Invariant

In any zero-rewind non-overlapping automaton, the active pending suffix $w_{\text{suffix}}$ is governed by a strict structural invariant:

> **The Suffix-Commitment Invariant**:
> For any path string $W$ with committed matches $M_1, \dots, M_k$ ending at $E = \text{end}(M_k)$:
> 1. The active suffix $w_{\text{suffix}}$ must be a proper suffix of $W$ contained entirely within the uncommitted remainder $W[E..]$:
>    $$\text{start}(w_{\text{suffix}}) \ge E \iff |w_{\text{suffix}}| \le |W| - E$$
> 2. Among all valid suffixes satisfying (1) with outgoing transitions in the Trie, $w_{\text{suffix}}$ is the **longest** (starts leftmost after $E$).

Because Leftmost-First and Leftmost-Longest resolve prefix candidates with different tie-breakers:
1. They select different prefix matches ($M_{\text{first}} \ne M_{\text{longest}}$).
2. They establish different commitment boundaries ($E_{\text{first}} \ne E_{\text{longest}}$).
3. They permit different active suffixes ($w_{\text{first}} \ne w_{\text{longest}}$).

---

## 4. Disambiguation: "Longer Suffix" vs "Longer Pattern"

A common source of confusion is equating "longer suffix" with "longer pattern":

- **"Longer Pattern"**: Refers to pattern length $|P|$. Leftmost-First explicitly does **not** prefer longer patterns over shorter ones.
- **"Longer Suffix"**: Refers to the suffix of the consumed path string $W$. Because $\text{start}(w) = \text{pos} - |w|$, a longer suffix starts **further to the left** in the haystack ($\text{pos} - 10 < \text{pos} - 3$).

Under Leftmost-First:
$$\text{start}(A) < \text{start}(B) \implies A \text{ wins over } B$$
Regardless of pattern length or registration order, an earlier start position always dominates a later start position. Therefore, within the uncommitted window $W[E..]$, **Leftmost-First must still pursue the longest extendable suffix** to preserve leftmost priority.

---

## 5. Requirements for an Adapted Leftmost-First Build Function

To construct a zero-rewind Leftmost-First automaton:

1. **Prefix Match Selection Policy**:
   At each position $p$ along a path:
   - Leftmost-Longest selects $\arg\max_{P}(\text{length}(P))$.
   - Leftmost-First selects $\arg\min_{P}(\text{pattern\_id}(P))$.

2. **Dynamic Boundary-Constrained Suffix Walk**:
   During BFS failure resolution for child state $S = P \cdot c$:
   - Suffix candidates $w = \delta^*(\text{ROOT}, \text{curr})$ walked along failure links must satisfy $\text{start}(w) \ge E$, where $E$ is the boundary established by Leftmost-First prefix selection.
   - Any suffix starting before $E$ must be pruned or bypassed.

---

# Appendix A — Experimental Matcher: Measured Findings (tyo2, musl-static)

Environment: single-vCPU NixOS box `tyo2`, statically-linked musl binaries,
`taskset -c 0`, arms run round-robin (alternating) so drift cancels; medians
reported. Harnesses: `sparse` (words_100 shapes: real/rootmiss/failwalk) and
`official` (words100/5k/15k/100k + unidic, `exp/leg-off`; **higher = experimental faster**).

## A.1 Where the per-character cost is NOT

Instrumented counters (staged code) on words_100:

| shape | chars | fail_steps | output reads | hits | steps/char | hits/read |
|---|---|---|---|---|---|---|
| real (sherlock) | 594915 | 830617 | 407018 | 10 | 1.396 | ~2e-5 |
| rootmiss | 200000 | 0 | 0 | 0 | 0.000 | — |
| failwalk | 400007 | 503382 | 105908 | 2110 | 1.258 | 0.020 |

- The `state.output_pos()` read is read ~0.68×/char on prose and yields a
  chain 10 times in 407k reads — near-pure overhead, but too infrequent to
  explain the gap.
- **Ablation**: removing the `output_pos()` read + harvest from the hot loop
  (`/tmp/dac-noread`, timing-only, parity knowingly broken) moves `real` by
  **0.992 (≈0.8%)** — within the code-layout noise band (see A.3). The read is
  **not** the load-bearing code.
- `rootmiss` never enters the non-root inner loop (0 fail steps) and the
  experimental matcher **wins** there (exp/leg ≈ 0.76). The cost tracks
  *entering the non-root inner loop at all*, not the emit read.

## A.2 The locals-hoist IS a real win (reproduced on tyo2)

Variant `loc`: hoist the loop-carried `pos` / `state_id` out of `self` into
registers inside `ensure_buffered_impl` (write back at every exit). Full suite +
fuzz differential green.

Official datasets, 6 alternating rounds, medians of `exp/leg-off`:

| dataset | staged (`full`) | hoist (`loc`) | loc/full |
|---|---|---|---|
| words100 | 0.817 | **0.912** | 1.116 |
| words5k | 0.757 | **0.902** | 1.192 |
| words15k | 0.722 | **0.849** | 1.176 |
| words100k | 0.784 | **0.853** | 1.089 |
| unidic | 1.032 | **1.111** | 1.076 |

Sparse shapes (8 rounds): real 0.976→0.883, failwalk 0.667→0.629 (both win);
rootmiss 0.764→0.788 (≈3% "loss", but that path is byte-identical — noise, A.3).

## A.3 Methodology caveat: `rootmiss` is a noise probe, not a signal

`rootmiss` takes the ROOT fast path and never executes the inner loop, so any
variant that only edits the inner loop runs **byte-identical code** on it. Yet
its measured ratio swings by 15–20% between binaries (e.g. 0.786 vs 0.594 for
`loc` vs `noread`). Therefore **any `rootmiss` delta is code-layout/alignment
noise** and must not be read as a real regression or improvement. Likewise, a
change that moves only `real`/`failwalk` by <~2% is unresolvable at this noise
level. Treat |Δ| ≲ 3% on any single shape as inconclusive.

## A.4 Removing the fast-ROOT path is not a net win

Variant `live` (user's in-tree experiment): fast-ROOT probe and ROOT-fallback
fusion commented out, everything folded into the inner loop. Full suite green.
Official: wins words15k (1.150) but loses words100k (0.969) and unidic (0.896)
vs `full`; sparse rootmiss improves (0.764→0.929) while real/failwalk worsen.
Net: **mixed, not adopted.**

## Appendix B — Callgrind profiling of the hot match loop (local box, -O3, LTO off)

Instrument: `valgrind --tool=callgrind --cache-sim=yes`, comparing `cg_legacy` vs `cg_exp`
on 400 sherlock lines (words_100, prefilter off), ~16M instructions total each.

Whole-program:
- Legacy: 15,813,809 Ir ; Dr 3,210,167 ; Dw 1,161,753.
- Experimental: 16,243,958 Ir (+2.7%) ; Dr 3,822,382 (+19.1%) ; Dw 1,648,698 (+41.9%).
- L1/LL cache miss rates **identical** (LL miss 0.2%, L1-d 1.3-1.6%) — so it is not
  memory **capacity**, it is memory **traffic** + branch density.

Per-line hot spots in `src/charwise/iter.rs` (experimental):

| cost | line |
|---|---|
| 533,094 Ir / 327,376 Dr (3.28/8.56%) | `states.get_unchecked(child_idx).check() == expect` (inside `step_child`) |
| 308,474 (1.90%) | `if let (Some(code), Some(base)) = ...` (step_child entry) |
| 293,722 (1.81%) | `while pos < bytes.len()` |
| 287,392 (1.77%) | Fast Root probe entry `if state_id == ROOT_STATE_IDX` |
| 250,212 (1.54%) | post-fail ROOT check `if state_id == ROOT_STATE_IDX` |
| 199,088 (1.23%) | `if let Some(ep) = emit_pos` |
| 180,194 (1.11%) | `let state = states.get_unchecked(state_id as usize)` |

Versus legacy (`src/charwise.rs:next_state_id_leftmost_unchecked`):
- 355,396 Ir / 177,698 Dr per `child_idx = base.get() ^ mapped_c` + check.
- legacy calls the transition function ONCE per character; experimental wraps the
  same XOR+load+check in `step_child` and calls it 3× per outer iteration
  (fast-root, inner probe, fusion) plus the redundant ROOT re-probe on a
  no-harvest fail-to-ROOT.
- The locals-hoist already deleted the loop-carried `pos`/`state_id` load/store
  traffic; the remaining cost (~10% on English) is mostly branch density from
  the double-`Option` match in `step_child` and the redundant root reprobe.

Promising experiments still worth running (gate by correctness tests):
1. **Inline `step_child` to legacy form** (manual `if let Some(base) { ... }`,
   eliminating the double-`Option` destructure) — expected a few %, matches the
   `355k+355k+355k+177k` legacy 4-line form.
2. The root-reprobe merge (root-consume on no-harvest fail-to-root) requires
   returning-or-advancing without consuming the current byte twice; attempt only
   when perf attributes the probe concretely on tyo2.
3. Per `perf` on tyo2 (preferred over local — single-vCPU, matches production
   target): branch-misses per `state_id` / `mapped_c` predictability.

## Appendix C — Root cause of the residual per-character gap (measured on `zin`)

Setup: `zin` (4-core, idle, `perf` available), statically-linked musl binaries,
`taskset -c 0`, alternating arms, medians. Both matchers built from the same
source tree; only `MatchKind` differs. Harness scans sherlock line-by-line
(official shape).

### C.1 The gap is real and reproducible on `zin`

| dataset | exp/leg (ns/char) | matches |
|---|---|---|
| words100 | 1.360 | 10 |
| words5k | 1.423 | 567 |
| words15k | 1.430 | 2,100 |
| words100k | 1.328 | 23,826 |
| unidic | **0.936** | 236,442 |

Earlier local-box numbers (which showed parity or exp winning) were noise-
contaminated; the single-vCPU-ish idle `zin` result is consistent across runs.
Note exp is *faster* on unidic — the gap is not universal.

**Counter hygiene note.** Any `perf stat` that runs a harness doing
`build(...)` *and* the scan includes the builder, which costs **1.5–1.9 billion
cycles — comparable to the entire 20-rep scan** (0.64 B cycles for legacy,
0.77 B for experimental). Earlier cycle/instruction/miss figures in this
appendix were contaminated this way and are superseded by the build-subtracted
numbers below (same binary, only `MatchKind` differs, `REPS=20`):

| scan-only, words100k | cycles | instructions | L1 miss | LLC loads |
|---|---|---|---|---|
| legacy | 641,297,449 | 531,477,016 | 4,711,861 | 9,659,584 |
| experimental | 772,681,567 | 560,645,054 | 6,082,624 | 12,228,672 |
| ratio | **1.205** | 1.055 | 1.291 | 1.266 |

The builder is *itself* more expensive for the experimental matcher (+23%
cycles), which is worth noting separately from the scan gap.

### C.2 What it is NOT

Measured and rejected, each with a full test-suite pass:

- **The `output_pos()` read.** Ablation removing it from the hot loop moves
  `real` by 0.8% (within code-layout noise).
- ~~**Unmapped-character fail-walking.**~~ **RETRACTED — this is the cause.**
  An earlier version of this section claimed the unmapped-character walk was
  irrelevant because variant `U3` (skip the guaranteed-fail *probes*, keep the
  per-hop harvest) removed 49% of inner-loop iterations for +0.13% wall clock.
  That experiment removed the wrong half. `U3` still performs the full
  fail-chain walk; only the transition probe was skipped. The walk's
  `states[]` loads are what cost, and they were never removed. See C.3.
- **Per-iterator allocation.** `Vec::with_capacity(4)` per iterator ->
  `Vec::new()` was *worse* (+0.5%).
- **Prefetching the child slot** (`_mm_prefetch`, T0): -0.08%.
- **Inlining the ROOT re-probe** (probe ROOT instead of break-and-reprobe):
  +0.3%, and it *increases* probe count.
- **Re-enabling the fast-ROOT path + fusion**: breaks 7 fuzz tests
  (correctness bug, not a perf option).
- **UTF-8 decode**: manual `decode_char_unchecked` vs `chars()` is a wash.

### C.3 What it IS: the fail-chain walk on *unmapped* characters

The decisive split is not "mapped vs unmapped characters in general" but **what
the two matchers do when the character is not in the mapper at all** (spaces,
punctuation, capitals — 27.3% of sherlock's characters for words100k).

Legacy short-circuits at the top of `next_state_id_leftmost_unchecked`:

    if let Some(mapped_c) = self.mapper.get(c) { loop { ... } }
    else { ROOT_STATE_IDX }

One `mapper` lookup, **zero `states[]` accesses**, done.

The experimental matcher cannot do that, because it has pre-committed emits
stored *on states* (`State::output_pos`). When the automaton resets to ROOT it
must first harvest any pending emit that lives on the current state or on an
ancestor along the fail chain, **in innermost-first order**. So it enters the
generic loop, loads the state, finds no transition is possible, steps to `fail`,
loads again, and repeats until it either finds an `output_pos` or reaches ROOT.

Measured on words100k (sherlock, single pass, 568,811 chars):

| | legacy | experimental |
|---|---|---|
| fail hops on **mapped** chars | 19,089 | 19,745 |
| fail hops on **unmapped** chars | 0 | **301,881** |
| total fail hops | 19,089 | 321,626 |
| unmapped characters | 155,368 (27.3%) | same |
| avg walk depth per unmapped char that is not already at ROOT | — | 2.72 states |

**On mapped characters the two matchers are at parity** (19,745 vs 19,089 hops —
a 656-hop difference over 568k characters). Essentially the *entire* gap is the
301,881 extra random `states[]` loads triggered by the 27% of characters that
are not in the alphabet. That is 0.531 extra loads per text byte into an 11.5 MB
array.

The cost per hop is directly measurable from the build-subtracted counters in
C.1: the experimental matcher spends 6,569,205 extra cycles and 68,538 extra L1
misses per rep over 301,881 extra hops — **21.8 cycles and a 22.7% L1 miss rate
per hop**. A dependent load into a large array that misses ~23% of the time is
exactly the cache-miss signature this appendix originally suspected but
attributed to the wrong access.

### C.3.1 Causal confirmation

Take the same pattern set and the same words, and delete every non-`a-z`
character so nothing is unmapped. The automaton, the matcher code, and the word
sequence are unchanged; only the unmapped fraction goes to zero.

| sherlock, words100k | exp/leg wall | unmapped chars |
|---|---|---|
| prose as-is | **1.421** | 27.3% |
| same prose glued `a-z` | **0.879** | 0% |

The sign flips. With no unmapped characters the experimental matcher is 12%
*faster*. This is not a correlation — it is the same binary, same automaton,
same text content, with only the unmapped-character fraction changed.

unidic (0.0% unmapped, 236k matches) is the independent replication: exp/leg
0.94–0.96, i.e. exp wins, matching the prediction.

### C.4 Why the walk is not free to remove

The walk looks wasteful (it usually finds nothing), so I tried three removals:

- **UM0 — reset to ROOT and skip the walk.** Fails: emits 3,716 matches instead
  of 23,826 (−84%). Pending emits are simply lost. (Its apparent speedup was
  measured while emitting 84% fewer matches and is meaningless.)
- **UM1/UM2 — harvest only the *current* state, then reset.** Correct match
  *set*, wrong *order*, and 5 fuzz tests fail. Emits also live on deeper
  ancestors, and the order they are flushed in is observable.
- **UM3 — restructure the walk to harvest innermost-first without probing.**
  Passes the full suite **and all 9 fuzz differentials**, with exact match-count
  parity. Timing: a wash — 1.153–1.301 vs 1.183–1.305 exp/leg across four
  alternating rounds on `zin`, i.e. the intervals overlap completely.

UM3 is the useful negative result: a correct, probe-free rewrite of the same
walk is a **wash**. So the cost is the *walk itself* — the random `states[]`
loads — not the transition probes inside it. There is no loop-level shape that
makes it cheaper.

### C.5 Why this is inherent, and the real trade

The walk exists because of a genuine design difference:

- **Legacy** keeps the single pending match in a **register** (`last_output_pos`)
  and, when the scan resets, *rewinds* to that match's end and re-scans. Cheap
  to flush, expensive to re-scan.
- **Experimental** keeps pending matches **on the states** (`output_pos`) so it
  never rewinds. Cheap to scan, but flushing requires walking the fail chain to
  find the next committed emit.

So the two designs trade rewind cost against flush cost. Which one wins is
decided by the data:

| regime | rewind cost | flush cost | winner |
|---|---|---|---|
| dense matches, full alphabet (unidic) | high (1.68 char visits/char) | low (0.0% unmapped) | **experimental** (0.94–0.96) |
| sparse matches, many unmapped chars (English prose) | low (1.09 char visits/char) | high (27% unmapped) | **legacy** (1.19–1.43) |

This is why the gap scales with automaton size (see C.6) and why it is absent
on Japanese: a bigger array makes each flush load more likely to miss cache,
and Japanese text has essentially no unmapped characters.

### C.6 Consequences

- The English-prose deficit is **fully explained** by the unmapped-character
  flush walk. There is no residual "mystery" cost: on mapped characters the two
  matchers are within 0.1% of each other on hop count.
- A fix would have to eliminate the flush walk without reintroducing rewind —
  i.e. keep the pending-emit head in the iterator (as legacy does) while still
  pre-committing chains on states. That is a **design change**, not a tweak.
  Every loop-level attempt (C.2, plus UM0/UM1/UM2/UM3) either broke correctness
  or changed nothing.
- The gap **scales with automaton size**, not with matches or text length:
  words100 (920 states, 16 KB — fits L1) shows the smallest cost, words100k
  (644,363 states, 11.5 MB) the largest. This is why microbenchmarks on tiny
  automata showed no gap.
- unidic (dense Japanese, 0% unmapped) is where the zero-rewind design pays off
  and the experimental matcher already wins.
