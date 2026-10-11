use daachorse::{CharwiseDoubleArrayAhoCorasick, CharwiseDoubleArrayAhoCorasickBuilder, MatchKind};
use std::time::Instant;

fn bench_case(case_name: &str, patterns: &[&str], haystack: &str, iters: usize) {
    println!("================================================================================");
    println!("  {}", case_name);
    println!(
        "  Patterns: {} | Haystack: {} bytes ({:.2} KB)",
        patterns.len(),
        haystack.len(),
        haystack.len() as f64 / 1024.0
    );
    println!("================================================================================");

    // Build Legacy automaton (MatchKind::LeftmostLongest)
    let pma_legacy: CharwiseDoubleArrayAhoCorasick<usize> =
        CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns)
            .expect("Failed to build legacy PMA");

    // Build Linear Zero-Rewind automaton (MatchKind::LeftmostLongestExperimental)
    let pma_linear: CharwiseDoubleArrayAhoCorasick<usize> =
        CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build(patterns)
            .expect("Failed to build linear PMA");

    // 1. Correctness Verification: Must match 100% identically!
    let matches_legacy: Vec<_> = pma_legacy
        .leftmost_find_iter(haystack)
        .map(|m| (m.start(), m.end(), m.value()))
        .collect();
    let matches_linear: Vec<_> = pma_linear
        .leftmost_longest_find_iter(haystack)
        .map(|m| (m.start(), m.end(), m.value()))
        .collect();

    assert_eq!(
        matches_legacy.len(),
        matches_linear.len(),
        "Match count mismatch: legacy={}, linear={}",
        matches_legacy.len(),
        matches_linear.len()
    );
    assert_eq!(
        matches_legacy, matches_linear,
        "Match content mismatch between legacy and linear!"
    );
    println!(
        "  [PARITY VERIFIED] Exact 1:1 match parity confirmed: {} matches found.",
        matches_legacy.len()
    );

    // 2. Performance Measurement
    // Warm-up
    for _ in 0..2 {
        let _ = pma_legacy.leftmost_find_iter(haystack).count();
        let _ = pma_linear.leftmost_longest_find_iter(haystack).count();
    }

    // Measure Linear Semi-DFA
    let t0 = Instant::now();
    let mut count_linear = 0;
    for _ in 0..iters {
        count_linear += pma_linear.leftmost_longest_find_iter(haystack).count();
    }
    let dur_linear = t0.elapsed();
    assert_eq!(count_linear, matches_linear.len() * iters);

    // Measure Legacy Rewind
    let t0 = Instant::now();
    let mut count_legacy = 0;
    for _ in 0..iters {
        count_legacy += pma_legacy.leftmost_find_iter(haystack).count();
    }
    let dur_legacy = t0.elapsed();
    assert_eq!(count_legacy, matches_legacy.len() * iters);

    let avg_linear = dur_linear / iters as u32;
    let avg_legacy = dur_legacy / iters as u32;

    let total_bytes = haystack.len() as f64 * iters as f64;
    let mb_linear = total_bytes / 1_000_000.0 / dur_linear.as_secs_f64();
    let mb_legacy = total_bytes / 1_000_000.0 / dur_legacy.as_secs_f64();

    let speedup = dur_legacy.as_secs_f64() / dur_linear.as_secs_f64();

    println!("\n  Results (averaged over {} iterations):", iters);
    println!(
        "    Legacy (Rewind):       {:>10.3?}  ({:>8.2} MB/s)",
        avg_legacy, mb_legacy
    );
    println!(
        "    Linear (Zero-Rewind):  {:>10.3?}  ({:>8.2} MB/s)",
        avg_linear, mb_linear
    );
    println!(
        "    >>> SPEEDUP:           {:>10.2}x faster!  ({:+.1}% time reduction)\n",
        speedup,
        (1.0 - dur_linear.as_secs_f64() / dur_legacy.as_secs_f64()) * 100.0
    );
}

