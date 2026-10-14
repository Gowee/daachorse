use core::cell::Cell;
use core::cmp::Reverse;
use core::hash::Hash;
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
    V: Copy + Eq + core::hash::Hash,
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
    pub(crate) fn build_fails_and_emits_leftmost_longest(&mut self) {
        assert!(
            self.states[usize::from_u32(ROOT_STATE_ID)]
                .output
                .is_empty(),
            "Error: empty pattern is not supported for MatchKind::LeftmostLongestExperimental"
        );
        // TODO: choose an universal set of glossry to describe our implementation consistently, say pending suffix? committed prefix? Better ones?
        let num_states = self.states.len();
        let mut q = Vec::with_capacity(num_states);
        let mut parents = vec![ROOT_STATE_ID; num_states];
        let mut edge_labels: Vec<Option<L>> = vec![None; num_states];

        // Stage 1: BFS Traversal to Establish Topological Order and Parent Links.
        //
        // This traversal computes:
        // 1. `q`: Topological BFS ordering ensuring ancestor states are processed before descendants.
        // 2. `parents` and `edge_labels`: Structural links for parent lookup and DP transitions.
        //
        // Initialize BFS queue starting with ROOT_STATE_ID:
        q.push(ROOT_STATE_ID);
        let mut qi = 0;
        while qi < q.len() {
            let state_id = q[qi];
            qi += 1;

            for &(ch, child_id) in self.states[state_id as usize].edges.iter() {
                parents[child_id as usize] = state_id;
                edge_labels[child_id as usize] = Some(ch);
                q.push(child_id);
            }
        }

        // Construction-time store for deduplicating and indexing emit nodes.
        let mut store = EmitStore::with_capacity(q.len());

        // Reusable scratch buffer for harvesting matches during failure fallback walks.
        let mut scratch_buffer = Vec::new();

        // Stage 2: Resolving Active Candidate Fail Transitions and Committed Match Chains.
        for &state_id in &q[1..] {
            // Fast Path: Root-Covered Pattern (Barrier Reset).
            // If `state_id` itself is a complete pattern from ROOT, under leftmost-longest priority
            // it supersedes any candidate fallback. `fail` is reset to ROOT and `gap` is 0.
            if let Some(&pat) = self.states[state_id as usize].output.first() {
                let emit = Emit::from_output(pat, 0, None);
                self.states[state_id as usize]
                    .output_pos
                    .set(Some(store.insert(emit)));
                self.states[state_id as usize].fail.set(ROOT_STATE_ID);
                continue;
            }

            // Universal Recurrence System:
            // S = P · c inherits P's in-flight head match with gap advanced by |c|.
            let parent_id = parents[state_id as usize];
            let ch = edge_labels[state_id as usize].unwrap();
            let initial_head = self.states[parent_id as usize]
                .output_pos
                .get()
                .map(|pos| store.get(pos).with_incremented_gap(ch.num_bytes()));

            let mut chain = EmitChain::new(initial_head);

            // 1. Failure Path Walk & Candidate Match Harvesting:
            // When an active candidate `curr = fail(parent_id)` cannot transition on `ch`,
            // its pattern pursuit has failed. Any pre-committed non-overlapping matches
            // along `curr`'s prefix are appended onto `chain`.
            let mut curr = self.states[parent_id as usize].fail.get();
            let mut next_child = Self::child_id_from_states(&self.states, curr, ch);
            while curr != ROOT_STATE_ID && next_child.is_none() {
                if let Some(curr_head) = self.states[curr as usize].output_pos.get() {
                    scratch_buffer.clear();
                    scratch_buffer.extend(store.iter_chain(curr_head));
                    scratch_buffer[0] = scratch_buffer[0].with_incremented_gap(ch.num_bytes());
                    chain.extend_from(&scratch_buffer, &mut store);
                }
                curr = self.states[curr as usize].fail.get();
                next_child = Self::child_id_from_states(&self.states, curr, ch);
            }

            // 2. Active Candidate Transition & Leaf Match Decision:
            let mut fail_state = ROOT_STATE_ID;
            if parent_id != ROOT_STATE_ID {
                if let Some(child_id) = next_child {
                    if !self.states[child_id as usize].edges.is_empty() {
                        // Extend active candidate along child.
                        fail_state = child_id;
                    } else {
                        // Suffix completes a leaf pattern at the very end of `state_id`.
                        let pat = self.states[child_id as usize].output[0];
                        chain.push_leaf(pat, &mut store);
                        fail_state = ROOT_STATE_ID;
                    }
                }
            }

            // 3. Finalize match chain:
            let final_pos = chain.finish(&mut store);
            self.states[state_id as usize].output_pos.set(final_pos);
            self.states[state_id as usize].fail.set(fail_state);
        }

        self.emits = store.into_emits();
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

/// An active match chain builder for a state in the leftmost-longest automaton.
///
/// Tracks the in-flight head match whose trailing gap is maintained relative to the current state.
/// Inter-match gaps and candidate suffix links are computed purely via relative delta offsets,
/// with zero absolute coordinates or depth arrays.
#[derive(Clone, Copy, Debug)]
struct EmitChain<V> {
    head: Option<Emit<V>>,
}

impl<V> Default for EmitChain<V> {
    #[inline(always)]
    fn default() -> Self {
        Self { head: None }
    }
}

impl<V: Copy + Eq + Hash> EmitChain<V> {
    /// Initializes an active match chain with an optional in-flight head match.
    #[inline(always)]
    const fn new(head: Option<Emit<V>>) -> Self {
        Self { head }
    }

    /// Appends all matches from a candidate match chain into this chain.
    ///
    /// `emits` contains the matches in reverse chronological order `[M_k, ..., M_1]`,
    /// with `emits[0].gap` already adjusted relative to the current state.
    #[inline(always)]
    fn extend_from(&mut self, emits: &[Emit<V>], store: &mut EmitStore<V>) {
        if emits.is_empty() {
            return;
        }

        // Link earliest match M_1 to the previous head:
        let mut pred = if let Some(prev) = self.head {
            let span: u32 = emits.iter().map(|em| em.gap + em.length).sum();
            debug_assert!(
                prev.gap >= span,
                "candidate match cannot overlap with committed ancestor match"
            );
            let inter_gap = prev.gap - span;
            Some(store.insert(prev.with_gap(inter_gap)))
        } else {
            None
        };

        // Chain intermediate matches M_1 .. M_{k-1}:
        for em in emits[1..].iter().rev() {
            pred = Some(store.insert(Emit::new(em.value, em.length, em.gap, pred)));
        }

        // Install M_k as the new in-flight head:
        self.head = Some(emits[0].with_pred(pred));
    }

    /// Appends a newly completed leaf pattern ending at the current state.
    #[inline(always)]
    fn push_leaf(&mut self, output: (V, u32), store: &mut EmitStore<V>) {
        let pred = if let Some(prev) = self.head {
            debug_assert!(
                prev.gap >= output.1,
                "leaf pattern cannot overlap with committed ancestor match"
            );
            let inter_gap = prev.gap - output.1;
            Some(store.insert(prev.with_gap(inter_gap)))
        } else {
            None
        };
        self.head = Some(Emit::from_output(output, 0, pred));
    }

    /// Finalizes the match chain by committing the in-flight head to `store`.
    #[inline(always)]
    fn finish(self, store: &mut EmitStore<V>) -> Option<NonZeroU32> {
        self.head.map(|h| store.insert(h))
    }
}

/// A construction-time store combining emit node storage with open-addressing deduplication.
///
/// Encapsulates node allocation, hash-consing lookup, and chain traversal.
/// When construction is complete, unwraps into the final [`Vec<Emit<V>>`].
struct EmitStore<V> {
    emits: Vec<Emit<V>>,
    dedup: EmitDedup<V>,
}

impl<V: Copy + Eq + Hash> EmitStore<V> {
    /// Creates an empty store with initial capacity.
    fn with_capacity(cap: usize) -> Self {
        Self {
            emits: Vec::with_capacity(cap),
            dedup: EmitDedup::with_capacity(cap),
        }
    }

    /// Retrieves an [`Emit`] node by its 1-based index.
    #[inline(always)]
    fn get(&self, pos: NonZeroU32) -> Emit<V> {
        self.emits[usize::from_u32(pos.get() - 1)]
    }

    /// Inserts an [`Emit`] node with deduplication, returning its canonical 1-based index.
    #[inline(always)]
    fn insert(&mut self, emit: Emit<V>) -> NonZeroU32 {
        if let Some(pos) = self.dedup.get(&emit) {
            pos
        } else {
            self.emits.push(emit);
            let pos = NonZeroU32::new(u32::try_from(self.emits.len()).unwrap()).unwrap();
            self.dedup.insert(emit, pos);
            pos
        }
    }

    /// Returns an iterator over matches along a chain from head to tail.
    #[inline(always)]
    fn iter_chain(&self, head: NonZeroU32) -> ChainIter<'_, V> {
        ChainIter {
            store: self,
            pos: Some(head),
        }
    }

    /// Unwraps the store to extract the finalized [`Vec<Emit<V>>`].
    #[inline(always)]
    fn into_emits(self) -> Vec<Emit<V>> {
        self.emits
    }
}

