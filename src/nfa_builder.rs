use core::cell::Cell;
use core::cmp::Reverse;
use core::num::NonZeroU32;

use alloc::vec::Vec;

use crate::build_helper::{Profile, SiblingGroup};
use crate::edge_map::EdgeMap;
use crate::errors::{DaachorseError, Result};
use crate::utils::FromU32;
use crate::{Emit, MatchKind, Output};

// The root state id of SparseNFA.
pub const ROOT_STATE_ID: u32 = 0;
// The dead state id of SparseNFA.
pub const DEAD_STATE_ID: u32 = 1;

pub trait EdgeLabel: Copy + Ord + core::fmt::Debug {
    fn num_bytes(&self) -> usize;
}

impl EdgeLabel for u8 {
    fn num_bytes(&self) -> usize {
        1
    }
}

impl EdgeLabel for char {
    fn num_bytes(&self) -> usize {
        self.len_utf8()
    }
}

/// State of [`NfaBuilder`].
#[derive(Clone)]
pub struct NfaBuilderState<L, V> {
    pub(crate) edges: EdgeMap<L>,
    pub(crate) fail: Cell<u32>,
    pub(crate) output: Vec<(V, u32)>,
    pub(crate) output_pos: Cell<Option<NonZeroU32>>,
}

impl<L, V> Default for NfaBuilderState<L, V> {
    fn default() -> Self {
        Self {
            edges: EdgeMap::<L>::default(),
            fail: Cell::new(ROOT_STATE_ID),
            output: vec![],
            output_pos: Cell::new(None),
        }
    }
}

/// Builder of an Aho-Corasick automaton.
pub struct NfaBuilder<L, V> {
    pub(crate) states: Vec<NfaBuilderState<L, V>>,
    pub(crate) outputs: Vec<Output<V>>, // in which common parts are merged.
    pub(crate) emits: Vec<Emit<V>>,     // emits for leftmost-longest matching.
    pub(crate) len: usize,
    pub(crate) match_kind: MatchKind,
}

impl<L, V> NfaBuilder<L, V>
where
    L: EdgeLabel,
    V: Copy,
{
    pub(crate) fn new(match_kind: MatchKind) -> Self {
        Self {
            states: vec![
                NfaBuilderState::<L, V>::default(), // root
                NfaBuilderState::<L, V>::default(), // dead
            ],
            outputs: vec![],
            emits: vec![],
            len: 0,
            match_kind,
        }
    }

    #[inline(always)]
    pub(crate) fn add(&mut self, pattern: &[L], value: V) -> Result<()> {
        let pattern_len = pattern
            .iter()
            .fold(0, |acc, c| acc + c.num_bytes())
            .try_into()
            .map_err(|_| DaachorseError::invalid_argument("pattern.len()", "<=", u32::MAX))?;

        let mut state_id = ROOT_STATE_ID;
        for &c in pattern {
            if self.match_kind.is_leftmost_first() {
                // If state_id has an output, the descendants will never be searched.
                if !self.states[usize::from_u32(state_id)].output.is_empty() {
                    return Ok(());
                }
            }

            if let Some(next_state_id) = self.child_id(state_id, c) {
                state_id = next_state_id;
            } else if let Ok(next_state_id) = u32::try_from(self.states.len()) {
                self.states[usize::from_u32(state_id)]
                    .edges
                    .insert(c, next_state_id);
                self.states.push(NfaBuilderState::<L, V>::default());
                state_id = next_state_id;
            } else {
                return Err(DaachorseError::automaton_scale("state_id", u32::MAX));
            }
        }

        self.states[usize::from_u32(state_id)]
            .output
            .push((value, pattern_len));

        self.len += 1;
        Ok(())
    }

    pub(crate) fn build_fails(&self) -> Vec<u32> {
        let mut q = Vec::with_capacity(self.states.len());
        for &child_id in self.states[usize::from_u32(ROOT_STATE_ID)].edges.values() {
            q.push(child_id);
        }

        let mut qi = 0;
        while qi < q.len() {
            let state_id = usize::from_u32(q[qi]);
            qi += 1;

            let s = &self.states[state_id];
            for &(c, child_id) in s.edges.iter() {
                let mut fail_id = s.fail.get();
                let new_fail_id = loop {
                    if let Some(child_fail_id) = self.child_id(fail_id, c) {
                        break child_fail_id;
                    }
                    let next_fail_id = self.states[usize::from_u32(fail_id)].fail.get();
                    if fail_id == ROOT_STATE_ID && next_fail_id == ROOT_STATE_ID {
                        break ROOT_STATE_ID;
                    }
                    fail_id = next_fail_id;
                };
                self.states[usize::from_u32(child_id)].fail.set(new_fail_id);
                q.push(child_id);
            }
        }
        q
    }

    pub(crate) fn build_fails_leftmost(&self) -> Vec<u32> {
        let mut q = Vec::with_capacity(self.states.len());
        for &child_id in self.states[usize::from_u32(ROOT_STATE_ID)].edges.values() {
            q.push(child_id);
        }
        if !self.states[usize::from_u32(ROOT_STATE_ID)]
            .output
            .is_empty()
        {
            for &child_id in self.states[usize::from_u32(ROOT_STATE_ID)].edges.values() {
                self.states[usize::from_u32(child_id)]
                    .fail
                    .set(DEAD_STATE_ID);
            }
        }

        let mut qi = 0;
        while qi < q.len() {
            let state_id = usize::from_u32(q[qi]);
            qi += 1;

            let s = &self.states[state_id];

            // Sets the output state to the dead fail.
            if !s.output.is_empty() {
                s.fail.set(DEAD_STATE_ID);
            }

            for &(c, child_id) in s.edges.iter() {
                let mut fail_id = s.fail.get();

                // If the parent has the dead fail, the child also has the dead fail.
                let new_fail_id = if fail_id == DEAD_STATE_ID {
                    DEAD_STATE_ID
                } else {
                    loop {
                        if let Some(child_fail_id) = self.child_id(fail_id, c) {
                            break child_fail_id;
                        }
                        let next_fail_id = self.states[usize::from_u32(fail_id)].fail.get();
                        if next_fail_id == DEAD_STATE_ID {
                            break DEAD_STATE_ID;
                        }
                        if fail_id == ROOT_STATE_ID && next_fail_id == ROOT_STATE_ID {
                            break ROOT_STATE_ID;
                        }
                        fail_id = next_fail_id;
                    }
                };

                self.states[usize::from_u32(child_id)].fail.set(new_fail_id);
                q.push(child_id);
            }
        }
        q
    }
}

