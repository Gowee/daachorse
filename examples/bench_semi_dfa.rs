use daachorse::{CharwiseDoubleArrayAhoCorasick, CharwiseDoubleArrayAhoCorasickBuilder, MatchKind};
use std::time::Instant;

fn main() {
    println!("================================================================================");
    println!("  BENCHMARK: Zero-Rewind LeftmostLongest Semi-DFA vs Legacy Rewind");
    println!("================================================================================\n");

    // Case 1: Worst-case rewind torture
    // Pattern: 200 'a's followed by 'b', and search in 100,000 'a's.
    // Legacy rewind: On every mismatch after 199 'a's, rewinds 198 positions -> O(N * L) steps!
    // Semi-DFA: Single pass forward only, zero rewind -> O(N) steps!
    {
        println!("--- Case 1: Worst-Case Rewind Torture Test ---");
        let pattern_len = 200;
        let text_len = 100_000;
        let mut pat = "a".repeat(pattern_len);
        pat.push('b');
        let patterns = vec![pat.as_str()];
        let haystack = "a".repeat(text_len);

        let pma_longest: CharwiseDoubleArrayAhoCorasick<usize> =
            CharwiseDoubleArrayAhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongestExperimental)
                .build(&patterns)
                .unwrap();

        let pma_first: CharwiseDoubleArrayAhoCorasick<usize> =
            CharwiseDoubleArrayAhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostFirst)
                .build(&patterns)
                .unwrap();

        // Warm up
        let _ = pma_longest.leftmost_longest_find_iter(&haystack).count();
        let _ = pma_first.leftmost_find_iter(&haystack).count();

        let iters = 20;
        let t0 = Instant::now();
        let mut count1 = 0;
        for _ in 0..iters {
            count1 += pma_longest.leftmost_longest_find_iter(&haystack).count();
        }
        let elapsed_longest = t0.elapsed();

        let t0 = Instant::now();
        let mut count2 = 0;
        for _ in 0..iters {
            count2 += pma_first.leftmost_find_iter(&haystack).count();
        }
        let elapsed_legacy = t0.elapsed();

        assert_eq!(count1, count2);
        let speedup = elapsed_legacy.as_secs_f64() / elapsed_longest.as_secs_f64();
        println!(
            "  Text length: {} chars, Pattern length: {} chars",
            text_len, pattern_len
        );
        println!(
            "  Semi-DFA (LeftmostLongestExperimental): {:>10.3?}  ({:.1} MB/s)",
            elapsed_longest / iters,
            (text_len * iters as usize) as f64 / 1_000_000.0 / elapsed_longest.as_secs_f64()
        );
        println!(
            "  Legacy Rewind (LeftmostFirst): {:>10.3?}  ({:.1} MB/s)",
            elapsed_legacy / iters,
            (text_len * iters as usize) as f64 / 1_000_000.0 / elapsed_legacy.as_secs_f64()
        );
        println!("  >>> SPEEDUP: {:.1}x faster! (O(N) vs O(N*L))\n", speedup);
    }

    // Case 2: Realistic Multibyte Text Matching
    {
        println!("--- Case 2: Realistic Multibyte Text Matching ---");
        let patterns = vec![
            "全世界",
            "世界",
            "人民",
            "大团结",
            "万岁",
            "共产党",
            "伟大",
            "中国",
            "中华",
            "中华人民共和国",
            "天安门",
            "太阳",
            "升起",
        ];
        let base_text = "中华人民共和国万岁，世界人民大团结万岁！在天安门广场上，伟大的中国人民看到太阳升起。全世界人民心连心。";
        let haystack = base_text.repeat(10_000); // ~1.5 MB text

        let pma_longest: CharwiseDoubleArrayAhoCorasick<usize> =
            CharwiseDoubleArrayAhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongestExperimental)
                .build(&patterns)
                .unwrap();

        let pma_legacy: CharwiseDoubleArrayAhoCorasick<usize> =
            CharwiseDoubleArrayAhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
                .unwrap();

        // Warm up
        let matches_longest: Vec<_> = pma_longest
            .leftmost_longest_find_iter(&haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        let matches_leftmost: Vec<_> = pma_legacy
            .leftmost_find_iter(&haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(matches_longest, matches_leftmost);

        let iters = 50;
        let t0 = Instant::now();
        let mut count = 0;
        for _ in 0..iters {
            count += pma_longest.leftmost_longest_find_iter(&haystack).count();
        }
        let elapsed = t0.elapsed();
        assert_eq!(count, matches_longest.len() * iters as usize);

        let total_bytes = haystack.len() * iters as usize;
        let throughput = total_bytes as f64 / 1_000_000.0 / elapsed.as_secs_f64();

        println!(
            "  Haystack size: {:.2} MB ({} iterations)",
            haystack.len() as f64 / 1_000_000.0,
            iters
        );
        println!("  Total matches per run: {}", matches_longest.len());
        println!("  Average time per run: {:?}", elapsed / iters);
        println!("  Throughput: {:.2} MB/s", throughput);
        println!("  Both leftmost_longest_find_iter and leftmost_find_iter produced 100% identical matches.\n");
    }

    println!("================================================================================");
    println!("  All benchmarks completed successfully!");
    println!("================================================================================");
}