/// An iterator traversing an [`Emit`] chain from head to tail.
struct ChainIter<'a, V> {
    store: &'a EmitStore<V>,
    pos: Option<NonZeroU32>,
}

impl<'a, V: Copy + Eq + Hash> Iterator for ChainIter<'a, V> {
    type Item = Emit<V>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.pos?;
        let em = self.store.get(pos);
        self.pos = em.pred;
        Some(em)
    }
}

/// A compact, open-addressing deduplication table for [`Emit`] nodes in `no_std`.
///
/// In leftmost-longest matching, multiple states across divergent branches frequently produce
/// identical [`Emit`] configurations. This deduplication table provides hash-consing directly
/// over the value type `Emit<V>`, unifying identical `(value, length, gap, pred)` tuples into
/// a single canonical index, saving ~35% node allocations.
///
/// Deduplication is cleanly separated from emit list storage: [`EmitDedup`] acts as a lookup
/// cache mapping `Emit<V> -> NonZeroU32`, while [`NfaBuilder::emits`] owns the underlying storage.
///
/// # Where Do Duplicates Arise? (Concrete Examples)
///
/// The vast majority of duplicates arise from **different branches committing the same pattern**:
///
/// 1. **Cross-Path Pattern Convergence (82.5% of all deduplication hits)**:
///    - In an Aho-Corasick automaton, many divergent branches in the Trie share common suffixes or
///      leaf matches.
///    - Suppose Path 1 (e.g., prefix `xyz...`) and Path 2 (e.g., prefix `abc...`) are entirely disjoint.
///    - When both paths encounter the same pattern $M$ at the same relative trailing offset without
///      prior committed matches (`head == None`), both compute the exact same node:
///      `(M.value, M.length, gap, None)`.
///    - Even though Path 1 and Path 2 are completely different automaton states, their match records
///      for pattern $M$ are identical and unified into a single DAG node.
///
/// 2. **Sibling Fanout in `finish()` (~75% of deduplicated node count)**:
///    - Suppose parent state $P$ has an active match chain ending at pattern $M$ with trailing gap $g$.
///    - $P$ has multiple outgoing child edges $P \xrightarrow{c_1} S_1, P \xrightarrow{c_2} S_2, \dots$
///      (e.g., diverging on ASCII characters `'a'`, `'b'`, `'c'`).
///    - If none of these sibling states match a new pattern or harvest a broken suffix, they all
///      advance $M$'s trailing gap by $|c|$ in `finish()`.
///    - Because they share the same parent and same character byte length $|c|$, every sibling
///      computes the identical node: `(M.value, M.length, g + |c|, M.pred)`.
///    - *Example*: For a 54k-word dictionary, this accounts for **39,000+ deduplicated nodes**.
///
/// 3. **Rekeying Previous Head on Chaining (~19% of deduplicated node count)**:
///    - When a new match $M_{new}$ is committed after an earlier match $M_{prev}$, the previous head
///      transitions from an active head (trailing gap) to an internal node (inter-match gap).
///    - Its gap is rekeyed to the inter-match distance ($\text{prev.gap} - \text{span}$ or $\text{prev.gap} - \text{pat.len}$).
///    - If multiple divergent branches commit pairs of patterns separated by the exact same
///      inter-match distance (e.g., adjacent words with gap 0), they generate the identical
///      `(M_{prev}.value, M_{prev}.length, inter_gap, M_{prev}.pred)`.
///    - *Example*: Accounts for **~10,000 deduplicated nodes** in a 54k dictionary.
///
/// # Where Do Duplicates NOT Arise?
///
/// - **Along the same path**: Moving down the Trie strictly increases depth at every step ($|c| \ge 1$),
///   so trailing gap is strictly monotonic ($g \to g+|c| \to g+2|c| \dots$). A single path never duplicates itself.
/// - **Suffix copying with divergent ancestor histories**: When two branches have different prior committed
///   matches ($P_A \neq P_B$), path copying forces the earliest copied match to link to $P_A$ and $P_B$
///   respectively. All downstream matches inherit this divergent `pred`, preventing node reuse.
struct EmitDedup<V> {
    entries: Vec<Option<(Emit<V>, NonZeroU32)>>,
    mask: usize,
    count: usize,
}

