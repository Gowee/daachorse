// Comprehensive evaluation: Interleaved (AOS, 16B) vs Split-Array (SOA, 8B + 8B)
//
// Protocol for 100% noise elimination:
// 1. Separate #[inline(never)] runner functions per arm.
// 2. Hardware performance counters (instructions:u, cycles:u, L1-dcache-misses) via perf_event_open.
// 3. 10 interleaved measurement rounds rotating arm order.
// 4. Parity assertion across all arms.

use std::fs::File;
use std::hint::black_box;
use std::io::{BufRead, BufReader, Read};
use std::num::NonZeroU32;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

extern "C" {
    fn syscall(number: i64, ...) -> i64;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

use daachorse::charwise::CharwiseDoubleArrayAhoCorasick;
use daachorse::{CharwiseDoubleArrayAhoCorasickBuilder, Match, MatchKind};

const ROOT_STATE_IDX: u32 = 0;

#[repr(C)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    bp_addr: u64,
    bp_len: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved: u16,
}

struct PerfCounter {
    file: Option<File>,
}

impl PerfCounter {
    fn new(type_: u32, config: u64) -> Self {
        use std::os::unix::io::FromRawFd;
        let mut attr: PerfEventAttr = unsafe { std::mem::zeroed() };
        attr.type_ = type_;
        attr.size = std::mem::size_of::<PerfEventAttr>() as u32;
        attr.config = config;
        attr.flags = (1 << 0) | (1 << 5); // disabled=1, exclude_kernel=1

        let fd = unsafe {
            syscall(
                298, // __NR_perf_event_open
                &attr as *const PerfEventAttr,
                0,  // pid = 0 (calling process)
                -1, // cpu = -1
                -1, // group_fd = -1
                0,  // flags = 0
            )
        };
        if fd >= 0 {
            PerfCounter {
                file: Some(unsafe { File::from_raw_fd(fd as i32) }),
            }
        } else {
            PerfCounter { file: None }
        }
    }

    fn measure<F, R>(&mut self, mut f: F) -> (R, u64)
    where
        F: FnMut() -> R,
    {
        if let Some(ref mut file) = self.file {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            unsafe {
                ioctl(fd, 0x2403, 0); // RESET
                ioctl(fd, 0x2400, 0); // ENABLE
            }
            let res = f();
            unsafe {
                ioctl(fd, 0x2401, 0); // DISABLE
            }
            let mut buf = [0u8; 8];
            if file.read_exact(&mut buf).is_ok() {
                let count = u64::from_ne_bytes(buf);
                (res, count)
            } else {
                (res, 0)
            }
        } else {
            (f(), 0)
        }
    }
}

