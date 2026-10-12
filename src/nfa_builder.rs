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
    /// Builds failure transitions and match emit chains for zero-rewind leftmost-longest matching.
    ///
    /// # Overview & Motivation
    ///
    /// Standard Aho-Corasick finds all overlapping pattern occurrences. When adapted for leftmost-longest
    /// matching, conventional engines (like the legacy daachorse search) simulate greedy behavior by:
    /// 1. Advancing forward in the trie until a mismatch occurs.
    /// 2. Recording the longest match encountered along the way.
    /// 3. Resetting state to ROOT and **rewinding the input text position backward** to the end of that match.
    ///
    /// On adversarial inputs (e.g. repeated overlapping candidates), rewinding causes worst-case $O(N \times L)$
    /// scanning overhead.
    ///
    /// This builder constructs an automaton that achieves strictly $O(N)$ matching with **zero text rewinding**:
    /// the text cursor always moves forward monotonically.
    ///
    /// # Core Mechanism: Active Suffix and Pre-Committed Match Chain
    ///
    /// For any trie state representing a path string from `ROOT`, the consumed prefix is partitioned into
    /// two distinct regions:
    ///
    /// ```text
    ///                 Prefix string of state `sid`
    ///   ┌──────────────────────────────────────────────┬──────────────────┐
    ///   │  Finalized matches in prefix (output_pos)    │ Active suffix    │
    ///   ├──────────────────────────────────────────────┼──────────────────┤
    ///   │  [Match 1] ... [Match k] (locked in)         │ fail state       │
    ///   └──────────────────────────────────────────────┴──────────────────┘
    /// ```
    ///
    /// 1. **Active Suffix (`fail`)**:
    ///    The longest proper suffix of the current prefix that corresponds to a valid trie state with
    ///    outgoing edges.
    ///    - *Why longest?* Under Leftmost Priority, an earlier starting position in the text always takes
    ///      precedence over a later starting position. A longer suffix starts earlier in the text than a
    ///      shorter suffix (e.g. offset `pos - len_long` is earlier than `pos - len_short`).
    ///      Transitioning to the longest extendable suffix guarantees that the automaton will not bypass
    ///      or miss the earliest potential pattern match. If this longer suffix subsequently fails on future
    ///      characters, its own fail link will fall back to shorter suffixes in chronological order.
    ///    - When the next input character mismatches, the automaton transitions directly to this state
    ///      to continue searching into future characters without rewinding.
    ///
    /// 2. **Committed Match Chain (`output_pos`)**:
    ///    All leftmost-longest matches occurring *before* the active suffix. Because branches starting
    ///    before the active suffix have already terminated, these matches are permanently finalized:
    ///    future characters can never alter, invalidate, or merge with them.
    ///
    /// # Reverse Delta-Encoded Match Chains
    ///
    /// Matches are stored as a reverse singly-linked list in `self.emits`:
    /// ```text
    ///   Head (Match k) ──(gap)──> Predecessor (Match k-1) ──(gap)──> ... ──> Match 1 ──> None
    /// ```
    /// Each `Emit` node contains:
    /// - `value`: Pattern value/ID.
    /// - `length`: Byte length of the pattern.
    /// - `gap`: Byte distance. For the head node (`Match k`), it is the distance from the end of the
    ///   state's path to the end of `Match k`. For predecessor nodes (`Match i`), it is the unmatched
    ///   byte distance from the end of `Match i` to the start of the subsequent match `Match i+1`
    ///   (`start(Match i+1) - end(Match i)`).
    /// - `pred`: 1-based index pointing to the preceding match node.
    ///
    /// ### Why Reverse Linking?
    /// Because internal gaps between matches depend only on the text between them, the entire predecessor
    /// chain `Match k-1 -> ... -> Match 1` is completely invariant and structurally shared across all
    /// descendant trie branches. When trie branches fork, states share the same predecessor nodes in
    /// `self.emits` with zero copying.
    ///
    /// ### Runtime Match Extraction (O(1) Per Match, Zero Allocations)
    /// When a mismatch occurs at text cursor `pos`:
    /// - `end(Match k) = pos - head.gap`, `start(Match k) = end - head.length`.
    /// - `end(Match k-1) = start(Match k) - em.gap`, `start(Match k-1) = end - em.length`.
    ///
    /// Nodes are pushed onto a thread-local LIFO buffer and popped in chronological order `Match 1 .. Match k`.
    ///
    /// # Construction Algorithm (BFS Traversal)
    ///
    /// States are processed in breadth-first search order. For each state `sid` with parent `p` and edge `c`:
    ///
    /// - **Fast Path 1 (Full-Path Pattern, O(1))**:
    ///   If a pattern covers the full path from root to `sid`, no trailing suffix remains.
    ///   `fail` is set to `ROOT`, and `output_pos` points to this single match (`gap = 0`).
    ///
    /// - **Fast Path 0 (Depth-1 Child of ROOT, O(1))**:
    ///   If `sid` has depth 1 and is not a pattern, its only proper suffix is empty (`ROOT`).
    ///   `fail` is set to `ROOT`, and `output_pos` is `None`.
    ///
    /// - **Fast Path 2 (Transition from Parent's Active Suffix, O(1))**:
    ///   Let `p_fail = parent.fail` be the parent's active suffix state.
    ///   - **2A (Active Suffix Continuation)**: If `p_fail` steps on `c` to a non-leaf state `pc`,
    ///     the active suffix continues advancing in-flight (`fail = pc`). Matches committed before
    ///     the suffix are identical to the parent's (`new_gap = p_head.gap + c.num_bytes()`).
    ///   - **2C (Leaf Pattern Completion)**: If `p_fail` steps on `c` to a leaf state `pc`, the active
    ///     suffix completes pattern `pc` at the end of the state. Because `pc` has no outgoing edges,
    ///     it cannot extend further and commits immediately with `gap = 0` (`fail = ROOT`), linked to
    ///     the parent's match chain.
    ///   - **2B (Root Non-Advancement Theorem)**: If `p_fail == ROOT` and ROOT has no transition on `c`,
    ///     no proper suffix of `sid` exists in the Trie (`fail = ROOT`). Matches are identical to the parent's.
    ///
    /// - **Fallback Path (In-Flight Candidate Discontinuity, Rare)**:
    ///   Entered strictly when `p_fail != ROOT` and `p_fail` has no transition on `c`.
    ///   The in-flight candidate broke; the trailing uncommitted slice is evaluated online to commit
    ///   any shadowed leftmost pattern and establish the new longest extendable proper suffix.
    pub(crate) fn build_fails_leftmost_longest(&mut self) -> Vec<u32> {
        assert!(
            self.states[usize::from_u32(ROOT_STATE_ID)]
                .output
                .is_empty(),
            "Error: empty pattern is not supported for MatchKind::LeftmostLongestExperimental"
        );

        let num_states = self.states.len();
        let mut q = Vec::with_capacity(num_states);
        let mut parents = vec![ROOT_STATE_ID; num_states];
        let mut edge_labels: Vec<Option<L>> = vec![None; num_states];
        let mut char_depths = vec![0u32; num_states];
        let mut byte_depths = vec![0usize; num_states];
        let mut deepest_outputs = vec![None; num_states];

        // Stage 1: BFS Traversal to Establish Topological Order, Metrics, and Longest Prefix Matches.
        //
        // This traversal computes:
        // 1. `q`: Topological BFS ordering ensuring ancestor states are processed before descendants.
        // 2. `parents` and `edge_labels`: Structural links for backtracing and DP transitions.
        // 3. `char_depths` and `byte_depths`: Exact character and UTF-8 byte depths from ROOT for O(1) coordinate math.
        // 4. `deepest_outputs[u]`: The deepest ancestor state (including `u`) that is a complete pattern.
        //    In leftmost matching, matches starting at ROOT (index 0) possess supreme priority; precomputing
        //    the longest pattern on the path from ROOT allows O(1) resolution of the first match.
        //
        // Initialize BFS queue starting with ROOT_STATE_ID:
        q.push(ROOT_STATE_ID);
        let mut qi = 0;
        while qi < q.len() {
            let state_id = q[qi];
            qi += 1;

            let s = &self.states[usize::from_u32(state_id)];
            let s_char_depth = char_depths[usize::from_u32(state_id)];
            let s_byte_depth = byte_depths[usize::from_u32(state_id)];
            let s_deepest = deepest_outputs[usize::from_u32(state_id)];

            for &(c, child_id) in s.edges.iter() {
                let cid = usize::from_u32(child_id);
                parents[cid] = state_id;
                edge_labels[cid] = Some(c);
                char_depths[cid] = s_char_depth + 1;
                byte_depths[cid] = s_byte_depth + c.num_bytes();
                // If child is itself a complete pattern, it is strictly longer than any ancestor pattern;
                // otherwise, it inherits the deepest pattern found on its parent path.
                deepest_outputs[cid] = if !self.states[cid].output.is_empty() {
                    Some(child_id)
                } else {
                    s_deepest
                };
                q.push(child_id);
            }
        }

        // Deduplication hash table for `Emit` nodes across all states.
        let mut emit_dedup = EmitDedup::with_capacity(q.len());

        // Stage 2: Resolving Active Suffix Fail Transitions and Pre-Committed Match Chains.
        //
        // In BFS order (skipping ROOT), for each state `curr_id` (index `sid`) we determine:
        // - `fail`: The longest active proper suffix state (with outgoing edges).
        // - `output_pos`: The pre-committed match chain occurring prior to the active suffix.
        //
        // Because parents are visited before children, parent fail states and emit chains are already finalized.
        for &curr_id in &q[1..] {
            let sid = usize::from_u32(curr_id);
            let s_char_depth = char_depths[sid] as usize;
            let s_byte_depth = byte_depths[sid];

            // Fast Path 1: if a pattern covers the entire path from root to `sid`, no suffix remains.
            if let Some(pat_st) = deepest_outputs[sid] {
                let pat_sid = usize::from_u32(pat_st);
                if char_depths[pat_sid] as usize == s_char_depth {
                    let (val, pat_bytes) = self.states[pat_sid].output[0];
                    let emit_node = Emit::new(val, pat_bytes, 0, None);
                    let key = DedupKey::new(pat_st, 0, None);
                    let head_emit_pos =
                        Some(emit_dedup.insert_or_get(key, emit_node, &mut self.emits));
                    self.states[sid].output_pos.set(head_emit_pos);
                    self.states[sid].fail.set(ROOT_STATE_ID);
                    continue;
                }
            }

            // Universal Recurrence System (Pure Cons List):
            // S = P · c inherits P's immutable match chain directly.
            // Any newly committed match (from broken candidates or leaf completion) is cons'd
            // directly onto the chain in O(1) time without intermediate arrays.
            let p = parents[sid];
            let c = edge_labels[sid].unwrap();
            let mut curr = self.states[usize::from_u32(p)].fail.get();

            let mut head_emit_pos = self.states[usize::from_u32(p)].output_pos.get();
            let mut last_end = if let Some(p_pos) = head_emit_pos {
                let p_emit = self.emits[p_pos.get() as usize - 1];
                (s_byte_depth - c.num_bytes()) - p_emit.gap as usize
            } else {
                0
            };
            let mut has_new_match = false;

            macro_rules! cons_emit {
                ($pat_state:expr, $val:expr, $len:expr, $start_b:expr, $end_b:expr) => {{
                    if let Some(prev) = head_emit_pos {
                        let p_idx = prev.get() as usize - 1;
                        let p_key = emit_dedup.keys[p_idx];
                        let p_emit = self.emits[p_idx];
                        let inter_gap = ($start_b - last_end) as u32;
                        let rekeyed =
                            Emit::new(p_emit.value, p_emit.length, inter_gap, p_emit.pred);
                        let k = DedupKey::new(p_key.pat_state, inter_gap, p_emit.pred);
                        head_emit_pos = Some(emit_dedup.insert_or_get(k, rekeyed, &mut self.emits));
                    }
                    let gap = (s_byte_depth - $end_b) as u32;
                    let node = Emit::new($val, $len, gap, head_emit_pos);
                    let k = DedupKey::new($pat_state, gap, head_emit_pos);
                    head_emit_pos = Some(emit_dedup.insert_or_get(k, node, &mut self.emits));
                    last_end = $end_b;
                    has_new_match = true;
                }};
            }

            // 1. Failure Path Walk & Broken Candidate Harvesting:
            while curr != ROOT_STATE_ID && self.child_id(curr, c).is_none() {
                let curr_sid = usize::from_u32(curr);
                if let Some(curr_head) = self.states[curr_sid].output_pos.get() {
                    let em = self.emits[curr_head.get() as usize - 1];
                    let key = emit_dedup.keys[curr_head.get() as usize - 1];
                    let curr_base = (s_byte_depth - c.num_bytes()) - byte_depths[curr_sid];
                    let end_in_curr = byte_depths[curr_sid] - em.gap as usize;
                    let start_in_curr = end_in_curr - em.length as usize;
                    cons_emit!(
                        key.pat_state,
                        em.value,
                        em.length,
                        curr_base + start_in_curr,
                        curr_base + end_in_curr
                    );
                }
                curr = self.states[curr_sid].fail.get();
            }

            // 2. Transition & Active Suffix Decision:
            let mut fail_state = ROOT_STATE_ID;
            if s_char_depth > 1 {
                if let Some(pc) = self.child_id(curr, c) {
                    let pc_sid = usize::from_u32(pc);
                    if !self.states[pc_sid].edges.is_empty() {
                        fail_state = pc;
                    } else {
                        // Leaf pattern completed at the very end of `sid`:
                        let (val, pat_bytes_len) = self.states[pc_sid].output[0];
                        let pat_bytes = byte_depths[pc_sid];
                        cons_emit!(
                            pc,
                            val,
                            pat_bytes_len,
                            s_byte_depth - pat_bytes,
                            s_byte_depth
                        );
                        fail_state = ROOT_STATE_ID;
                    }
                }
            }

            // 3. If no new match was cons'd, simply expand parent's head gap by |c|:
            if !has_new_match {
                if let Some(p_pos) = head_emit_pos {
                    let p_idx = p_pos.get() as usize - 1;
                    let p_key = emit_dedup.keys[p_idx];
                    let p_emit = self.emits[p_idx];
                    let new_gap = p_emit.gap + c.num_bytes() as u32;
                    let node = Emit::new(p_emit.value, p_emit.length, new_gap, p_emit.pred);
                    let k = DedupKey::new(p_key.pat_state, new_gap, p_emit.pred);
                    head_emit_pos = Some(emit_dedup.insert_or_get(k, node, &mut self.emits));
                }
            }

            self.states[sid].output_pos.set(head_emit_pos);
            self.states[sid].fail.set(fail_state);
        }

        q.remove(0);
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

impl DedupKey {
    #[inline(always)]
    const fn new(pat_state: u32, gap: u32, pred: Option<NonZeroU32>) -> Self {
        Self {
            pat_state,
            gap,
            pred,
        }
    }
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
