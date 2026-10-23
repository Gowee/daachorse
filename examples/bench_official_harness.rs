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
            syscall(298 /* sys_perf_event_open */, &attr, 0, -1, -1, 0) as i32
        };
        if fd < 0 {
            Self { file: None }
        } else {
            Self {
                file: Some(unsafe { File::from_raw_fd(fd) }),
            }
        }
    }

    fn instructions() -> Self {
        Self::new(0, 1)
    }
    fn cycles() -> Self {
        Self::new(0, 0)
    }

    fn start(&mut self) {
        if let Some(f) = &self.file {
            use std::os::unix::io::AsRawFd;
            unsafe {
                ioctl(f.as_raw_fd(), 0x2400, 0);
            }
        }
    }

    fn stop(&mut self) -> u64 {
        if let Some(mut f) = self.file.take() {
            use std::os::unix::io::AsRawFd;
            unsafe {
                ioctl(f.as_raw_fd(), 0x2401, 0);
            }
            let mut buf = [0u8; 8];
            let _ = f.read_exact(&mut buf);
            u64::from_ne_bytes(buf)
        } else {
            0
        }
    }
}

// Exact official harness match consumption:
#[inline(never)]
fn run_legacy(da: &CharwiseDoubleArrayAhoCorasick<u32>, haystacks: &[String]) -> usize {
    let mut sum = 0usize;
    for h in haystacks {
        for m in da.leftmost_find_iter(h) {
            sum += m.start() + m.end() + m.value() as usize;
        }
    }
    black_box(sum)
}

#[inline(never)]
fn run_v6695258(da: &CharwiseDoubleArrayAhoCorasick<u32>, haystacks: &[String]) -> usize {
    let mut sum = 0usize;
    for h in haystacks {
        for m in da.leftmost_find_iter_v6695258(h) {
            sum += m.start() + m.end() + m.value() as usize;
        }
    }
    black_box(sum)
}

#[inline(never)]
fn run_fast_root(da: &CharwiseDoubleArrayAhoCorasick<u32>, haystacks: &[String]) -> usize {
    let mut sum = 0usize;
    for h in haystacks {
        for m in da.leftmost_find_iter_fast_root(h) {
            sum += m.start() + m.end() + m.value() as usize;
        }
    }
    black_box(sum)
}

#[inline(never)]
fn run_single_probe(da: &CharwiseDoubleArrayAhoCorasick<u32>, haystacks: &[String]) -> usize {
    let mut sum = 0usize;
    for h in haystacks {
        for m in da.leftmost_find_iter_single_probe(h) {
            sum += m.start() + m.end() + m.value() as usize;
        }
    }
    black_box(sum)
}

#[inline(never)]
fn run_peeled_cache(da: &CharwiseDoubleArrayAhoCorasick<u32>, haystacks: &[String]) -> usize {
    let mut sum = 0usize;
    for h in haystacks {
        for m in da.leftmost_find_iter_peeled_cache(h) {
            sum += m.start() + m.end() + m.value() as usize;
        }
    }
    black_box(sum)
}

fn load_patterns<P: AsRef<Path>>(path: P) -> Vec<String> {
    let file = File::open(path).unwrap();
    let mut pats: Vec<String> = BufReader::new(file).lines().map(|l| l.unwrap()).collect();
    pats.sort_unstable(); // Official harness sorts patterns
    pats
}

fn load_whole<P: AsRef<Path>>(path: P) -> Vec<String> {
    let mut s = String::new();
    BufReader::new(File::open(path).unwrap())
        .read_to_string(&mut s)
        .unwrap();
    vec![s]
}

fn load_lines<P: AsRef<Path>>(path: P) -> Vec<String> {
    let file = File::open(path).unwrap();
    BufReader::new(file).lines().map(|l| l.unwrap()).collect()
}

#[derive(Clone, Copy)]
struct Metric {
    ns: f64,
    cycles: u64,
    instrs: u64,
}

struct ArmResult {
    med_ms: f64,
    _mean_ms: f64,
    _min_ms: f64,
    _max_ms: f64,
    med_cyc: u64,
    instrs: u64,
}