// ----------------------------------------------------------------------------
// Representation 2: SOA (Split Array: 8B Trans + 8B FailOutput)
// ----------------------------------------------------------------------------
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct TransState {
    base: Option<NonZeroU32>,
    check: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct FailOutputState {
    fail: u32,
    output_pos: Option<NonZeroU32>,
}

impl FailOutputState {
    #[inline(always)]
    fn fail(&self) -> u32 {
        self.fail & 0x7FFF_FFFF
    }

    #[inline(always)]
    fn fail_has_emit(&self) -> bool {
        (self.fail & 0x8000_0000) != 0
    }
}

struct SoaAutomaton<V> {
    pma: CharwiseDoubleArrayAhoCorasick<V>,
    trans: Vec<TransState>,
    fail_output: Vec<FailOutputState>,
}

impl<V: Copy> SoaAutomaton<V> {
    fn new(pma: CharwiseDoubleArrayAhoCorasick<V>) -> Self {
        let pma_states = pma.states();
        let mut trans = Vec::with_capacity(pma_states.len());
        let mut fail_output = Vec::with_capacity(pma_states.len());

        for s in pma_states {
            trans.push(TransState {
                base: s.base(),
                check: s.check(),
            });
            let raw_fail = s.fail() | if s.fail_has_emit() { 0x8000_0000 } else { 0 };
            fail_output.push(FailOutputState {
                fail: raw_fail,
                output_pos: s.output_pos(),
            });
        }

        Self {
            pma,
            trans,
            fail_output,
        }
    }

    fn leftmost_find_iter<'a>(&'a self, haystack: &'a str) -> SoaIter<'a, V> {
        SoaIter {
            soa: self,
            haystack,
            pos: 0,
            state_id: ROOT_STATE_IDX,
            buf: Vec::new(),
        }
    }
}

#[inline(always)]
unsafe fn step_child_soa(
    trans: &[TransState],
    base: Option<NonZeroU32>,
    state_id: u32,
    code: u32,
) -> Option<u32> {
    let base = base?.get();
    let child_id = base ^ code;
    if let Some(child_state) = trans.get(child_id as usize) {
        if child_state.check == state_id {
            return Some(child_id);
        }
    }
    None
}

#[inline(always)]
unsafe fn decode_char_unchecked(bytes: &[u8], pos: usize) -> (char, usize) {
    let first = *bytes.get_unchecked(pos);
    let (c, len) = if first < 0x80 {
        (u32::from(first), 1)
    } else {
        let c = u32::from(*bytes.get_unchecked(pos + 1) & 0x3f);
        if first < 0xe0 {
            ((u32::from(first & 0x1f) << 6) | c, 2)
        } else {
            let c = (c << 6) | u32::from(*bytes.get_unchecked(pos + 2) & 0x3f);
            if first < 0xf0 {
                ((u32::from(first & 0x0f) << 12) | c, 3)
            } else {
                let c = (c << 6) | u32::from(*bytes.get_unchecked(pos + 3) & 0x3f);
                ((u32::from(first & 0x07) << 18) | c, 4)
            }
        }
    };
    (char::from_u32_unchecked(c), len)
}

struct SoaIter<'a, V> {
    soa: &'a SoaAutomaton<V>,
    haystack: &'a str,
    pos: usize,
    state_id: u32,
    buf: Vec<Match<V>>,
}

impl<'a, V: Copy> SoaIter<'a, V> {
    #[inline(always)]
    fn ensure_buffered(&mut self) {
        if !self.buf.is_empty() {
            return;
        }

        let bytes = self.haystack.as_bytes();
        let mut pos = self.pos;
        let mut state_id = self.state_id;
        let trans = &self.soa.trans;
        let fail_out = &self.soa.fail_output;

        while pos < bytes.len() {
            let (c, char_len) = unsafe { decode_char_unchecked(bytes, pos) };

            if let Some(code) = self.soa.pma.mapper().get(c) {
                loop {
                    let st = unsafe { trans.get_unchecked(state_id as usize) };
                    if let Some(child_id) = unsafe { step_child_soa(trans, st.base, state_id, code) } {
                        state_id = child_id;
                        break;
                    }
                    if state_id == ROOT_STATE_IDX {
                        break;
                    }
                    let fo = unsafe { fail_out.get_unchecked(state_id as usize) };
                    state_id = fo.fail();
                    if let Some(ep) = fo.output_pos {
                        self.buf.extend(unsafe { self.soa.pma.iter_emits(ep, pos) });

                        // SingleProbe
                        let fail_st = unsafe { trans.get_unchecked(state_id as usize) };
                        if let Some(child_id) = unsafe { step_child_soa(trans, fail_st.base, state_id, code) } {
                            self.state_id = child_id;
                            self.pos = pos + char_len;
                        } else if state_id == ROOT_STATE_IDX {
                            self.state_id = ROOT_STATE_IDX;
                            self.pos = pos + char_len;
                        } else {
                            self.state_id = state_id;
                            self.pos = pos;
                        }
                        return;
                    }
                }
            } else {
                while state_id != ROOT_STATE_IDX {
                    let fo = unsafe { fail_out.get_unchecked(state_id as usize) };
                    if !fo.fail_has_emit() {
                        state_id = ROOT_STATE_IDX;
                        break;
                    }
                    state_id = fo.fail();
                    if let Some(ep) = fo.output_pos {
                        self.buf.extend(unsafe { self.soa.pma.iter_emits(ep, pos) });
                        if state_id == ROOT_STATE_IDX {
                            self.pos = pos + char_len;
                        } else {
                            self.pos = pos;
                        }
                        self.state_id = state_id;
                        return;
                    }
                }
            }

            pos += char_len;
        }

        self.pos = pos;
        self.drain_eof(pos, state_id);
    }

