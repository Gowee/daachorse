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
    /// Patterns terminating at this state: `(value, pattern_length)`.
    ///
    /// For standard Aho-Corasick, multiple patterns may terminate at the same state.
    /// In `LeftmostLongestExperimental`, only the longest pattern terminating here (at most one)
    /// is kept and inspected.
    pub(crate) output: Vec<(V, u32)>,
    /// 1-based index pointing to the head of the output/emit chain.
    ///
    /// This field has dual semantics depending on `MatchKind`:
    /// - **Standard / LeftmostFirst**: 1-based index into `outputs: Vec<Output<V>>`,
    ///   forming a dictionary suffix link chain (traversed via `Output::parent()`).
    /// - **LeftmostLongestExperimental**: 1-based index into `emits: Vec<Emit<V>>`,
    ///   forming a reverse delta-encoded pre-committed match chain (traversed via `Emit::pred()`).
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
    /// States are processed in breadth-first search order. For each state `sid` with parent `parent_id` and edge `c`:
    ///
    /// - **Fast Path 1 (Full-Path Pattern / Barrier Reset, O(1))**:
    ///   If a pattern covers the full path from root to `sid`, no trailing suffix remains.
    ///   Under leftmost-longest priority, an earlier root match supersedes any proper suffix.
    ///   `fail` is set to `ROOT`, and `output_pos` points to this single match (`gap = 0`).
    ///
    /// - **Universal Recurrence System (Pure Cons List)**:
    ///   For any state `sid = parent · c`, `sid` inherits the parent's finalized match chain.
    ///   1. **Failure Path Walk & Broken Candidate Harvesting**:
    ///      While active suffix candidate `curr` cannot transition on `c` (`child_id(curr, c) == None`),
    ///      the candidate has broken. Any pre-committed non-overlapping matches along `curr` are
    ///      harvested and topologically spliced onto `sid`'s chain via reverse functional consing.
    ///      `curr` then falls back along its own `fail` link.
    ///   2. **Active Suffix Transition & Leaf Pattern Completion**:
    ///      If `child_state = child_id(curr, c)` exists:
    ///      - If `child_state` is non-leaf, active suffix continues in-flight (`fail = child_state`).
    ///      - If `child_state` is a leaf pattern, it completes at the end of `sid` and commits immediately
    ///        onto the chain with `gap = 0` (`fail = ROOT`).
    ///   3. **Head Gap Expansion**:
    ///      If no new matches were cons'd, the parent's head match gap simply expands by `|c|` bytes.
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

        // Stage 1: BFS Traversal to Establish Topological Order, Metrics, and Parent Links.
        //
        // This traversal computes:
        // 1. `q`: Topological BFS ordering ensuring ancestor states are processed before descendants.
        // 2. `parents` and `edge_labels`: Structural links for parent lookup and DP transitions.
        // 3. `char_depths` and `byte_depths`: Exact character and UTF-8 byte depths from ROOT for O(1) coordinate math.
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

            for &(c, child_id) in s.edges.iter() {
                let cid = usize::from_u32(child_id);
                parents[cid] = state_id;
                edge_labels[cid] = Some(c);
                char_depths[cid] = s_char_depth + 1;
                byte_depths[cid] = s_byte_depth + c.num_bytes();
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
            let s_char_depth = char_depths[sid];
            let s_byte_depth = byte_depths[sid];

            // Fast Path: Root-Covered Pattern (Barrier Reset).
            // If `sid` itself is a complete pattern from ROOT, under leftmost-longest priority
            // it supersedes any proper suffix. `fail` is reset to ROOT and `gap` is 0.
            if let Some(&(val, pat_bytes)) = self.states[sid].output.first() {
                let emit_node = Emit::new(val, pat_bytes, 0, None);
                let key = DedupKey::new(curr_id, 0, None);
                let head_emit_pos = Some(emit_dedup.insert_or_get(key, emit_node, &mut self.emits));
                self.states[sid].output_pos.set(head_emit_pos);
                self.states[sid].fail.set(ROOT_STATE_ID);
                continue;
            }

            // Universal Recurrence System (Pure Cons List):
            // S = P · c inherits P's immutable match chain directly.
            // Newly committed matches are spliced onto the chain:
            // - Leaf pattern completions are cons'd directly onto the head in O(1) time.
            // - Broken candidate suffixes are appended via functional path copying in O(k) time
            //   (where k <= depth(curr) is the candidate chain length), while all ancestor matches
            //   preceding the suffix remain 100% structurally shared.
            let parent_id = parents[sid];
            let c = edge_labels[sid].unwrap();
            let mut curr = self.states[usize::from_u32(parent_id)].fail.get();

            let parent_head = self.states[usize::from_u32(parent_id)].output_pos.get();
            let mut chain = EmitChainBuilder::new(
                &mut self.emits,
                &mut emit_dedup,
                parent_head,
                s_byte_depth,
                c.num_bytes(),
            );

            // 1. Failure Path Walk & Broken Candidate Splicing:
            //
            // When an in-flight suffix candidate `curr = fail(parent_id)` cannot transition on `c`,
            // its active pattern pursuit has failed and broken. Any pre-committed non-overlapping
            // matches locked in along `curr`'s prefix must now be harvested and transferred onto `sid`.
            //
            // ## Path Copying & Topological Splicing Rationale:
            // Why must we copy and append `curr.output_pos` onto `chain` in forward chronological order?
            // 1. Coordinate Frame Shift:
            //    `curr`'s prefix is a proper suffix of `sid` starting at `curr_base`. All match byte offsets
            //    in `curr` must be translated into `sid`'s global coordinate frame.
            // 2. Topological Re-linking:
            //    The earliest match M_0 in `curr` previously had `M_0.pred == None`. When transferring
            //    to `sid`, `M_0` must now link to `parent_id`'s prior matches (`parent_head`). Because existing
            //    `Emit` nodes are immutable and structurally shared by other branches in the Trie,
            //    functional path copying (re-consing the suffix chain) is mathematically necessary.
            //
            // ## Space Complexity & Bounds Analysis (Ω(|P|) ~ O(|V|)):
            // One might wonder whether path copying could cause quadratic O(L^2) node explosion.
            // In fact, the total number of unique `Emit` nodes is tightly bounded to Ω(|P|) ~ O(|V|):
            // 1. Path Length Invariant (Non-Overlapping):
            //    In leftmost-longest semantics, all matches along any path are strictly non-overlapping
            //    and have length >= 1. Thus, the match chain length at any state S is strictly bounded by
            //    its string depth: k_S <= depth(S) <= max_{p ∈ P} |p|.
            // 2. Suffix-Localized Splicing:
            //    `curr` is a strict proper suffix of `parent_id` (depth(curr) < depth(parent_id)). Only the matches within
            //    this local suffix slice are copied. All preceding ancestor matches (S_before_suffix)
            //    remain 100% structurally shared without copying: M_0' directly links to `parent_head` in O(1).
            // 3. DAG Hash-Consing (EmitDedup):
            //    Identical (pat_state, gap, pred) triples across divergent branches are collapsed into
            //    a canonical DAG, preventing duplicate node allocations across the entire automaton.
            // Hence, the total number of unique `Emit` nodes across the entire automaton is Ω(|P|) ~ O(|V|),
            // where |P| is the pattern count and |V| is the number of Trie states (|V| <= ∑ |p| + 1).
            while curr != ROOT_STATE_ID
                && Self::child_id_from_states(&self.states, curr, c).is_none()
            {
                let curr_sid = usize::from_u32(curr);
                if let Some(curr_head) = self.states[curr_sid].output_pos.get() {
                    let curr_base = (s_byte_depth - c.num_bytes()) - byte_depths[curr_sid];
                    chain.append_suffix(curr_head, byte_depths[curr_sid], curr_base);
                }
                curr = self.states[curr_sid].fail.get();
            }

            // 2. Transition & Active Suffix Decision:
            let mut fail_state = ROOT_STATE_ID;
            if s_char_depth > 1 {
                if let Some(child_state) = Self::child_id_from_states(&self.states, curr, c) {
                    let child_sid = usize::from_u32(child_state);
                    if !self.states[child_sid].edges.is_empty() {
                        fail_state = child_state;
                    } else {
                        // Leaf pattern completed at the very end of `sid`:
                        let (val, pat_bytes_len) = self.states[child_sid].output[0];
                        let pat_bytes = byte_depths[child_sid];
                        chain.cons(
                            child_state,
                            val,
                            pat_bytes_len,
                            s_byte_depth - pat_bytes,
                            s_byte_depth,
                        );
                        fail_state = ROOT_STATE_ID;
                    }
                }
            }

            self.states[sid].output_pos.set(chain.finish(c.num_bytes()));
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
        Self::child_id_from_states(&self.states, state_id, c)
    }

    #[inline(always)]
    fn child_id_from_states(states: &[NfaBuilderState<L, V>], state_id: u32, c: L) -> Option<u32> {
        states[usize::from_u32(state_id)].edges.get(&c).copied()
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

/// Helper to construct and splice delta-encoded reverse match chains with DAG hash-consing.
///
/// Implements functional list operations (`cons`, `append_suffix`, `expand_head_gap`)
/// over [`Emit`] nodes in `emits`.
struct EmitChainBuilder<'a, V> {
    emits: &'a mut Vec<Emit<V>>,
    dedup: &'a mut EmitDedup,
    head: Option<NonZeroU32>,
    last_end: usize,
    s_byte_depth: usize,
    has_new_match: bool,
}