fn analyze_arm(m: &[Metric]) -> ArmResult {
    let mut ms_vals: Vec<_> = m.iter().map(|x| x.ns / 1e6).collect();
    ms_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min_ms = ms_vals[0];
    let max_ms = ms_vals[ms_vals.len() - 1];
    let med_ms = ms_vals[ms_vals.len() / 2];
    let mean_ms = ms_vals.iter().sum::<f64>() / ms_vals.len() as f64;

    let mut cyc_vals: Vec<_> = m.iter().map(|x| x.cycles).collect();
    cyc_vals.sort();
    let med_cyc = cyc_vals[cyc_vals.len() / 2];

    let instrs = m[0].instrs;

    ArmResult {
        med_ms,
        _mean_ms: mean_ms,
        _min_ms: min_ms,
        _max_ms: max_ms,
        med_cyc,
        instrs,
    }
}

pub struct WorkloadResult {
    corpus: String,
    mode: String,
    legacy: ArmResult,
    v6695258: ArmResult,
    fast_root: ArmResult,
    single_probe: ArmResult,
    peeled_cache: ArmResult,
}

fn eval_workload(
    corpus_name: &str,
    mode_name: &str,
    pat_path: &str,
    haystack_path: &str,
    longtext: bool,
    rounds: usize,
) -> Option<WorkloadResult> {
    if !Path::new(pat_path).exists() || !Path::new(haystack_path).exists() {
        println!("Skipping {}_{}: file not found", corpus_name, mode_name);
        return None;
    }

    let patterns = load_patterns(pat_path);
    let haystacks = if longtext {
        load_whole(haystack_path)
    } else {
        load_lines(haystack_path)
    };

    let leg: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&patterns)
        .unwrap();

    let exp: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongestExperimental)
        .build(&patterns)
        .unwrap();

    // Verify 100% equivalence & sanity check sum across all 5 arms
    let mut leg_count = 0;
    let mut v6695258_count = 0;
    let mut fast_root_count = 0;
    let mut sp_count = 0;
    let mut pc_count = 0;
    for h in &haystacks {
        let leg_m: Vec<_> = leg
            .leftmost_find_iter(h)
            .map(|m| (m.start(), m.end(), m.value()))
            .collect();
        let v6695258_m: Vec<_> = exp
            .leftmost_find_iter_v6695258(h)
            .map(|m| (m.start(), m.end(), m.value()))
            .collect();
        let fast_root_m: Vec<_> = exp
            .leftmost_find_iter_fast_root(h)
            .map(|m| (m.start(), m.end(), m.value()))
            .collect();
        let sp_m: Vec<_> = exp
            .leftmost_find_iter_single_probe(h)
            .map(|m| (m.start(), m.end(), m.value()))
            .collect();
        let pc_m: Vec<_> = exp
            .leftmost_find_iter_peeled_cache(h)
            .map(|m| (m.start(), m.end(), m.value()))
            .collect();
        assert_eq!(
            leg_m, v6695258_m,
            "v6695258 mismatch on {}/{}",
            corpus_name, mode_name
        );
        assert_eq!(
            leg_m, fast_root_m,
            "FastRoot mismatch on {}/{}",
            corpus_name, mode_name
        );
        assert_eq!(
            leg_m, sp_m,
            "SingleProbe mismatch on {}/{}",
            corpus_name, mode_name
        );
        assert_eq!(
            leg_m, pc_m,
            "PeeledCache mismatch on {}/{}",
            corpus_name, mode_name
        );
        leg_count += leg_m.len();
        v6695258_count += v6695258_m.len();
        fast_root_count += fast_root_m.len();
        sp_count += sp_m.len();
        pc_count += pc_m.len();
    }
    assert_eq!(leg_count, v6695258_count);
    assert_eq!(leg_count, fast_root_count);
    assert_eq!(leg_count, sp_count);
    assert_eq!(leg_count, pc_count);

    // Warmup
    for _ in 0..2 {
        run_legacy(&leg, &haystacks);
        run_v6695258(&exp, &haystacks);
        run_fast_root(&exp, &haystacks);
        run_single_probe(&exp, &haystacks);
        run_peeled_cache(&exp, &haystacks);
    }

    let mut leg_metrics = Vec::new();
    let mut v6695258_metrics = Vec::new();
    let mut fast_root_metrics = Vec::new();
    let mut sp_metrics = Vec::new();
    let mut pc_metrics = Vec::new();

    // 15 Interleaved rounds with strict 5-way cyclic rotation:
    for round in 0..rounds {
        let order = match round % 5 {
            0 => [0, 1, 2, 3, 4],
            1 => [1, 2, 3, 4, 0],
            2 => [2, 3, 4, 0, 1],
            3 => [3, 4, 0, 1, 2],
            4 => [4, 0, 1, 2, 3],
            _ => unreachable!(),
        };

        for &arm in &order {
            sleep(Duration::from_millis(15));

            let mut c_ins = PerfCounter::instructions();
            let mut c_cyc = PerfCounter::cycles();

            c_ins.start();
            c_cyc.start();
            let t0 = Instant::now();

            let _ = match arm {
                0 => run_legacy(&leg, &haystacks),
                1 => run_v6695258(&exp, &haystacks),
                2 => run_fast_root(&exp, &haystacks),
                3 => run_single_probe(&exp, &haystacks),
                4 => run_peeled_cache(&exp, &haystacks),
                _ => unreachable!(),
            };

            let dt = t0.elapsed();
            let cyc = c_cyc.stop();
            let ins = c_ins.stop();

            let metric = Metric {
                ns: dt.as_nanos() as f64,
                cycles: cyc,
                instrs: ins,
            };

            match arm {
                0 => leg_metrics.push(metric),
                1 => v6695258_metrics.push(metric),
                2 => fast_root_metrics.push(metric),
                3 => sp_metrics.push(metric),
                4 => pc_metrics.push(metric),
                _ => unreachable!(),
            }
        }
    }

    let leg_res = analyze_arm(&leg_metrics);
    let v6695258_res = analyze_arm(&v6695258_metrics);
    let fast_root_res = analyze_arm(&fast_root_metrics);
    let sp_res = analyze_arm(&sp_metrics);
    let pc_res = analyze_arm(&pc_metrics);

    Some(WorkloadResult {
        corpus: corpus_name.to_string(),
        mode: mode_name.to_string(),
        legacy: leg_res,
        v6695258: v6695258_res,
        fast_root: fast_root_res,
        single_probe: sp_res,
        peeled_cache: pc_res,
    })
}