    #[inline(always)]
    fn drain_eof(&mut self, pos: usize, mut state_id: u32) {
        let fail_out = &self.soa.fail_output;
        while state_id != ROOT_STATE_IDX {
            let fo = unsafe { fail_out.get_unchecked(state_id as usize) };
            if !fo.fail_has_emit() {
                state_id = ROOT_STATE_IDX;
                break;
            }
            state_id = fo.fail();
            if let Some(ep) = fo.output_pos {
                self.buf.extend(unsafe { self.soa.pma.iter_emits(ep, pos) });
                self.pos = pos;
                self.state_id = state_id;
                return;
            }
        }
        self.state_id = state_id;
    }
}

impl<'a, V: Copy> Iterator for SoaIter<'a, V> {
    type Item = Match<V>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.ensure_buffered();
        self.buf.pop()
    }
}

#[inline(never)]
fn run_search_aos(pma: &CharwiseDoubleArrayAhoCorasick<usize>, haystack: &str) -> (usize, usize) {
    let mut count = 0;
    let mut checksum = 0;
    for m in pma.leftmost_find_iter_experimental(haystack) {
        count += 1;
        checksum ^= m.start() ^ (m.end() << 16) ^ (m.value() << 32);
    }
    (count, checksum)
}

#[inline(never)]
fn run_search_soa(soa: &SoaAutomaton<usize>, haystack: &str) -> (usize, usize) {
    let mut count = 0;
    let mut checksum = 0;
    for m in soa.leftmost_find_iter(haystack) {
        count += 1;
        checksum ^= m.start() ^ (m.end() << 16) ^ (m.value() << 32);
    }
    (count, checksum)
}

fn load_patterns(path: &str) -> Option<Vec<String>> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    Some(reader.lines().map_while(Result::ok).collect())
}

fn load_haystack(path: &str) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Some(text)
}

fn bench_workload(title: &str, soa: &SoaAutomaton<usize>, haystack: &str) {
    println!("\n================================================================================");
    println!(">>> Workload: {}", title);
    println!("Haystack length: {} bytes ({} chars)", haystack.len(), haystack.chars().count());

    let res_aos = run_search_aos(&soa.pma, haystack);
    let res_soa = run_search_soa(soa, haystack);
    assert_eq!(res_aos, res_soa, "PARITY MISMATCH between AOS and SOA!");
    println!("Parity verified: {} matches identically found.", res_aos.0);

    // 1. Instructions
    let mut ins_counter = PerfCounter::new(0, 1); // instructions:u
    let (_, ins_aos) = ins_counter.measure(|| black_box(run_search_aos(&soa.pma, haystack)));
    let (_, ins_soa) = ins_counter.measure(|| black_box(run_search_soa(soa, haystack)));

    // 2. Cycles
    let mut cyc_counter = PerfCounter::new(0, 0); // cycles:u
    let (_, cyc_aos) = cyc_counter.measure(|| black_box(run_search_aos(&soa.pma, haystack)));
    let (_, cyc_soa) = cyc_counter.measure(|| black_box(run_search_soa(soa, haystack)));

    // 3. L1 Data Cache Misses (PERF_TYPE_HW_CACHE = 3)
    // config = L1D | READ | MISS = (0) | (0 << 8) | (1 << 16) = 0x10000
    let mut l1_counter = PerfCounter::new(3, 0x10000);
    let (_, l1_aos) = l1_counter.measure(|| black_box(run_search_aos(&soa.pma, haystack)));
    let (_, l1_soa) = l1_counter.measure(|| black_box(run_search_soa(soa, haystack)));

    println!("\n[1] Deterministic CPU User Instructions (Zero Noise):");
    println!("  AOS (Interleaved 16B) : {:10} ins  (baseline)", ins_aos);
    let d_ins = (ins_soa as f64 - ins_aos as f64) / ins_aos as f64 * 100.0;
    println!("  SOA (Split 8B + 8B)   : {:10} ins  ({:+.2}% vs AOS)", ins_soa, d_ins);

    println!("\n[2] CPU Cycles:");
    println!("  AOS (Interleaved 16B) : {:10} cycles", cyc_aos);
    let d_cyc = (cyc_soa as f64 - cyc_aos as f64) / cyc_aos as f64 * 100.0;
    println!("  SOA (Split 8B + 8B)   : {:10} cycles ({:+.2}% vs AOS)", cyc_soa, d_cyc);

    println!("\n[3] L1 Data Cache Misses:");
    println!("  AOS (Interleaved 16B) : {:10} misses", l1_aos);
    let d_l1 = (l1_soa as f64 - l1_aos as f64) / l1_aos as f64 * 100.0;
    println!("  SOA (Split 8B + 8B)   : {:10} misses ({:+.2}% vs AOS)", l1_soa, d_l1);

    // 4. Interleaved wall-clock timing
    let mut times_aos = Vec::new();
    let mut times_soa = Vec::new();

    for round in 0..10 {
        sleep(Duration::from_millis(3));
        if round % 2 == 0 {
            let t0 = Instant::now();
            black_box(run_search_aos(&soa.pma, haystack));
            times_aos.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);

            let t1 = Instant::now();
            black_box(run_search_soa(soa, haystack));
            times_soa.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);
        } else {
            let t1 = Instant::now();
            black_box(run_search_soa(soa, haystack));
            times_soa.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);

            let t0 = Instant::now();
            black_box(run_search_aos(&soa.pma, haystack));
            times_aos.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);
        }
    }
    times_aos.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times_soa.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n[4] Wall-Clock Latency (10 rounds interleaved):");
    println!("  AOS (Interleaved 16B) : med={:.3} ms, min={:.3} ms", times_aos[5], times_aos[0]);
    let sp_soa = times_aos[5] / times_soa[5];
    println!("  SOA (Split 8B + 8B)   : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", times_soa[5], times_soa[0], sp_soa);
    println!("--------------------------------------------------------------------------------");
}