impl<V: Copy + Eq + Hash> EmitDedup<V> {
    fn with_capacity(cap: usize) -> Self {
        let size = cap.next_power_of_two().max(1024);
        Self {
            entries: vec![None; size],
            mask: size - 1,
            count: 0,
        }
    }

    #[inline(always)]
    fn get(&self, emit: &Emit<V>) -> Option<NonZeroU32> {
        let h = hash_emit(emit);
        let mut idx = h & self.mask;
        loop {
            match self.entries[idx] {
                None => return None,
                Some((existing, pos)) => {
                    if existing == *emit {
                        return Some(pos);
                    }
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    fn insert(&mut self, emit: Emit<V>, pos: NonZeroU32) {
        if self.count * 2 >= self.entries.len() {
            self.grow();
        }
        let h = hash_emit(&emit);
        let mut idx = h & self.mask;
        loop {
            match self.entries[idx] {
                None => {
                    self.entries[idx] = Some((emit, pos));
                    self.count += 1;
                    return;
                }
                Some((existing, _)) => {
                    if existing == emit {
                        return;
                    }
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    fn grow(&mut self) {
        let new_size = self.entries.len() * 2;
        let new_mask = new_size - 1;
        let mut new_entries = vec![None; new_size];
        for (emit, pos) in self.entries.drain(..).flatten() {
            let h = hash_emit(&emit);
            let mut idx = h & new_mask;
            while new_entries[idx].is_some() {
                idx = (idx + 1) & new_mask;
            }
            new_entries[idx] = Some((emit, pos));
        }
        self.entries = new_entries;
        self.mask = new_mask;
    }
}

#[inline(always)]
fn hash_emit<V: Copy + core::hash::Hash>(emit: &Emit<V>) -> usize {
    struct FxHasher(u64);
    impl core::hash::Hasher for FxHasher {
        #[inline(always)]
        fn finish(&self) -> u64 {
            self.0
        }
        #[inline(always)]
        fn write(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0 = (self.0.rotate_left(5) ^ (b as u64)).wrapping_mul(0x517c_c1b7_2722_0a95);
            }
        }
        #[inline(always)]
        fn write_u32(&mut self, i: u32) {
            self.0 = (self.0.rotate_left(5) ^ (i as u64)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        #[inline(always)]
        fn write_usize(&mut self, i: usize) {
            self.0 = (self.0.rotate_left(5) ^ (i as u64)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    let mut hasher = FxHasher(0);
    emit.value.hash(&mut hasher);
    emit.length.hash(&mut hasher);
    emit.gap.hash(&mut hasher);
    emit.pred.hash(&mut hasher);
    hasher.0 as usize
}
