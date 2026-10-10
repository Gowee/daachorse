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
    /// prefix-committed semi-DFA construction with gap chaining.
    ///
    /// # Theoretical Foundation: Greedy Leftmost-Longest Decomposition
    ///
    /// In classic Aho-Corasick leftmost-longest matching, finding the longest match starting
    /// at the leftmost position requires scanning forward until mismatch, then **rewinding the
    /// text cursor** back to `match.start + match.len` to begin the next search. This backtracking
    /// causes worst-case $O(N \times L)$ time (e.g., patterns like `a`, `aa`, `aaa`... on haystack `a^N`).
    ///
    /// ## Principle of Prefix Commitment
    ///
    /// In this automaton, each state $S$ reachable via trie path $W \in \Sigma^*$ represents a known
    /// text prefix. Under the greedy leftmost-longest matching semantics, any string $W$ has a
    /// **unique, deterministic greedy partition**:
    ///
    /// $$W = M_1 \circ M_2 \circ \dots \circ M_k \circ S_{\text{suffix}}$$
    ///
    /// where:
    /// 1. $M_1, M_2, \dots, M_k$ are non-overlapping matches that are *already closed and committed*
    ///    within $W$. Because their start positions are the earliest possible and their lengths are
    ///    maximal among all candidates starting at those positions, **no future character in the
    ///    haystack can ever invalidate or alter them**.
    /// 2. $S_{\text{suffix}}$ is an incomplete trailing suffix that could potentially be extended
    ///    into a longer match by future input characters.
    ///
    /// ## Failure Edge Redirection and Zero-Rewind Invariant
    ///
    /// When the text input mismatches at state $S$:
    /// - We know that the prefix $W$ cannot be extended further along this trie branch.
    /// - Therefore, all committed matches $M_1, \dots, M_k$ are immediately finalized and emitted.
    /// - The automaton transitions to the state corresponding to $S_{\text{suffix}}$ (or ROOT if
    ///   $S_{\text{suffix}}$ is empty).
    /// - The text scan pointer **never moves backward**. Execution advances purely forward,
    ///   guaranteeing strictly optimal $O(N)$ streaming time!
    ///
    /// ## Gap Chaining
    ///
    /// Rather than buffering historical text, the matches $M_k \to M_{k-1} \to \dots \to M_1$
    /// are stored in reverse chronological order as an immutable chain of [`Emit`] nodes.
    /// Each node encodes its length, its relative byte distance (`gap`) to the next anchor,
    /// and a predecessor pointer `pred`. This enables $O(1)$ match coordinate reconstruction.
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

        for &(c, child_id) in self.states[usize::from_u32(ROOT_STATE_ID)].edges.iter() {
            edge_label[usize::from_u32(child_id)] = Some(c);
            depth[usize::from_u32(child_id)] = 1;
            byte_len[usize::from_u32(child_id)] = c.num_bytes();
            q.push(child_id);
        }

        let mut qi = 0;
        while qi < q.len() {
            let state_id = q[qi];
            qi += 1;

            let s = &self.states[usize::from_u32(state_id)];
            let s_depth = depth[usize::from_u32(state_id)];
            let s_byte_len = byte_len[usize::from_u32(state_id)];

            for &(c, child_id) in s.edges.iter() {
                parent[usize::from_u32(child_id)] = state_id;
                edge_label[usize::from_u32(child_id)] = Some(c);
                depth[usize::from_u32(child_id)] = s_depth + 1;
                byte_len[usize::from_u32(child_id)] = s_byte_len + c.num_bytes();
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
            w.clear();
            w.reserve(s_depth);
            let mut curr = state_id;
            while curr != ROOT_STATE_ID {
                w.push(edge_label[usize::from_u32(curr)].unwrap());
                curr = parent[usize::from_u32(curr)];
            }
            w.reverse();

            byte_offsets.clear();
            byte_offsets.reserve(w.len() + 1);
            let mut b = 0;
            for &c in &w {
                byte_offsets.push(b);
                b += c.num_bytes();
            }
            byte_offsets.push(b);
            let total_bytes = b;

            let mut idx = 0;
            temp_matches.clear();
            let mut target_state = ROOT_STATE_ID;

            while idx < w.len() {
                let mut suffix_state = ROOT_STATE_ID;
                let mut suffix_in_trie = true;
                if idx > 0 {
                    let mut st = ROOT_STATE_ID;
                    for &c in &w[idx..] {
                        if let Some(nxt) = self.child_id(st, c) {
                            st = nxt;
                        } else {
                            suffix_in_trie = false;
                            break;
                        }
                    }
                    if suffix_in_trie {
                        suffix_state = st;
                    }
                } else {
                    suffix_in_trie = false;
                }

                if suffix_in_trie && !self.states[usize::from_u32(suffix_state)].edges.is_empty() {
                    target_state = suffix_state;
                    break;
                }

                let mut st = ROOT_STATE_ID;
                let mut best_match = None;
                let mut j = idx;
                let mut curr_sub_bytes = 0usize;
                while j < w.len() {
                    let c = w[j];
                    curr_sub_bytes += c.num_bytes();
                    if let Some(nxt) = self.child_id(st, c) {
                        st = nxt;
                        j += 1;
                        let s_ref = &self.states[usize::from_u32(st)];
                        if let Some(&(val, pat_bytes)) = s_ref.output.first() {
                            if pat_bytes as usize == curr_sub_bytes {
                                best_match = Some((st, val, pat_bytes as usize, j));
                            }
                        }
                    } else {
                        break;
                    }
                }

                if let Some((pat_st, val, len_bytes, end_j)) = best_match {
                    let start_byte = byte_offsets[idx];
                    let end_byte = byte_offsets[end_j];
                    temp_matches.push((pat_st, val, start_byte, end_byte, len_bytes));
                    idx = end_j;
                } else if suffix_in_trie
                    && !self.states[usize::from_u32(suffix_state)].output.is_empty()
                {
                    let (val, pat_bytes) = *self.states[usize::from_u32(suffix_state)]
                        .output
                        .first()
                        .unwrap();
                    let start_byte = byte_offsets[idx];
                    let end_byte = total_bytes;
                    temp_matches.push((
                        suffix_state,
                        val,
                        start_byte,
                        end_byte,
                        pat_bytes as usize,
                    ));
                    target_state = ROOT_STATE_ID;
                    break;
                } else {
                    idx += 1;
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
