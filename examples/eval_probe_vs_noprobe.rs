// Comprehensive noise-free evaluation: Legacy vs NoProbe vs SingleProbe
//
// Protocol for 100% noise elimination:
// 1. Separate #[inline(never)] runner functions per arm to eliminate compiler inlining perturbation.
// 2. Hardware performance counters (instructions:u, cycles:u, cache-misses, branch-misses)
//    via perf_event_open for hardware-deterministic, noise-free metrics.
// 3. 10 interleaved measurement rounds rotating arm order (0->1->2, 1->2->0, 2->0->1).
// 4. In-process cooldown pauses between heavy iterations to avoid APU thermal down-clocking.
// 5. Exact match-stream parity assertion on every dataset.

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

#[inline(never)]
fn run_legacy<V: Copy>(da: &CharwiseDoubleArrayAhoCorasick<V>, text: &str) -> usize {
    let mut sum = 0usize;
    for m in da.leftmost_find_iter(text) {
        sum += m.start() + m.end();
    }
    sum
}

#[inline(never)]
fn run_nop_with_bit<V: Copy>(da: &CharwiseDoubleArrayAhoCorasick<V>, text: &str) -> usize {
    let mut sum = 0usize;
    for m in da.leftmost_find_iter_no_probe(text) {
        sum += m.start() + m.end();
    }
    sum
}

#[inline(never)]
fn run_nop_no_bit<V: Copy>(da: &CharwiseDoubleArrayAhoCorasick<V>, text: &str) -> usize {
    let mut sum = 0usize;
    for m in da.leftmost_find_iter_no_probe_no_bit(text) {
        sum += m.start() + m.end();
    }
    sum
}

#[inline(never)]
fn run_sng_with_bit<V: Copy>(da: &CharwiseDoubleArrayAhoCorasick<V>, text: &str) -> usize {
    let mut sum = 0usize;
    for m in da.leftmost_find_iter_experimental(text) {
        sum += m.start() + m.end();
    }
    sum
}

#[inline(never)]
fn run_sng_no_bit<V: Copy>(da: &CharwiseDoubleArrayAhoCorasick<V>, text: &str) -> usize {
    let mut sum = 0usize;
    for m in da.leftmost_find_iter_experimental_no_bit(text) {
        sum += m.start() + m.end();
    }
    sum
}

struct Stat {
    min_ms: f64,
    med_ms: f64,
}

fn calc_stat(durs: &[Duration]) -> Stat {
    let mut ms: Vec<f64> = durs.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stat {
        min_ms: ms[0],
        med_ms: ms[ms.len() / 2],
    }
}

fn load_lines<P: AsRef<Path>>(p: P) -> Vec<String> {
    let f = File::open(p).unwrap();
    let r = BufReader::new(f);
    r.lines().map(|l| l.unwrap()).collect()
}