impl<'a, V: Copy> EmitChainBuilder<'a, V> {
    #[inline(always)]
    fn new(
        emits: &'a mut Vec<Emit<V>>,
        dedup: &'a mut EmitDedup,
        head: Option<NonZeroU32>,
        s_byte_depth: usize,
        c_num_bytes: usize,
    ) -> Self {
        let last_end = if let Some(p_pos) = head {
            let p_emit = emits[p_pos.get() as usize - 1];
            (s_byte_depth - c_num_bytes) - p_emit.gap as usize
        } else {
            0
        };
        Self {
            emits,
            dedup,
            head,
            last_end,
            s_byte_depth,
            has_new_match: false,
        }
    }

    /// Functional `cons`: links a new match onto the head of the chain.
    ///
    /// 1. If a previous head exists, rekeys its gap to the inter-match distance `(start_byte - self.last_end)`.
    /// 2. Creates a new head node with trailing gap `(self.s_byte_depth - end_byte)` pointing to the previous head.
    /// 3. Collapses identical nodes into a canonical DAG via `self.dedup`.
    #[inline(always)]
    fn cons(&mut self, pat_state: u32, val: V, len: u32, start_byte: usize, end_byte: usize) {
        debug_assert!(start_byte >= self.last_end);
        debug_assert!(self.s_byte_depth >= end_byte);
        if let Some(prev) = self.head {
            let p_idx = prev.get() as usize - 1;
            let p_key = self.dedup.keys[p_idx];
            let p_emit = self.emits[p_idx];
            let inter_gap = (start_byte - self.last_end) as u32;
            let rekeyed = Emit::new(p_emit.value, p_emit.length, inter_gap, p_emit.pred);
            let k = DedupKey::new(p_key.pat_state, inter_gap, p_emit.pred);
            self.head = Some(self.dedup.insert_or_get(k, rekeyed, self.emits));
        }
        let gap = (self.s_byte_depth - end_byte) as u32;
        let node = Emit::new(val, len, gap, self.head);
        let k = DedupKey::new(pat_state, gap, self.head);
        self.head = Some(self.dedup.insert_or_get(k, node, self.emits));
        self.last_end = end_byte;
        self.has_new_match = true;
    }