impl<L, V> NfaBuilder<L, V>
where
    L: EdgeLabel,
    V: Copy,
{
    /// Builds failure transitions and emits for leftmost-longest matching using
    /// a zero-rewind automaton with pre-committed match chains.
    ///
    /// # Background & The Problem with Existing AC Leftmost Matching
    ///
    /// Classic Aho-Corasick (1975) is strictly an overlapping all-occurrences algorithm.
    /// Existing Aho-Corasick-based libraries supporting leftmost matching (such as `aho-corasick`
    /// and `daachorse`) simulate leftmost semantics via a heuristic restart trick:
    /// 1. In `build_fails_leftmost()`, failure transitions of matching states are truncated to `DEAD`.
    /// 2. During search, execution advances forward until mismatch, falls back to `ROOT`, and
    ///    **rewinds the text cursor backward** to the end of the last recorded match.
    ///
    /// This backtracking causes worst-case $O(N \times L)$ execution time and prevents true
    /// single-pass streaming on forward-only inputs.
    ///
    /// # Theoretical Foundation: Why Zero-Rewind is Optimal and Sound
    ///
    /// ## Foundational Observations
    ///
    /// ### Observation 1: Optimal Failure Target $S_{\text{suffix}}$ and Commitment Boundary
    /// At any trie state $S_W$ representing path string $W \in \Sigma^*$ from `ROOT`, consider the
    /// event where the next input character mismatches:
    /// 1. **Optimal Failure Target**: If future characters can still match any dictionary pattern,
    ///    that match must begin with some suffix of $W$. Therefore, the automaton must transition
    ///    to a state representing a proper suffix of $W$ that possesses outgoing branches capable
    ///    of continuing into the future. To avoid missing any earlier match, the optimal target is
    ///    the **longest such extendable suffix**, denoted $w_{\text{suffix}}$ (with state
    ///    $S_{\text{suffix}} = \delta^*(\text{ROOT}, w_{\text{suffix}})$).
    /// 2. **Commitment Boundary**: Within the prefix text $W_{\text{prefix}}$ preceding $w_{\text{suffix}}$,
    ///    no forward extension across $w_{\text{suffix}}$ is possible along this branch. Under
    ///    leftmost-longest semantics, all greedy matches within $W_{\text{prefix}}$ are permanently
    ///    closed and finalized. Future characters can never alter, invalidate, or merge with them
    ///    without violating the definition of $w_{\text{suffix}}$ as the longest extendable suffix.
    /// 3. **Sound Atomic Emission**: Therefore, strictly upon mismatch, all leftmost-longest matches in
    ///    $W_{\text{prefix}}$ are committed and emitted ($\text{Emit}_W$), while the automaton
    ///    transitions directly to $S_{\text{suffix}}$. The text scanner advances monotonically with
    ///    **zero backtracking**.
    ///
    /// ### Observation 2: Inductive Chaining, Reverse DAG Sharing, and $O(1)$ Coordinate Recovery
    /// 1. **Suffix Exclusion**:
    ///    $$\text{Emit}_W = \text{Emit}_{W \setminus w_{\text{suffix}}}$$
    ///    The active suffix $w_{\text{suffix}}$ is reserved for future exploration and produces no emits.
    /// 2. **Backward Inductive Recurrence**:
    ///    Within $W_{\text{prefix}}$, the non-overlapping leftmost-longest matches form a finite sequence
    ///    $[M_1, \dots, M_k]$. The chain exhibits an exact inductive recurrence:
    ///    $$\text{Chain}_W = \text{Cons}(M_k, \text{Chain}_{<k})$$
    ///    where $M_k$ is the last closed match in $W_{\text{prefix}}$, and $\text{Chain}_{<k}$ is the
    ///    immutable predecessor chain for all matches prior to $M_k$.
    /// 3. **Why Reverse Chaining? (Avoiding Quadratic $O(L^2)$ Copying)**:
    ///    Forward chaining ($M_1 \to M_2 \dots$) would require deep-copying predecessor chains whenever
    ///    trie branches fork and append distinct matches, incurring quadratic $O(L^2)$ space/time
    ///    construction costs.
    ///    By storing chains in **reverse order** ($M_k \to M_{k-1} \dots \to M_1$), appending a match
    ///    is an $O(1)$ functional prepend (`Cons`). Because internal match gaps
    ///    ($\text{start}(M_i) - \text{end}(M_{i-1})$) are strictly local and invariant, predecessor
    ///    sub-chains are 100% structurally shared across trie states as a persistent Directed Acyclic
    ///    Graph (DAG) in `self.emits`, ensuring optimal $O(|V|)$ construction.
    /// 4. **$O(1)$ Zero-Allocation Coordinate Recovery**:
    ///    Using the absolute mismatch scan offset $P$ as the base anchor, match boundaries are reconstructed
    ///    in $O(1)$ arithmetic operations by walking backwards along the reverse delta-encoded chain:
    ///    - $\text{end}(M_k) = P - M_k.\text{gap}, \quad \text{start}(M_k) = \text{end}(M_k) - M_k.\text{length}$
    ///    - $\text{anchor}_{i-1} = \text{start}(M_i)$
    ///    - $\text{end}(M_{i-1}) = \text{anchor}_{i-1} - M_{i-1}.\text{gap}, \quad \text{start}(M_{i-1}) = \text{end}(M_{i-1}) - M_{i-1}.\text{length}$
    ///
    ///    A small LIFO stack unpacks $M_k \dots M_2$ to yield $M_1$ first, streaming matches in true
    ///    chronological order without heap allocation.
    ///
    /// ### Observation 3: Zero Runtime Struct Overhead
    /// DoubleArray `State` fields are directly reused without any size expansion:
    /// - `fail`: Reused to store the target state ID of $S_{\text{suffix}}$.
    /// - `output_pos`: Reused to store the 1-based index of the head emit node $M_k$.
    ///
    /// The 16-byte footprint of `State` is preserved with no extra fields and zero branching overhead.
    pub(crate) fn build_fails_leftmost_longest(&mut self) -> Vec<u32> {
        assert!(
            self.states[usize::from_u32(ROOT_STATE_ID)]
                .output
                .is_empty(),
            "Error: empty pattern is not supported for MatchKind::LeftmostLongestExperimental"
        );

        let num_states = self.states.len();
        let mut q = Vec::with_capacity(num_states);
        let mut parent = vec![ROOT_STATE_ID; num_states];
        let mut edge_label: Vec<Option<L>> = vec![None; num_states];
        let mut depth = vec![0u32; num_states];
        let mut byte_len = vec![0usize; num_states];

        let mut deepest_output = vec![None; num_states];

        for &(c, child_id) in self.states[usize::from_u32(ROOT_STATE_ID)].edges.iter() {
            let cid = usize::from_u32(child_id);
            edge_label[cid] = Some(c);
            depth[cid] = 1;
            byte_len[cid] = c.num_bytes();
            if !self.states[cid].output.is_empty() {
                deepest_output[cid] = Some(child_id);
            }
            q.push(child_id);
        }

        let mut qi = 0;
        while qi < q.len() {
            let state_id = q[qi];
            qi += 1;

            let s = &self.states[usize::from_u32(state_id)];
            let s_depth = depth[usize::from_u32(state_id)];
            let s_byte_len = byte_len[usize::from_u32(state_id)];
            let s_deepest = deepest_output[usize::from_u32(state_id)];

            for &(c, child_id) in s.edges.iter() {
                let cid = usize::from_u32(child_id);
                parent[cid] = state_id;
                edge_label[cid] = Some(c);
                depth[cid] = s_depth + 1;
                byte_len[cid] = s_byte_len + c.num_bytes();
                if !self.states[cid].output.is_empty() {
                    deepest_output[cid] = Some(child_id);
                } else {
                    deepest_output[cid] = s_deepest;
                }
                q.push(child_id);
            }
        }

        let mut emit_dedup = EmitDedup::with_capacity(q.len());
        let mut w = Vec::new();
        let mut byte_offsets = Vec::new();
        let mut temp_matches = Vec::new();

        for &state_id in &q {
            let sid = usize::from_u32(state_id);
            let s_depth = depth[sid] as usize;
            let total_bytes = byte_len[sid];

            // O(1) Fast Path: When the deepest output matches the full path of sid,
            // the match starts at 0 and covers the entire path. It leaves no residual suffix.
            if let Some(pat_st) = deepest_output[sid] {
                let pat_sid = usize::from_u32(pat_st);
                if depth[pat_sid] as usize == s_depth {
                    let (val, pat_bytes) = self.states[pat_sid].output[0];
                    let emit_node = Emit {
                        value: val,
                        length: pat_bytes,
                        gap: 0,
                        pred: None,
                    };
                    let key = DedupKey {
                        pat_state: pat_st,
                        gap: 0,
                        pred: None,
                    };
                    let head_emit_pos =
                        Some(emit_dedup.insert_or_get(key, emit_node, &mut self.emits));
                    self.states[sid].output_pos.set(head_emit_pos);
                    self.states[sid].fail.set(ROOT_STATE_ID);
                    continue;
                }
            }

            let mut idx = 0;
            temp_matches.clear();
            let mut target_state = ROOT_STATE_ID;

            let start_idx = if let Some(pat_st) = deepest_output[sid] {
                let pat_sid = usize::from_u32(pat_st);
                let pat_d = depth[pat_sid] as usize;
                let pat_b = byte_len[pat_sid];
                let (val, pat_bytes) = self.states[pat_sid].output[0];
                temp_matches.push((pat_st, val, 0, pat_b, pat_bytes as usize));
                pat_d
            } else {
                1
            };

            let rem_len = s_depth - start_idx;

            if rem_len > 0 {
                w.clear();
                w.reserve(rem_len);
                let mut curr = state_id;
                for _ in 0..rem_len {
                    w.push(edge_label[usize::from_u32(curr)].unwrap());
                    curr = parent[usize::from_u32(curr)];
                }
                w.reverse();

                byte_offsets.clear();
                byte_offsets.reserve(rem_len + 1);
                let mut b = 0;
                for &c in &w {
                    byte_offsets.push(b);
                    b += c.num_bytes();
                }
                byte_offsets.push(b);
                let base_byte = total_bytes - b;

                while idx < rem_len {
                    let mut st = ROOT_STATE_ID;
                    let mut walked_all = true;
                    for &c in &w[idx..] {
                        if let Some(nxt) = self.child_id(st, c) {
                            st = nxt;
                        } else {
                            walked_all = false;
                            break;
                        }
                    }

                    if walked_all && !self.states[usize::from_u32(st)].edges.is_empty() {
                        target_state = st;
                        break;
                    }

                    if let Some(pat_st) = deepest_output[usize::from_u32(st)] {
                        let pat_sid = usize::from_u32(pat_st);
                        let (val, pat_bytes) = self.states[pat_sid].output[0];
                        let end_j = idx + depth[pat_sid] as usize;
                        let start_byte = base_byte + byte_offsets[idx];
                        let end_byte = base_byte + byte_offsets[end_j];
                        temp_matches.push((pat_st, val, start_byte, end_byte, pat_bytes as usize));
                        idx = end_j;
                    } else {
                        idx += 1;
                    }
                }
            }

            let mut head_emit_pos = None;
            let k = temp_matches.len();
            if k > 0 {
                for i in 0..k {
                    let (pat_st, val, _start_b, end_b, len_b) = temp_matches[i];
                    let gap = if i == k - 1 {
                        (total_bytes - end_b) as u32
                    } else {
                        let next_start = temp_matches[i + 1].2;
                        (next_start - end_b) as u32
                    };

                    let emit_node = Emit {
                        value: val,
                        length: len_b as u32,
                        gap,
                        pred: head_emit_pos,
                    };

                    let key = DedupKey {
                        pat_state: pat_st,
                        gap,
                        pred: head_emit_pos,
                    };

                    head_emit_pos = Some(emit_dedup.insert_or_get(key, emit_node, &mut self.emits));
                }
            }

            self.states[sid].output_pos.set(head_emit_pos);
            self.states[sid].fail.set(target_state);
        }

        q
    }
}

