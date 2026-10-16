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