fn main() {
    println!("Evaluating Structure-of-Arrays (SOA) vs Array-of-Structures (AOS)");

    let data_dir = std::env::var("BENCH_DATA_DIR").unwrap_or_else(|_| "/home/lotus/Workspace".to_string());

    // Workload 1: English words_100000
    let pat_file = format!("{}/stringmatch-bench/data/words_100000.txt", data_dir);
    let hay_file = format!("{}/stringmatch-bench/data/sherlock.txt", data_dir);
    if Path::new(&pat_file).exists() && Path::new(&hay_file).exists() {
        let patterns = load_patterns(&pat_file).unwrap();
        let haystack = load_haystack(&hay_file).unwrap();
        println!("Building English words_100000 PMA...");
        let pma = CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build_with_values(patterns.iter().enumerate().map(|(i, s)| (s, i)))
            .unwrap();
        let soa = SoaAutomaton::new(pma);
        bench_workload("English words_100000", &soa, &haystack);
    }

    // Workload 2: Japanese UniDic
    let pat_file = format!("{}/stringmatch-bench/data/unidic/unidic", data_dir);
    let hay_file = format!("{}/stringmatch-bench/data/wagahaiwa_nekodearu.txt", data_dir);
    if Path::new(&pat_file).exists() && Path::new(&hay_file).exists() {
        let patterns = load_patterns(&pat_file).unwrap();
        let haystack = load_haystack(&hay_file).unwrap();
        println!("Building UniDic PMA...");
        let pma = CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build_with_values(patterns.iter().enumerate().map(|(i, s)| (s, i)))
            .unwrap();
        let soa = SoaAutomaton::new(pma);
        bench_workload("Japanese UniDic", &soa, &haystack);
    }

    // Workload 3: Chinese OpenCC 3185k
    let pat_files = [
        format!("{}/zhconv-rs/data/opencc/STCharacters.txt", data_dir),
        format!("{}/zhconv-rs/data/opencc/STPhrases.txt", data_dir),
    ];
    let hay_file = format!("{}/zhconv-rs/benches/data3185k.txt", data_dir);
    if Path::new(&hay_file).exists() {
        let mut patterns = Vec::new();
        for pf in &pat_files {
            if let Ok(file) = File::open(pf) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Some(first) = line.split('\t').next() {
                        let trimmed = first.trim();
                        if !trimmed.is_empty() {
                            patterns.push(trimmed.to_string());
                        }
                    }
                }
            }
        }
        let haystack = load_haystack(&hay_file).unwrap();
        println!("Building Chinese OpenCC PMA (patterns: {})...", patterns.len());
        let pma = CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build_with_values(patterns.iter().enumerate().map(|(i, s)| (s, i)))
            .unwrap();
        let soa = SoaAutomaton::new(pma);
        bench_workload("Chinese OpenCC 3185k", &soa, &haystack);
    }
}
