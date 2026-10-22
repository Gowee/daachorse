// Evaluation: CodeMapper::get_old (slice get + copied + filter)
//         vs. CodeMapper::get_new (single bounds check + direct load)
//         vs. CodeMapper::get_code (raw scalar u32::MAX)
//
// Protocol for 100% noise elimination:
// 1. Separate #[inline(never)] runner functions per arm to eliminate compiler inlining perturbation.
// 2. Hardware performance counters (instructions:u, cycles:u) via perf_event_open.
// 3. 10 interleaved measurement rounds rotating arm order.
// 4. Parity assertion across all arms.

use std::fs::File;
use std::hint::black_box;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

extern "C" {
    fn syscall(number: i64, ...) -> i64;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

use daachorse::{CharwiseDoubleArrayAhoCorasick, CharwiseDoubleArrayAhoCorasickBuilder, MatchKind};

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
                ioctl(fd, 0x2403, 0); // PERF_EVENT_IOC_RESET
                ioctl(fd, 0x2400, 0); // PERF_EVENT_IOC_ENABLE
            }
            let res = f();
            unsafe {
                ioctl(fd, 0x2401, 0); // PERF_EVENT_IOC_DISABLE
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

#[inline(never)]
fn run_micro_new(pma: &CharwiseDoubleArrayAhoCorasick<usize>, chars: &[char]) -> u64 {
    let mut sum = 0u64;
    for &c in chars {
        if let Some(code) = pma.mapper().get_new(c) {
            sum = sum.wrapping_add(code as u64);
        }
    }
    sum
}

#[inline(never)]
fn run_micro_old(pma: &CharwiseDoubleArrayAhoCorasick<usize>, chars: &[char]) -> u64 {
    let mut sum = 0u64;
    for &c in chars {
        if let Some(code) = pma.mapper().get_old(c) {
            sum = sum.wrapping_add(code as u64);
        }
    }
    sum
}

#[inline(never)]
fn run_micro_scalar(pma: &CharwiseDoubleArrayAhoCorasick<usize>, chars: &[char]) -> u64 {
    let mut sum = 0u64;
    for &c in chars {
        let code = pma.mapper().get_code(c);
        if code != u32::MAX {
            sum = sum.wrapping_add(code as u64);
        }
    }
    sum
}

#[inline(never)]
fn run_search_new(pma: &CharwiseDoubleArrayAhoCorasick<usize>, haystack: &str) -> (usize, usize) {
    let mut count = 0;
    let mut checksum = 0;
    for m in pma.leftmost_find_iter_experimental(haystack) {
        count += 1;
        checksum ^= m.start() ^ (m.end() << 16) ^ (m.value() << 32);
    }
    (count, checksum)
}

#[inline(never)]
fn run_search_old(pma: &CharwiseDoubleArrayAhoCorasick<usize>, haystack: &str) -> (usize, usize) {
    let mut count = 0;
    let mut checksum = 0;
    for m in pma.leftmost_find_iter_old_mapper(haystack) {
        count += 1;
        checksum ^= m.start() ^ (m.end() << 16) ^ (m.value() << 32);
    }
    (count, checksum)
}

#[inline(never)]
fn run_search_scalar(pma: &CharwiseDoubleArrayAhoCorasick<usize>, haystack: &str) -> (usize, usize) {
    let mut count = 0;
    let mut checksum = 0;
    for m in pma.leftmost_find_iter_scalar(haystack) {
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

fn find_pattern_file(name: &str) -> Option<String> {
    let candidates = [
        format!("/home/lotus/Workspace/stringmatch-bench/data/{}.txt", name),
        format!("/home/lotus/Workspace/zhconv-rs/data/opencc/STCharacters.txt"),
    ];
    for c in &candidates {
        if Path::new(c).exists() {
            return Some(c.clone());
        }
    }
    None
}

fn find_haystack_file(name: &str) -> Option<String> {
    let candidates = [
        format!("/home/lotus/Workspace/stringmatch-bench/data/{}.txt", name),
        format!("/home/lotus/Workspace/zhconv-rs/data/opencc/STCharacters.txt"),
        format!("/home/lotus/Workspace/zhconv-rs/data/test/sanguo.txt"),
        format!("/home/lotus/Workspace/zhconv-rs/data/test/data3185k.txt"),
    ];
    for c in &candidates {
        if Path::new(c).exists() {
            return Some(c.clone());
        }
    }
    None
}

fn bench_workload(title: &str, pma: &CharwiseDoubleArrayAhoCorasick<usize>, haystack: &str) {
    println!("\n================================================================================");
    println!(">>> Workload: {}", title);
    println!("Haystack length: {} bytes ({} chars)", haystack.len(), haystack.chars().count());

    // 1. Parity check
    let res_new = run_search_new(pma, haystack);
    let res_old = run_search_old(pma, haystack);
    let res_scalar = run_search_scalar(pma, haystack);
    assert_eq!(res_new, res_old, "Mismatch between new and old!");
    assert_eq!(res_new, res_scalar, "Mismatch between new and scalar!");
    println!("Parity verified: {} matches identically found.", res_new.0);

    // 2. Hardware instruction counters
    let mut ins_counter = PerfCounter::new(0, 1); // PERF_COUNT_HW_INSTRUCTIONS
    let (_, ins_new) = ins_counter.measure(|| black_box(run_search_new(pma, haystack)));
    let (_, ins_old) = ins_counter.measure(|| black_box(run_search_old(pma, haystack)));
    let (_, ins_scalar) = ins_counter.measure(|| black_box(run_search_scalar(pma, haystack)));

    // 3. Hardware cycle counters
    let mut cyc_counter = PerfCounter::new(0, 0); // PERF_COUNT_HW_CPU_CYCLES
    let (_, cyc_new) = cyc_counter.measure(|| black_box(run_search_new(pma, haystack)));
    let (_, cyc_old) = cyc_counter.measure(|| black_box(run_search_old(pma, haystack)));
    let (_, cyc_scalar) = cyc_counter.measure(|| black_box(run_search_scalar(pma, haystack)));

    println!("\n[1] Deterministic CPU User Instructions (Zero Noise):");
    println!("  Old Mapper (filter/copied) : {:10} ins  (baseline)", ins_old);
    let delta_new_ins = (ins_new as f64 - ins_old as f64) / ins_old as f64 * 100.0;
    println!("  New Mapper (get_new Option): {:10} ins  ({:+.2}% vs Old)", ins_new, delta_new_ins);
    let delta_scalar_ins = (ins_scalar as f64 - ins_old as f64) / ins_old as f64 * 100.0;
    println!("  Scalar (get_code u32::MAX) : {:10} ins  ({:+.2}% vs Old)", ins_scalar, delta_scalar_ins);

    println!("\n[2] CPU Cycles:");
    println!("  Old Mapper (filter/copied) : {:10} cycles", cyc_old);
    println!("  New Mapper (get_new Option): {:10} cycles", cyc_new);
    println!("  Scalar (get_code u32::MAX) : {:10} cycles", cyc_scalar);

    // 4. Interleaved wall-clock timing
    let mut times_new = Vec::new();
    let mut times_old = Vec::new();
    let mut times_scalar = Vec::new();

    for round in 0..10 {
        sleep(Duration::from_millis(2));
        match round % 3 {
            0 => {
                let t0 = Instant::now();
                black_box(run_search_old(pma, haystack));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t1 = Instant::now();
                black_box(run_search_new(pma, haystack));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t2 = Instant::now();
                black_box(run_search_scalar(pma, haystack));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
            1 => {
                let t1 = Instant::now();
                black_box(run_search_new(pma, haystack));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t2 = Instant::now();
                black_box(run_search_scalar(pma, haystack));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t0 = Instant::now();
                black_box(run_search_old(pma, haystack));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
            _ => {
                let t2 = Instant::now();
                black_box(run_search_scalar(pma, haystack));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t0 = Instant::now();
                black_box(run_search_old(pma, haystack));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t1 = Instant::now();
                black_box(run_search_new(pma, haystack));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
        }
    }

    times_old.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times_new.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times_scalar.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n[3] Wall-Clock Latency (10 rounds interleaved):");
    println!("  Old Mapper (filter/copied) : med={:.3} ms, min={:.3} ms", times_old[5], times_old[0]);
    let sp_new = times_old[5] / times_new[5];
    println!("  New Mapper (get_new Option): med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", times_new[5], times_new[0], sp_new);
    let sp_scalar = times_old[5] / times_scalar[5];
    println!("  Scalar (get_code u32::MAX) : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", times_scalar[5], times_scalar[0], sp_scalar);
    println!("--------------------------------------------------------------------------------");
}

fn bench_direct_micro(pma: &CharwiseDoubleArrayAhoCorasick<usize>, text: &str) {
    println!("\n================================================================================");
    println!(">>> Direct Microbenchmark: 10,000,000 queries directly on CodeMapper::get");
    
    // Prepare 10M query characters by tiling text characters + unmapped characters
    let mut query_chars: Vec<char> = text.chars().collect();
    if query_chars.is_empty() {
        query_chars = vec!['a', 'b', 'c', '中', '文', '日', '本', '語', '\u{10000}', '\u{10FFFF}', ' ', '\n', '1', '2'];
    }
    while query_chars.len() < 10_000_000 {
        let len = query_chars.len();
        query_chars.extend_from_within(..len.min(10_000_000 - len));
    }
    query_chars.truncate(10_000_000);
    println!("Query slice length: {} chars", query_chars.len());

    let sum_new = run_micro_new(pma, &query_chars);
    let sum_old = run_micro_old(pma, &query_chars);
    let sum_scalar = run_micro_scalar(pma, &query_chars);
    assert_eq!(sum_new, sum_old);
    assert_eq!(sum_new, sum_scalar);

    let mut ins_counter = PerfCounter::new(0, 1);
    let (_, ins_new) = ins_counter.measure(|| black_box(run_micro_new(pma, &query_chars)));
    let (_, ins_old) = ins_counter.measure(|| black_box(run_micro_old(pma, &query_chars)));
    let (_, ins_scalar) = ins_counter.measure(|| black_box(run_micro_scalar(pma, &query_chars)));

    let mut cyc_counter = PerfCounter::new(0, 0);
    let (_, cyc_new) = cyc_counter.measure(|| black_box(run_micro_new(pma, &query_chars)));
    let (_, cyc_old) = cyc_counter.measure(|| black_box(run_micro_old(pma, &query_chars)));
    let (_, cyc_scalar) = cyc_counter.measure(|| black_box(run_micro_scalar(pma, &query_chars)));

    println!("\n[1] Deterministic CPU User Instructions (10M Direct Queries):");
    println!("  Old Mapper (filter/copied) : {:10} ins  (baseline)", ins_old);
    let d_new = (ins_new as f64 - ins_old as f64) / ins_old as f64 * 100.0;
    println!("  New Mapper (get_new Option): {:10} ins  ({:+.2}% vs Old)", ins_new, d_new);
    let d_scalar = (ins_scalar as f64 - ins_old as f64) / ins_old as f64 * 100.0;
    println!("  Scalar (get_code u32::MAX) : {:10} ins  ({:+.2}% vs Old)", ins_scalar, d_scalar);

    println!("\n[2] CPU Cycles (10M Direct Queries):");
    println!("  Old Mapper (filter/copied) : {:10} cycles", cyc_old);
    println!("  New Mapper (get_new Option): {:10} cycles", cyc_new);
    println!("  Scalar (get_code u32::MAX) : {:10} cycles", cyc_scalar);

    let mut times_new = Vec::new();
    let mut times_old = Vec::new();
    let mut times_scalar = Vec::new();

    for round in 0..10 {
        sleep(Duration::from_millis(5));
        match round % 3 {
            0 => {
                let t0 = Instant::now();
                black_box(run_micro_old(pma, &query_chars));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t1 = Instant::now();
                black_box(run_micro_new(pma, &query_chars));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t2 = Instant::now();
                black_box(run_micro_scalar(pma, &query_chars));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
            1 => {
                let t1 = Instant::now();
                black_box(run_micro_new(pma, &query_chars));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t2 = Instant::now();
                black_box(run_micro_scalar(pma, &query_chars));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t0 = Instant::now();
                black_box(run_micro_old(pma, &query_chars));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
            _ => {
                let t2 = Instant::now();
                black_box(run_micro_scalar(pma, &query_chars));
                times_scalar.push(t2.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t0 = Instant::now();
                black_box(run_micro_old(pma, &query_chars));
                times_old.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);

                let t1 = Instant::now();
                black_box(run_micro_new(pma, &query_chars));
                times_new.push(t1.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
        }
    }
    times_old.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times_new.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times_scalar.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n[3] Wall-Clock Latency (10 rounds interleaved):");
    println!("  Old Mapper (filter/copied) : med={:.3} ms, min={:.3} ms", times_old[5], times_old[0]);
    let sp_new = times_old[5] / times_new[5];
    println!("  New Mapper (get_new Option): med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", times_new[5], times_new[0], sp_new);
    let sp_scalar = times_old[5] / times_scalar[5];
    println!("  Scalar (get_code u32::MAX) : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", times_scalar[5], times_scalar[0], sp_scalar);
    println!("--------------------------------------------------------------------------------");
}

fn find_file(candidates: &[&str]) -> Option<String> {
    for c in candidates {
        if Path::new(c).exists() {
            return Some(c.to_string());
        }
    }
    None
}

fn main() {
    println!("Evaluating CodeMapper: Old vs New vs Scalar");

    // Workload 1: English words_100000
    let en_pat = find_file(&[
        "/home/lotus/Workspace/stringmatch-bench/data/words_100000.txt",
    ]);
    let en_hay = find_file(&[
        "/home/lotus/Workspace/stringmatch-bench/data/sherlock.txt",
    ]);
    if let (Some(pat_file), Some(hay_file)) = (en_pat, en_hay) {
        let patterns = load_patterns(&pat_file).unwrap();
        let haystack = load_haystack(&hay_file).unwrap();
        println!("Building English words_100000 PMA...");
        let pma = CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build_with_values(patterns.iter().enumerate().map(|(i, s)| (s, i)))
            .unwrap();
        bench_workload("English words_100000", &pma, &haystack);
        bench_direct_micro(&pma, &haystack);
    } else {
        println!("Skipping words_100000 (data not found)");
    }

    // Workload 2: Japanese UniDic
    let jp_pat = find_file(&[
        "/home/lotus/Workspace/stringmatch-bench/data/unidic/unidic",
    ]);
    let jp_hay = find_file(&[
        "/home/lotus/Workspace/stringmatch-bench/data/wagahaiwa_nekodearu.txt",
    ]);
    if let (Some(pat_file), Some(hay_file)) = (jp_pat, jp_hay) {
        let patterns = load_patterns(&pat_file).unwrap();
        let haystack = load_haystack(&hay_file).unwrap();
        println!("Building UniDic PMA...");
        let pma = CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build_with_values(patterns.iter().enumerate().map(|(i, s)| (s, i)))
            .unwrap();
        bench_workload("Japanese UniDic", &pma, &haystack);
    } else {
        println!("Skipping UniDic (data not found)");
    }

    // Workload 3: Chinese OpenCC 3185k
    let zh_pat_files = [
        "/home/lotus/Workspace/zhconv-rs/data/opencc/STCharacters.txt",
        "/home/lotus/Workspace/zhconv-rs/data/opencc/STPhrases.txt",
    ];
    let zh_hay = find_file(&[
        "/home/lotus/Workspace/zhconv-rs/benches/data3185k.txt",
    ]);
    if let Some(hay_file) = zh_hay {
        let mut patterns = Vec::new();
        for pf in &zh_pat_files {
            if let Ok(file) = File::open(pf) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Some(first) = line.split('\t').next() {
                        patterns.push(first.to_string());
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
        bench_workload("Chinese OpenCC 3185k", &pma, &haystack);
    } else {
        println!("Skipping Chinese OpenCC (data not found)");
    }
}