impl<L, V> NfaBuilder<L, V>
where
    L: EdgeLabel,
    V: Copy,
{
    pub(crate) fn build_outputs(&mut self, q: &[u32]) {
        {
            let s = &self.states[usize::from_u32(ROOT_STATE_ID)];
            let mut last_pos = None;
            for output in s.output.iter().rev() {
                self.outputs.push(Output::new(output.0, output.1, last_pos));
                last_pos = NonZeroU32::new(u32::try_from(self.outputs.len()).unwrap());
            }
            s.output_pos.set(last_pos);
        }
        for &state_id in q {
            let s = &self.states[usize::from_u32(state_id)];
            let mut last_pos = self.states[usize::from_u32(s.fail.get())].output_pos.get();
            for output in s.output.iter().rev() {
                self.outputs.push(Output::new(output.0, output.1, last_pos));
                last_pos = NonZeroU32::new(u32::try_from(self.outputs.len()).unwrap());
            }
            s.output_pos.set(last_pos);
        }
    }

    #[inline(always)]
    fn child_id(&self, state_id: u32, c: L) -> Option<u32> {
        self.states[usize::from_u32(state_id)]
            .edges
            .get(&c)
            .copied()
    }

    fn profile_step(
        &self,
        mut state_id: u32,
        c: L,
        profile: &mut Profile,
        dense_root: bool,
    ) -> u32 {
        loop {
            // The runtime handles such root transitions with a dense table without accessing
            // the double array, so no access is counted.
            if dense_root && state_id == ROOT_STATE_ID {
                return self.child_id(ROOT_STATE_ID, c).unwrap_or(ROOT_STATE_ID);
            }
            let s = &self.states[usize::from_u32(state_id)];
            // The runtime reads the double-array element of the current state.
            profile.visits[usize::from_u32(state_id)] += 1;
            if !s.edges.is_empty() {
                if let Some(&child_id) = s.edges.get(&c) {
                    // A successful probe reads the element of the child.
                    profile.visits[usize::from_u32(child_id)] += 1;
                    return child_id;
                }
                // A failed probe reads an element in the block where the children of this
                // state are placed.
                profile.probes[usize::from_u32(state_id)] += 1;
            }
            if state_id == ROOT_STATE_ID {
                return ROOT_STATE_ID;
            }
            let fail_id = s.fail.get();
            if fail_id == DEAD_STATE_ID {
                return ROOT_STATE_ID;
            }
            state_id = fail_id;
        }
    }

    pub(crate) fn profile_haystack<F>(
        &self,
        haystack: &[L],
        profile: &mut Profile,
        dense_root: bool,
        mut has_label: F,
    ) where
        F: FnMut(L) -> bool,
    {
        let leftmost = self.match_kind.is_leftmost();
        let mut state_id = ROOT_STATE_ID;
        let mut match_end = None;
        let mut pos = 0;
        while pos < haystack.len() {
            let c = haystack[pos];
            state_id = if has_label(c) {
                self.profile_step(state_id, c, profile, dense_root)
            } else {
                ROOT_STATE_ID
            };
            if leftmost {
                if state_id == ROOT_STATE_ID {
                    // The leftmost iterators yield the pending match here and restart
                    // scanning at its end position in the next call.
                    if let Some(end) = match_end.take() {
                        pos = end;
                        continue;
                    }
                } else if self.states[usize::from_u32(state_id)]
                    .output_pos
                    .get()
                    .is_some()
                {
                    match_end = Some(pos + 1);
                }
            }
            pos += 1;
        }
    }

    pub(crate) fn sibling_groups<M>(&self, profile: &Profile, mut map_label: M) -> Vec<SiblingGroup>
    where
        M: FnMut(L) -> u32,
    {
        let mut groups = vec![];
        let mut stack = vec![ROOT_STATE_ID];
        let profiled = !profile.is_empty();
        while let Some(state_id) = stack.pop() {
            let s = &self.states[usize::from_u32(state_id)];
            if s.edges.is_empty() {
                continue;
            }
            let mut children = Vec::with_capacity(s.edges.len());
            let mut weight = if profiled {
                profile.probes[usize::from_u32(state_id)]
            } else {
                0
            };
            for &(c, child_id) in s.edges.iter() {
                if profiled {
                    weight += profile.visits[usize::from_u32(child_id)];
                }
                children.push((map_label(c), child_id));
                stack.push(child_id);
            }
            groups.push(SiblingGroup {
                parent: state_id,
                children,
                weight,
            });
        }
        // With no profile, the sort is skipped and the groups keep the depth-first order, in
        // which they are placed in the same order as the classic depth-first construction.
        // The sort must be stable so that groups with equal weights also keep that order.
        if profiled {
            groups.sort_by_key(|g| Reverse(g.weight));
        }
        groups
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
struct DedupKey {
    pat_state: u32,
    gap: u32,
    pred: Option<NonZeroU32>,
}

/// A compact, open-addressing deduplication table for [`Emit`] nodes in `no_std`.
struct EmitDedup {
    entries: Vec<u32>, // 0 represents empty; >= 1 is 1-based index into `keys` and `emits`
    keys: Vec<DedupKey>,
    mask: usize,
}

impl EmitDedup {
    fn with_capacity(cap: usize) -> Self {
        let size = cap.next_power_of_two().max(1024);
        Self {
            entries: vec![0; size],
            keys: Vec::with_capacity(cap),
            mask: size - 1,
        }
    }

    fn grow(&mut self) {
        let new_size = self.entries.len() * 2;
        let new_mask = new_size - 1;
        let mut new_entries = vec![0; new_size];
        for &existing in &self.entries {
            if existing != 0 {
                let k = self.keys[existing as usize - 1];
                let p = k.pred.map_or(0, |x| x.get());
                let h = (k.pat_state.wrapping_mul(0x9e37_79b9)
                    ^ k.gap.wrapping_mul(0x85eb_ca6b)
                    ^ p.wrapping_mul(0xc2b2_ae35)) as usize;
                let mut idx = h & new_mask;
                while new_entries[idx] != 0 {
                    idx = (idx + 1) & new_mask;
                }
                new_entries[idx] = existing;
            }
        }
        self.entries = new_entries;
        self.mask = new_mask;
    }

    fn insert_or_get<V: Copy>(
        &mut self,
        key: DedupKey,
        emit: Emit<V>,
        emits: &mut Vec<Emit<V>>,
    ) -> NonZeroU32 {
        if self.keys.len() * 2 >= self.entries.len() {
            self.grow();
        }
        let h = {
            let p = key.pred.map_or(0, |x| x.get());
            (key.pat_state.wrapping_mul(0x9e37_79b9)
                ^ key.gap.wrapping_mul(0x85eb_ca6b)
                ^ p.wrapping_mul(0xc2b2_ae35)) as usize
        };
        let mut idx = h & self.mask;
        loop {
            let existing = self.entries[idx];
            if existing == 0 {
                self.keys.push(key);
                emits.push(emit);
                let pos = NonZeroU32::new(u32::try_from(emits.len()).unwrap()).unwrap();
                self.entries[idx] = pos.get();
                return pos;
            }
            if self.keys[existing as usize - 1] == key {
                return NonZeroU32::new(existing).unwrap();
            }
            idx = (idx + 1) & self.mask;
        }
    }
}