    /// Functional copy-and-append of a suffix state's match chain (Scheme `append`).
    ///
    /// Walks down the suffix's pre-committed match chain along `em.pred` and conses each match
    /// onto the current chain in forward chronological order upon recursion unwinding.
    fn append_suffix(&mut self, curr_pos: NonZeroU32, anchor_end: usize, curr_base: usize) {
        let curr_idx = curr_pos.get() as usize - 1;
        let em = self.emits[curr_idx];
        let key = self.dedup.keys[curr_idx];
        let end_in_curr = anchor_end - em.gap as usize;
        let start_in_curr = end_in_curr - em.length as usize;

        if let Some(pred) = em.pred {
            self.append_suffix(pred, start_in_curr, curr_base);
        }

        self.cons(
            key.pat_state,
            em.value,
            em.length,
            curr_base + start_in_curr,
            curr_base + end_in_curr,
        );
    }

    /// Finalizes the chain: if no new matches were cons'd, expands the existing head's gap by |c|.
    #[inline(always)]
    fn finish(mut self, c_num_bytes: usize) -> Option<NonZeroU32> {
        if !self.has_new_match {
            if let Some(p_pos) = self.head {
                let p_idx = p_pos.get() as usize - 1;
                let p_key = self.dedup.keys[p_idx];
                let p_emit = self.emits[p_idx];
                let new_gap = p_emit.gap + c_num_bytes as u32;
                let node = Emit::new(p_emit.value, p_emit.length, new_gap, p_emit.pred);
                let k = DedupKey::new(p_key.pat_state, new_gap, p_emit.pred);
                self.head = Some(self.dedup.insert_or_get(k, node, self.emits));
            }
        }
        self.head
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

    #[inline(always)]
    fn hash_code(&self) -> usize {
        let p = self.pred.map_or(0, |x| x.get());
        (self.pat_state.wrapping_mul(0x9e37_79b9)
            ^ self.gap.wrapping_mul(0x85eb_ca6b)
            ^ p.wrapping_mul(0xc2b2_ae35)) as usize
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
                let h = k.hash_code();
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
        let h = key.hash_code();
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