fn run_all() {
    let base = "/root/smb/data";
    println!("\n# Official-Harness Benchmark on zin (15 Interleaved Rounds)");
    println!("Arms: [0] Legacy  [1] v6695258 (Initial)  [2] FastRoot (Zero Tricks + Fast Root Path)  [3] SingleProbe  [4] PeeledCache");
    println!("Rotation: 5-way cyclic order per round (0->1->2->3->4, 1->2->3->4->0, ...)\n");

    let workloads = [
        (
            "unidic",
            "long-text",
            format!("{}/unidic/unidic", base),
            format!("{}/wagahaiwa_nekodearu.txt", base),
            true,
        ),
        (
            "unidic",
            "per-line",
            format!("{}/unidic/unidic", base),
            format!("{}/wagahaiwa_nekodearu.txt", base),
            false,
        ),
        (
            "words_100000",
            "long-text",
            format!("{}/words_100000.txt", base),
            format!("{}/sherlock.txt", base),
            true,
        ),
        (
            "words_100000",
            "per-line",
            format!("{}/words_100000.txt", base),
            format!("{}/sherlock.txt", base),
            false,
        ),
        (
            "words_15000",
            "long-text",
            format!("{}/words_15000.txt", base),
            format!("{}/sherlock.txt", base),
            true,
        ),
        (
            "words_15000",
            "per-line",
            format!("{}/words_15000.txt", base),
            format!("{}/sherlock.txt", base),
            false,
        ),
        (
            "words_5000",
            "long-text",
            format!("{}/words_5000.txt", base),
            format!("{}/sherlock.txt", base),
            true,
        ),
        (
            "words_5000",
            "per-line",
            format!("{}/words_5000.txt", base),
            format!("{}/sherlock.txt", base),
            false,
        ),
        (
            "words_100",
            "long-text",
            format!("{}/words_100.txt", base),
            format!("{}/sherlock.txt", base),
            true,
        ),
        (
            "words_100",
            "per-line",
            format!("{}/words_100.txt", base),
            format!("{}/sherlock.txt", base),
            false,
        ),
    ];

    let mut results = Vec::new();
    for (corpus, mode, pat, hay, longtext) in workloads {
        print!("Benchmarking {} ({}) ... ", corpus, mode);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        if let Some(r) = eval_workload(corpus, mode, &pat, &hay, longtext, 15) {
            println!(
                "done. (Leg: {:.2}ms, 6695258: {:.2}ms, FR: {:.2}ms, SP: {:.2}ms, PC: {:.2}ms)",
                r.legacy.med_ms,
                r.v6695258.med_ms,
                r.fast_root.med_ms,
                r.single_probe.med_ms,
                r.peeled_cache.med_ms
            );
            results.push(r);
        }
    }

    println!("\n### 1. Wall-Clock Execution Time (ms, 15 Interleaved Rounds)");
    println!("| Corpus | Mode | Legacy (ms) | v6695258 (ms) | FastRoot (ms) | FR vs Leg | SingleProbe (ms) | PeeledCache (ms) |");
    println!("|:---|:---|:---:|:---:|:---:|:---:|:---:|:---:|");
    for r in &results {
        let fr_vs_leg = (r.fast_root.med_ms - r.legacy.med_ms) / r.legacy.med_ms * 100.0;
        println!(
            "| {:<12} | {:<9} | {:8.2} | {:8.2} | {:8.2} | {:+8.2}% | {:8.2} | {:8.2} |",
            r.corpus,
            r.mode,
            r.legacy.med_ms,
            r.v6695258.med_ms,
            r.fast_root.med_ms,
            fr_vs_leg,
            r.single_probe.med_ms,
            r.peeled_cache.med_ms
        );
    }

    println!("\n### 2. Hardware Instructions Retired");
    println!("| Corpus | Mode | Legacy Instr | v6695258 Instr | FastRoot Instr | FR vs Leg | SingleProbe Instr | PeeledCache Instr |");
    println!("|:---|:---|:---:|:---:|:---:|:---:|:---:|:---:|");
    for r in &results {
        let fr_vs_leg =
            (r.fast_root.instrs as f64 - r.legacy.instrs as f64) / r.legacy.instrs as f64 * 100.0;
        println!(
            "| {:<12} | {:<9} | {:12} | {:12} | {:12} | {:+8.2}% | {:12} | {:12} |",
            r.corpus,
            r.mode,
            r.legacy.instrs,
            r.v6695258.instrs,
            r.fast_root.instrs,
            fr_vs_leg,
            r.single_probe.instrs,
            r.peeled_cache.instrs
        );
    }

    println!("\n### 3. CPU Cycles (Median)");
    println!("| Corpus | Mode | Legacy Cycles | v6695258 Cycles | FastRoot Cycles | FR vs Leg | SingleProbe Cycles | PeeledCache Cycles |");
    println!("|:---|:---|:---:|:---:|:---:|:---:|:---:|:---:|");
    for r in &results {
        let fr_vs_leg = (r.fast_root.med_cyc as f64 - r.legacy.med_cyc as f64)
            / r.legacy.med_cyc as f64
            * 100.0;
        println!(
            "| {:<12} | {:<9} | {:12} | {:12} | {:12} | {:+8.2}% | {:12} | {:12} |",
            r.corpus,
            r.mode,
            r.legacy.med_cyc,
            r.v6695258.med_cyc,
            r.fast_root.med_cyc,
            fr_vs_leg,
            r.single_probe.med_cyc,
            r.peeled_cache.med_cyc
        );
    }
}

fn main() {
    run_all();
}