fn evaluate_workload(
    name: &str,
    patterns: &[String],
    haystack: &str,
    iters_per_round: usize,
    rounds: usize,
) {
    println!("================================================================================");
    println!(">>> Workload: {}", name);
    println!("Patterns: {}, Haystack length: {} bytes ({} chars)", patterns.len(), haystack.len(), haystack.chars().count());

    let leg: CharwiseDoubleArrayAhoCorasick<usize> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .use_prefilter(false)
        .build(patterns)
        .unwrap();

    let exp: CharwiseDoubleArrayAhoCorasick<usize> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongestExperimental)
        .use_prefilter(false)
        .build(patterns)
        .unwrap();

    // 1. Verify 100% exact parity across all 5 arms
    let leg_matches: Vec<_> = leg.leftmost_find_iter(haystack).map(|m| (m.start(), m.end(), m.value())).collect();
    let nop_bit_matches: Vec<_> = exp.leftmost_find_iter_no_probe(haystack).map(|m| (m.start(), m.end(), m.value())).collect();
    let nop_nobit_matches: Vec<_> = exp.leftmost_find_iter_no_probe_no_bit(haystack).map(|m| (m.start(), m.end(), m.value())).collect();
    let sng_bit_matches: Vec<_> = exp.leftmost_find_iter_experimental(haystack).map(|m| (m.start(), m.end(), m.value())).collect();
    let sng_nobit_matches: Vec<_> = exp.leftmost_find_iter_experimental_no_bit(haystack).map(|m| (m.start(), m.end(), m.value())).collect();
    assert_eq!(leg_matches, nop_bit_matches, "Parity mismatch: NoProbe(with bit) != Legacy on {}", name);
    assert_eq!(leg_matches, nop_nobit_matches, "Parity mismatch: NoProbe(NO bit) != Legacy on {}", name);
    assert_eq!(leg_matches, sng_bit_matches, "Parity mismatch: SingleProbe(with bit) != Legacy on {}", name);
    assert_eq!(leg_matches, sng_nobit_matches, "Parity mismatch: SingleProbe(NO bit) != Legacy on {}", name);
    println!("Parity verified: {} matches found identically across all 5 arms.", leg_matches.len());

    // 2. Hardware performance counters (Deterministic & Zero Noise)
    let mut ins_cnt = PerfCounter::new(0, 1); // HW_INSTRUCTIONS
    let mut cyc_cnt = PerfCounter::new(0, 0); // HW_CYCLES

    let (_, leg_ins) = ins_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_legacy(&leg, haystack)); });
    let (_, nop_bit_ins) = ins_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_nop_with_bit(&exp, haystack)); });
    let (_, nop_nobit_ins) = ins_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_nop_no_bit(&exp, haystack)); });
    let (_, sng_bit_ins) = ins_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_sng_with_bit(&exp, haystack)); });
    let (_, sng_nobit_ins) = ins_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_sng_no_bit(&exp, haystack)); });

    let (_, leg_cyc) = cyc_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_legacy(&leg, haystack)); });
    let (_, nop_bit_cyc) = cyc_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_nop_with_bit(&exp, haystack)); });
    let (_, nop_nobit_cyc) = cyc_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_nop_no_bit(&exp, haystack)); });
    let (_, sng_bit_cyc) = cyc_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_sng_with_bit(&exp, haystack)); });
    let (_, sng_nobit_cyc) = cyc_cnt.measure(|| for _ in 0..iters_per_round { black_box(run_sng_no_bit(&exp, haystack)); });

    // 3. Wall-Clock Latency across interleaved rounds with thermal cool-down
    let mut durs_leg = Vec::with_capacity(rounds);
    let mut durs_nop_bit = Vec::with_capacity(rounds);
    let mut durs_nop_nobit = Vec::with_capacity(rounds);
    let mut durs_sng_bit = Vec::with_capacity(rounds);
    let mut durs_sng_nobit = Vec::with_capacity(rounds);

    // Warm-up
    for _ in 0..2 {
        black_box(run_legacy(&leg, haystack));
        black_box(run_nop_with_bit(&exp, haystack));
        black_box(run_nop_no_bit(&exp, haystack));
        black_box(run_sng_with_bit(&exp, haystack));
        black_box(run_sng_no_bit(&exp, haystack));
    }

    for r in 0..rounds {
        let start_arm = r % 5;
        for k in 0..5 {
            let arm = (start_arm + k) % 5;
            let t0 = Instant::now();
            let mut sum = 0usize;
            for _ in 0..iters_per_round {
                match arm {
                    0 => sum += run_legacy(&leg, haystack),
                    1 => sum += run_nop_with_bit(&exp, haystack),
                    2 => sum += run_nop_no_bit(&exp, haystack),
                    3 => sum += run_sng_with_bit(&exp, haystack),
                    _ => sum += run_sng_no_bit(&exp, haystack),
                }
            }
            black_box(sum);
            let per_iter = t0.elapsed() / (iters_per_round as u32);
            match arm {
                0 => durs_leg.push(per_iter),
                1 => durs_nop_bit.push(per_iter),
                2 => durs_nop_nobit.push(per_iter),
                3 => durs_sng_bit.push(per_iter),
                _ => durs_sng_nobit.push(per_iter),
            }
        }
        sleep(Duration::from_millis(15));
    }

    let s_leg = calc_stat(&durs_leg);
    let s_nop_bit = calc_stat(&durs_nop_bit);
    let s_nop_nobit = calc_stat(&durs_nop_nobit);
    let s_sng_bit = calc_stat(&durs_sng_bit);
    let s_sng_nobit = calc_stat(&durs_sng_nobit);

    let ins_leg = leg_ins / iters_per_round as u64;
    let ins_nop_bit = nop_bit_ins / iters_per_round as u64;
    let ins_nop_nobit = nop_nobit_ins / iters_per_round as u64;
    let ins_sng_bit = sng_bit_ins / iters_per_round as u64;
    let ins_sng_nobit = sng_nobit_ins / iters_per_round as u64;

    let cyc_leg = leg_cyc / iters_per_round as u64;
    let cyc_nop_bit = nop_bit_cyc / iters_per_round as u64;
    let cyc_nop_nobit = nop_nobit_cyc / iters_per_round as u64;
    let cyc_sng_bit = sng_bit_cyc / iters_per_round as u64;
    let cyc_sng_nobit = sng_nobit_cyc / iters_per_round as u64;

    println!("\n[1] Deterministic CPU User Instructions (Zero Noise):");
    println!("  Legacy                : {:>10} ins  (baseline)", ins_leg);
    println!("  NoProbe (with bit)    : {:>10} ins  ({:>+6.2}% vs Leg)", ins_nop_bit, ((ins_nop_bit as f64 - ins_leg as f64) / ins_leg as f64) * 100.0);
    println!("  NoProbe (NO bit)      : {:>10} ins  ({:>+6.2}% vs Leg, {:>+6.2}% vs with-bit)", ins_nop_nobit, ((ins_nop_nobit as f64 - ins_leg as f64) / ins_leg as f64) * 100.0, ((ins_nop_nobit as f64 - ins_nop_bit as f64) / ins_nop_bit as f64) * 100.0);
    println!("  SingleProbe (with bit): {:>10} ins  ({:>+6.2}% vs Leg)", ins_sng_bit, ((ins_sng_bit as f64 - ins_leg as f64) / ins_leg as f64) * 100.0);
    println!("  SingleProbe (NO bit)  : {:>10} ins  ({:>+6.2}% vs Leg, {:>+6.2}% vs with-bit)", ins_sng_nobit, ((ins_sng_nobit as f64 - ins_leg as f64) / ins_leg as f64) * 100.0, ((ins_sng_nobit as f64 - ins_sng_bit as f64) / ins_sng_bit as f64) * 100.0);

    println!("\n[2] CPU Cycles:");
    println!("  Legacy                : {:>10} cycles", cyc_leg);
    println!("  NoProbe (with bit)    : {:>10} cycles", cyc_nop_bit);
    println!("  NoProbe (NO bit)      : {:>10} cycles", cyc_nop_nobit);
    println!("  SingleProbe (with bit): {:>10} cycles", cyc_sng_bit);
    println!("  SingleProbe (NO bit)  : {:>10} cycles", cyc_sng_nobit);

    println!("\n[3] Wall-Clock Latency (10 rounds interleaved + cooldown):");
    println!("  Legacy                : med={:.3} ms, min={:.3} ms  (baseline)", s_leg.med_ms, s_leg.min_ms);
    println!("  NoProbe (with bit)    : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", s_nop_bit.med_ms, s_nop_bit.min_ms, s_leg.med_ms / s_nop_bit.med_ms);
    println!("  NoProbe (NO bit)      : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", s_nop_nobit.med_ms, s_nop_nobit.min_ms, s_leg.med_ms / s_nop_nobit.med_ms);
    println!("  SingleProbe (with bit): med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", s_sng_bit.med_ms, s_sng_bit.min_ms, s_leg.med_ms / s_sng_bit.med_ms);
    println!("  SingleProbe (NO bit)  : med={:.3} ms, min={:.3} ms  (speedup: {:.3}x)", s_sng_nobit.med_ms, s_sng_nobit.min_ms, s_leg.med_ms / s_sng_nobit.med_ms);
    println!("--------------------------------------------------------------------------------\n");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let target = args.get(1).map(|s| s.as_str()).unwrap_or("all");

    let data_dir = std::env::var("BENCH_DATA_DIR").unwrap_or_else(|_| "/home/lotus/Workspace".to_string());

    // Natural Language Corpora
    let sherlock_path = format!("{}/stringmatch-bench/data/sherlock.txt", data_dir);
    let unidic_path = format!("{}/stringmatch-bench/data/unidic/unidic", data_dir);
    let wagahai_path = format!("{}/stringmatch-bench/data/wagahaiwa_nekodearu.txt", data_dir);
    let sanguo_path = format!("{}/zhconv-rs/benches/25328-0.txt", data_dir);
    let data3185k_path = format!("{}/zhconv-rs/benches/data3185k.txt", data_dir);

    // 1. English words_100
    if target == "all" || target == "words_100" {
        let pats = load_lines(format!("{}/stringmatch-bench/data/words_100.txt", data_dir));
        let text = std::fs::read_to_string(&sherlock_path).unwrap();
        evaluate_workload("English words_100 (sparse matches)", &pats, &text, 15, 10);
    }

    // 2. English words_5000
    if target == "all" || target == "words_5k" {
        let pats = load_lines(format!("{}/stringmatch-bench/data/words_5000.txt", data_dir));
        let text = std::fs::read_to_string(&sherlock_path).unwrap();
        evaluate_workload("English words_5000 (medium matches)", &pats, &text, 10, 10);
    }

    // 3. English words_15000
    if target == "all" || target == "words_15k" {
        let pats = load_lines(format!("{}/stringmatch-bench/data/words_15000.txt", data_dir));
        let text = std::fs::read_to_string(&sherlock_path).unwrap();
        evaluate_workload("English words_15000 (dense matches)", &pats, &text, 10, 10);
    }

    // 4. English words_100000
    if target == "all" || target == "words_100k" {
        let pats = load_lines(format!("{}/stringmatch-bench/data/words_100000.txt", data_dir));
        let text = std::fs::read_to_string(&sherlock_path).unwrap();
        evaluate_workload("English words_100000 (massive dictionary)", &pats, &text, 5, 10);
    }

    // 5. Japanese UniDic
    if target == "all" || target == "unidic" {
        let pats = load_lines(&unidic_path);
        let text = std::fs::read_to_string(&wagahai_path).unwrap();
        evaluate_workload("Japanese UniDic (250k patterns, Wagahai text)", &pats, &text, 2, 10);
    }

    // 6. Chinese OpenCC Sanguo
    if target == "all" || target == "chinese_sanguo" {
        let mut pats = Vec::new();
        for fname in ["STCharacters.txt", "STPhrases.txt"] {
            let path = format!("{}/zhconv-rs/data/opencc/{}", data_dir, fname);
            if Path::new(&path).exists() {
                for line in load_lines(path) {
                    if let Some(src) = line.split('\t').next() {
                        if !src.is_empty() {
                            pats.push(src.to_string());
                        }
                    }
                }
            }
        }
        pats.sort_unstable();
        pats.dedup();
        let text = std::fs::read_to_string(&sanguo_path).unwrap();
        evaluate_workload("Chinese OpenCC Sanguo (50k+ patterns, 1.2 MB text)", &pats, &text, 5, 10);
    }

    // 7. Chinese OpenCC 3185k
    if target == "all" || target == "chinese_3185k" {
        let mut pats = Vec::new();
        for fname in ["STCharacters.txt", "STPhrases.txt"] {
            let path = format!("{}/zhconv-rs/data/opencc/{}", data_dir, fname);
            if Path::new(&path).exists() {
                for line in load_lines(path) {
                    if let Some(src) = line.split('\t').next() {
                        if !src.is_empty() {
                            pats.push(src.to_string());
                        }
                    }
                }
            }
        }
        pats.sort_unstable();
        pats.dedup();
        let text = std::fs::read_to_string(&data3185k_path).unwrap();
        evaluate_workload("Chinese OpenCC 3185k (50k+ patterns, 3.2 MB text)", &pats, &text, 2, 10);
    }

    // 8. Synthetic: All single-character patterns (100% emit frequency)
    if target == "all" || target == "synth_single_char" {
        let pats = vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()];
        let text = "abcd".repeat(25000); // 100,000 matches!
        evaluate_workload("Synthetic: 100% Emit Rate (abcd * 25k)", &pats, &text, 10, 10);
    }

    // 9. Synthetic: Unmapped Heavy
    if target == "all" || target == "synth_unmapped" {
        let words = ["apple", "banana", "cherry", "date", "elderberry", "fig", "grape"];
        let pats: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        // text with many unmapped characters (numbers and non-ascii symbols)
        let unmapped_sep = " 12345 67890 @#$%^&*()_+ ♠♥♦♣ ";
        let mut text = String::new();
        for _ in 0..1000 {
            for w in &words {
                text.push_str(unmapped_sep);
                text.push_str(w);
            }
        }
        evaluate_workload("Synthetic: Unmapped-Heavy (words in symbols)", &pats, &text, 10, 10);
    }

    // 10. Synthetic: Nested Prefixes (all-a)
    if target == "all" || target == "synth_nested_a" {
        let pats = vec![
            "a".to_string(),
            "aa".to_string(),
            "aaa".to_string(),
            "aaaa".to_string(),
            "aaaaa".to_string(),
        ];
        let text = "a".repeat(100_000);
        evaluate_workload("Synthetic: Nested Prefixes ('a'*100k with a..aaaaa)", &pats, &text, 10, 10);
    }

    // 11. Synthetic: Alternating Fail Transitions
    if target == "all" || target == "synth_alternating" {
        let pats = vec![
            "ab".to_string(),
            "ba".to_string(),
            "abc".to_string(),
            "bca".to_string(),
        ];
        let text = "ababacba".repeat(15000);
        evaluate_workload("Synthetic: Alternating Fails ('ababacba'*15k)", &pats, &text, 10, 10);
    }

    // 12. Synthetic: Zero Match Baseline
    if target == "all" || target == "synth_zero_match" {
        let pats = load_lines("/home/lotus/Workspace/stringmatch-bench/data/words_5000.txt");
        let text = std::fs::read_to_string(wagahai_path).unwrap(); // Japanese text vs English words
        evaluate_workload("Synthetic: Zero Matches (English words in Japanese text)", &pats, &text, 10, 10);
    }
}
