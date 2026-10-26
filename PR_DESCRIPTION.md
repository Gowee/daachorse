# [RFC] Experimental Linear-Time Leftmost-Longest Matching

### Background

In the existing implementation of leftmost-longest (LL) matching, standard failure links are followed upon mismatch as long as no earlier match was encountered along the current branch. However, when an earlier, shorter match has been recorded (e.g. pattern "a" while exploring towards "abcd"), and the longer candidate subsequently mismatches, the engine must commit the earlier match and rewind the input cursor back to the end of that match to resume searching.

Because of this cursor rewinding, worst-case matching time is $\mathcal{O}(N \cdot L)$, where $N$ is text length and $L$ is the maximum pattern length. Some implementations in other libraries avoid rewinding by running a full overlapping search first and filtering matches afterwards, which can take $\mathcal{O}(N^2)$ in the worst case.

### What this PR does

This PR experimentally implements a leftmost-longest matching automaton that scans input text strictly forward without cursor rewinding:
- Matching time: $\mathcal{O}(N)$ worst-case, scanning text in a single pass.
- Space: $\mathcal{O}(|S|)$, where $|S|$ is the number of states.
- Construction time: $\mathcal{O}(M + |S|)$, where $M = \sum |p_i|$ is the total pattern length.

### Core Observation

During forward matching, when an upcoming character $c$ causes a mismatch at state $s_w$ (path string $w$ cannot extend further on $c$):
1. The trailing portion of $w$ that exists in the trie serves as the pending suffix, tracked by $\text{fail}(s_w)$. The automaton transitions to this state to continue matching $c$.
2. In the prefix of $w$ that precedes this pending suffix, all completed leftmost-longest matches are finalized. Under leftmost priority, no future character can extend or supersede them, making them ready to be committed.

Instead of rewinding the text at runtime, both components are precalculated for each state at construction time:
- $\text{fail}(s)$: the failure link pointing to the pending suffix state.
- $\text{emit}(s)$: the committed matches to output upon mismatch.

If $\text{fail}(s_w)$ also cannot accept $c$, the automaton continues walking the failure chain (emitting any intermediate matches along the chain) until reaching a state that accepts $c$, or reaching `ROOT`.

### Example

Consider patterns $P = \{\text{"a"}, \text{"abce"}, \text{"bcd"}\}$ and input $T = \text{"abcd"}$:

- **Existing daachorse**:
  Scans `a` $\to$ `b` $\to$ `c` along `abce`. Character `d` causes a mismatch on `abce`. Because match `a` (0..1) was recorded earlier, the engine outputs `a` and rewinds the cursor back to index 1 (`b`). It then rescans `b`, `c`, `d` to match `bcd` (1..4).
- **Experimental (non-rewinding) matcher**:
  For state $s_{abc}$, the trie precalculates:
  - Committed match before the suffix: `a` (0..1)
  - Pending suffix: $s_{bc}$, via $\text{fail}(s_{abc}) = s_{bc}$
  When `d` mismatches at $s_{abc}$:
  1. It outputs `a` (0..1) immediately.
  2. Transitions directly to state $s_{bc}$ without moving the text cursor.
  3. State $s_{bc}$ accepts `d`, advancing directly to match `bcd` (1..4).
  Both `a` and `bcd` are matched in a single forward pass without rewinding.

### Construction Recurrence (Dynamic Programming)

The failure link $\text{fail}(s)$ and committed matches $\text{emit}(s)$ are precomputed in BFS order.

For a child state $s_w$ representing string $w = u \cdot c$ (parent state $s_u$ extended by character $c$):

1. **If $w$ is a pattern**:
   Since $w$ starts at the root, it dominates all interior suffixes under leftmost-longest rules:
   - $\text{fail}(s_w) = \text{ROOT}$
   - $\text{emit}(s_w) = \{ w \}$

2. **If $w$ is not a pattern**:
   $s_w$ inherits $\text{emit}(s_u)$. The failure chain of parent $s_u$ ($s_{u_1} = \text{fail}(s_u), s_{u_2} = \text{fail}(s_{u_1}), \dots$) is traversed:
   - For each failure state $s_{u_i}$:
     - If $s_{u_i \cdot c}$ exists in the trie: it becomes the pending suffix. Set $\text{fail}(s_w) = s_{u_i \cdot c}$ and stop the traversal.
     - If $s_{u_i \cdot c}$ does not exist: the suffix $u_i$ cannot extend on $c$. Its committed matches are accumulated into $\text{emit}(s_w)$, and the traversal advances to $s_{u_{i+1}} = \text{fail}(s_{u_i})$.
   - If no suffix in the chain accepts $c$, set $\text{fail}(s_w) = \text{ROOT}$.

*(Note: If the surviving pending suffix itself completes a leaf pattern on $c$, that pattern is appended to $\text{emit}(s_w)$ and $\text{fail}(s_w)$ resets to `ROOT`.)*

### Trade-offs

1. The existing 16-byte `State` struct is preserved without adding fields (`output_pos` points to an `emits: Vec<Emit<V>>` pool of committed matches). Each `Emit<V>` tracks an additional `gap` field indicating the byte distance between adjacent matches. Chains are linked in reverse order (pointing backwards to predecessors), enabling different trie branches to structurally share common predecessor chains. Consequently, the iterator introduces an internal heap allocation (`Vec`) to buffer multi-match chains for chronological output, unlike the legacy iterator which is allocation-free (though single-match chains return directly in registers without buffer usage).
2. Bit 31 of `state.fail` is used to flag whether a state or any state along its failure chain contains an emit. This allows immediately resetting to `ROOT` on unmapped characters or EOF without traversing empty failure chains, while restricting maximum states to $2^{31} - 1$.
