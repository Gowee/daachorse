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

### C.2 What it is NOT

Measured and rejected, each with a full test-suite pass:

- **The `output_pos()` read.** Ablation removing it from the hot loop moves
  `real` by 0.8% (within code-layout noise).
- **Unmapped-character fail-walking.** ~25% of English chars are unmapped
  (spaces/punctuation). A correct fast path that skips the guaranteed-fail
  probes and preserves the per-hop harvest (`U3`) removes **49% of inner-loop
  iterations** (888,270 -> 451,052 on words100k) yet changes wall clock by
  **+0.13%**. Those iterations never touched memory.
- **Per-iterator allocation.** `Vec::with_capacity(4)` per iterator ->
  `Vec::new()` was *worse* (+0.5%).
- **Prefetching the child slot** (`_mm_prefetch`, T0): -0.08%.
- **Inlining the ROOT re-probe** (probe ROOT instead of break-and-reprobe):
  +0.3%, and it *increases* probe count.
- **Re-enabling the fast-ROOT path + fusion**: breaks 7 fuzz tests
  (correctness bug, not a perf option).
- **UTF-8 decode**: manual `decode_char_unchecked` vs `chars()` is a wash.

### C.3 What it IS: 83% more inner-loop iterations (state-header loads)

Counter placement matters: an earlier attempt counted legacy's accesses only
inside its `if let Some(mapped_c)` branch, while counting every character for
the experimental path. That inflated the ratio and is retracted. With
comparable counters (all characters counted for both):

| words100k, per byte | legacy | experimental | ratio |
|---|---|---|---|
| character visits (incl. rewind) | 1.094 | 1.036 | exp 5% fewer |
| inner-loop iterations | 0.854 | 1.562 | **exp 83% more** |
| `states[state_id]` header loads | 0.854 | 1.562 | exp 83% more |
| `states[base ^ c]` probe loads | 0.854 | 0.797 | exp 7% fewer |
| total array accesses | 1.708 | 2.358 | exp 38% more |

Important correction: legacy **does rewind** — it visits 1.094 chars/byte vs the
experimental matcher's 1.036, i.e. legacy re-scans ~5.6% more characters
(`next_plain` advances `self.pos` only when a match is recorded and otherwise
keeps scanning forward). So legacy is not "doing less work overall"; it trades a
longer fail chain for text re-scanning.

Crucially, the experimental matcher issues slightly **fewer** random
`states[base ^ c]` probe loads (0.797 vs 0.854 per byte), because it only probes
when both `mapped_c` and `base` are `Some`. Therefore the 36.5% cycle
attribution on the probe instruction is **not** explained by probe count, and the
earlier "more cache-missing random accesses" framing is wrong.

What the experimental matcher genuinely does more of is **inner-loop
iterations** (1.562 vs 0.854 per byte). Each iteration costs a state-header
load, a `base` read, a fail read, and the loop branch. That accounts for the
+11% instructions and +23% cycles measured below; the +25% L1 misses are a
consequence of touching more distinct state headers, not of a worse probe
pattern.

Hardware counters (`perf stat`, words100k, 20 reps, same binary, only
`MatchKind` differs):

| | cycles | instructions | L1 load-misses |
|---|---|---|---|
| legacy | 2,153,085,929 | 1,337,916,856 | 11,922,180 |
| experimental | 2,664,423,419 (+24%) | 1,484,624,361 (+11%) | 14,988,316 (+26%) |

### C.4 Why legacy touches less

Legacy's `next_state_id_leftmost_unchecked` terminates the fail chain at
`DEAD_STATE_IDX` once a match has been committed:

    let fail_id = state.fail();
    if fail_id == DEAD_STATE_IDX { return ROOT_STATE_IDX; }

The experimental builder never emits a DEAD terminator — `fail_state` is only
ever `ROOT_STATE_ID` or a real suffix state (see
`build_fails_and_emits_leftmost_experimental_inner`, stage 2 step 2/3). So every
mismatch walks the full chain to ROOT: 1.562 iterations/char vs legacy's 0.854.

This is a genuine trade: legacy's shorter fail chain is bought with text
rewinding (1.094 vs 1.036 char visits/byte). The experimental design's whole
point is to eliminate rewinding, so some extra fail-chain walking is expected.
The net measured cost is +23% cycles on English prose at 644k states.

### C.5 Consequences

- The gap **scales with automaton size**, not with matches or text length:
  words100 (920 states, 16 KB — fits L1) shows the smallest absolute cost,
  words100k (644,363 states, 11.5 MB) the largest. This is why microbenchmarks
  on tiny automata or single-character texts showed no gap.
- Fixing it is a **builder change** (introduce an equivalent early termination
  in the experimental fail chain so a committed match can stop the walk), not a
  loop-level tweak. All six loop-level attempts above failed for that reason.
- unidic (dense Japanese, 236k matches, short fail chains) is unaffected and
  the experimental matcher already wins there.