fn main() {
    println!("********************************************************************************");
    println!("  ADVERSARIAL BENCHMARK: Stress-Testing Legacy Aho-Corasick Rewind Overhead");
    println!("  Comparison: MatchKind::LeftmostLongest vs MatchKind::LeftmostLongestExperimental");
    println!("********************************************************************************\n");

    // Case 1: Classical Rewind Torture Test (L = 200, N = 100,000)
    // Short match "a" committed at start; candidate "a^199 b" fails on every window;
    // legacy rewinds 199 characters for every single output emitted!
    {
        let l = 200;
        let n = 100_000;
        let pat_long = format!("{}b", "a".repeat(l - 1));
        let patterns = vec!["a", pat_long.as_str()];
        let haystack = "a".repeat(n);
        bench_case(
            "Case 1: Classic Rewind Torture (L = 200, N = 100K)",
            &patterns,
            &haystack,
            10,
        );
    }

    // Case 2: Scaled Length Torture Test (L = 1000, N = 100,000)
    // Legacy must rewind ~1000 positions per output. Linear remains O(N).
    {
        let l = 1000;
        let n = 100_000;
        let pat_long = format!("{}b", "a".repeat(l - 1));
        let patterns = vec!["a", pat_long.as_str()];
        let haystack = "a".repeat(n);
        bench_case(
            "Case 2: Scaled Deep Backtracking (L = 1000, N = 100K)",
            &patterns,
            &haystack,
            5,
        );
    }

    // Case 3: Real Chinese Pathological Prefix Mismatch (中文长句假匹配对抗)
    // Patterns have short base words ("中", "中华人民共和国") and long official titles of 30+ chars.
    // Haystack repeats the long title minus the final 2 characters repeatedly.
    // Legacy repeatedly traverses 30 chars, fails, rewinds back to the 1st char.
    {
        let base_short = "中";
        let base_mid = "中华人民共和国";
        let long_title =
            "中华人民共和国中央人民政府关于推动新时代高质量发展战略规划纲要实施细则意见通知";
        let patterns = vec![base_short, base_mid, long_title];

        // Haystack has 200 repetitions of (long_title prefix + mismatch char)
        let near_miss_prefix = &long_title[..long_title.len() - 6]; // drop last 2 Chinese chars (6 bytes)
        let repeat_unit = format!("{}错", near_miss_prefix);
        let haystack = repeat_unit.repeat(500);

        bench_case(
            "Case 3: Real Chinese Long-Prefix Near-Miss (中文长词近匹配反复回溯)",
            &patterns,
            &haystack,
            20,
        );
    }

    // Case 4: Multi-Tier Overlapping Cascades (多层阶梯嵌套词表回退)
    // Multiple overlapping patterns of powers-of-two lengths that cascade.
    {
        let mut patterns = Vec::new();
        patterns.push("一".to_string());
        for k in 1..=12 {
            let p = format!("{}乙", "甲".repeat(1 << k));
            patterns.push(p);
        }
        let patterns_str: Vec<&str> = patterns.iter().map(|s| s.as_str()).collect();

        // Haystack consists of alternating blocks of "甲"s that trigger all lengths
        let mut haystack = String::new();
        for _ in 0..500 {
            haystack.push_str(&"甲".repeat(4096));
            haystack.push('一');
        }

        bench_case(
            "Case 4: Multi-Tier Cascading Powers-of-Two Overlaps (多阶梯级联对抗)",
            &patterns_str,
            &haystack,
            10,
        );
    }

    // Case 5: Pathological Highly-Dense Repetitive Match (高密度极短词回溯与重试)
    // Fibonacci-like repeating sequences "ababab..." with prefixes
    {
        let patterns = vec![
            "ab",
            "abab",
            "ababab",
            "abababab",
            "ababababab",
            "abababababab",
            "ababababababab_x",
        ];
        let haystack = "abababababab".repeat(10_000);
        bench_case(
            "Case 5: Dense Cyclic Pattern Disambiguation (高密周期前缀冲突)",
            &patterns,
            &haystack,
            20,
        );
    }
}
