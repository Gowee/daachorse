use daachorse::{CharwiseDoubleArrayAhoCorasick, CharwiseDoubleArrayAhoCorasickBuilder, MatchKind};

// Simple zero-dependency XorShift64 PRNG for deterministic, reproducible fuzzing.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xdeadbeefcafe } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn gen_range(&mut self, low: usize, high: usize) -> usize {
        assert!(low <= high);
        if low == high {
            return low;
        }
        low + (self.next_u64() as usize % (high - low))
    }

    fn gen_string(&mut self, alphabet: &[char], min_len: usize, max_len: usize) -> String {
        let len = self.gen_range(min_len, max_len);
        let mut s = String::with_capacity(len * 4);
        for _ in 0..len {
            let idx = self.gen_range(0, alphabet.len());
            s.push(alphabet[idx]);
        }
        s
    }
}

fn differential_fuzz_round(patterns: &[String], haystack: &str, round: usize) {
    if patterns.is_empty() {
        return;
    }

    let pma_legacy: CharwiseDoubleArrayAhoCorasick<usize> =
        match CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns)
        {
            Ok(pma) => pma,
            Err(_) => return, // Duplicate pattern error or empty
        };

    let pma_linear: CharwiseDoubleArrayAhoCorasick<usize> =
        match CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongestExperimental)
            .build(patterns)
        {
            Ok(pma) => pma,
            Err(e) => panic!("Linear builder failed when legacy succeeded: {:?}", e),
        };

    let matches_legacy: Vec<_> = pma_legacy
        .leftmost_find_iter(haystack)
        .map(|m| (m.start(), m.end(), m.value()))
        .collect();

    let matches_linear: Vec<_> = pma_linear
        .leftmost_longest_find_iter(haystack)
        .map(|m| (m.start(), m.end(), m.value()))
        .collect();

    if matches_legacy != matches_linear {
        eprintln!("=== MISMATCH in Round {} ===", round);
        eprintln!("Patterns ({}): {:?}", patterns.len(), patterns);
        eprintln!("Haystack (len {}): {:?}", haystack.len(), haystack);
        let max_len = matches_legacy.len().max(matches_linear.len());
        for i in 0..max_len {
            let leg = matches_legacy.get(i);
            let lin = matches_linear.get(i);
            if leg != lin {
                eprintln!(
                    "First diff at match index {}: Legacy={:?}, Linear={:?}",
                    i, leg, lin
                );
                let start_idx = i.saturating_sub(5);
                let end_idx = (i + 5).min(max_len);
                eprintln!("--- Legacy matches [{}..{}] ---", start_idx, end_idx);
                for k in start_idx..end_idx {
                    if let Some(m) = matches_legacy.get(k) {
                        eprintln!("  {}: [{}..{}] = {:?}", k, m.0, m.1, patterns[m.2]);
                    }
                }
                eprintln!("--- Linear matches [{}..{}] ---", start_idx, end_idx);
                for k in start_idx..end_idx {
                    if let Some(m) = matches_linear.get(k) {
                        eprintln!("  {}: [{}..{}] = {:?}", k, m.0, m.1, patterns[m.2]);
                    }
                }
                break;
            }
        }
        panic!("Mismatch detected!");
    }
}

#[test]
fn test_repro_round_28() {
    let patterns = vec![
        "da".to_string(),
        "dbacbc".to_string(),
        "a".to_string(),
        "bbdadd".to_string(),
        "d".to_string(),
        "dbc".to_string(),
        "dbacbd".to_string(),
        "bad".to_string(),
        "cbac".to_string(),
        "cc".to_string(),
        "ddacad".to_string(),
        "b".to_string(),
    ];
    let haystack = "abbccdbdadbaacbcaacccbdbcbbcccdddbcabdbdbcbdbaaaaaabbddbaccabbddabbcabacaccabbccaddbbaadbbdadbcacbbcbcdabbaccdddbcacaccdccdacbbccbacaddbbcbbaacccacaacadcccbcccbbcdccaaacbcbcddddbabccbbbabbbdbdccdddcabdadbaaacadbdccadbbbdbbadcbaabaddaabddabcbbcbaadbbcdcdadadccccaadaabdddbacbbcdbbdcaabcbbcaaddbddcccbcccaddbdbbbaccdcabcccdbbdddddacbddacaddadcbdaabacdcabababbdbbadaaabbddcbadbbaaaabcccdcdadccddcabcdabbdbaacbaccabaccbbbaacbcdaadacbdbbddcdcacdcacdbbbdbbdddbacbdcccadabccbacaaadbccacbcccdbaacbcaabdabcbbbcdbccdcaadaadbbaddbaaddbcdbbabdbbbdabdabdbdcccccbbcadabaaddbdaadaaaacdddccddcbddcdbaaababdaaddacccbccddadadcdbdabdddccadcccccdcdbccbaabbabdacdabaccdbbbdccbcaaaabdcdacdbbbaadaacadaccbadcdcaddacbacbccbadccbbacbddcbbcbbbddccaabaacbbdccdadaddabdcbbcdcdadddaacacbbcddadbcbcdddcbaabcababbacdcdabbccacbddcbccdcbbcbacdcdaccba";
    differential_fuzz_round(&patterns, haystack, 28);
}

#[test]
fn test_parent_and_candidate_matches() {
    let patterns = vec![
        "xy".to_string(),
        "b".to_string(),
        "a".to_string(),
        "bacd".to_string(),
        "xybace".to_string(),
    ];
    let haystack = "xybacf";
    differential_fuzz_round(&patterns, haystack, 1);
}

#[test]
fn test_fuzz_binary_alphabet() {
    let mut rng = Rng::new(0x12345678);
    let alphabet = ['a', 'b'];

    for round in 0..1000 {
        let num_pats = rng.gen_range(2, 20);
        let mut patterns = Vec::with_capacity(num_pats);
        for _ in 0..num_pats {
            let pat = rng.gen_string(&alphabet, 1, 6);
            if !patterns.contains(&pat) {
                patterns.push(pat);
            }
        }
        let haystack = rng.gen_string(&alphabet, 10, 500);
        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_fuzz_small_alphabet() {
    let mut rng = Rng::new(0x87654321);
    let alphabet = ['a', 'b', 'c', 'd'];

    for round in 0..1000 {
        let num_pats = rng.gen_range(2, 30);
        let mut patterns = Vec::with_capacity(num_pats);
        for _ in 0..num_pats {
            let pat = rng.gen_string(&alphabet, 1, 8);
            if !patterns.contains(&pat) {
                patterns.push(pat);
            }
        }
        let haystack = rng.gen_string(&alphabet, 20, 1000);
        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_fuzz_unicode_chinese() {
    let mut rng = Rng::new(0xfeedface);
    let alphabet = ['中', '文', '分', '词', '测', '试', '长', '短', '匹', '配'];

    for round in 0..500 {
        let num_pats = rng.gen_range(2, 25);
        let mut patterns = Vec::with_capacity(num_pats);
        for _ in 0..num_pats {
            let pat = rng.gen_string(&alphabet, 1, 5);
            if !patterns.contains(&pat) {
                patterns.push(pat);
            }
        }
        let haystack = rng.gen_string(&alphabet, 10, 800);
        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_fuzz_heavy_nested_prefixes() {
    let mut rng = Rng::new(0xabcdef01);
    let alphabet = ['a', 'b', 'c'];

    for round in 0..500 {
        // Generate patterns where prefixes are intentionally injected
        let num_base = rng.gen_range(2, 6);
        let mut patterns = Vec::new();
        for _ in 0..num_base {
            let base = rng.gen_string(&alphabet, 3, 10);
            for k in 1..=base.len() {
                let prefix = base[..k].to_string();
                if !patterns.contains(&prefix) {
                    patterns.push(prefix);
                }
            }
        }
        let haystack = rng.gen_string(&alphabet, 50, 2000);
        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_fuzz_adversarial_near_miss() {
    let mut rng = Rng::new(0x99999999);

    for round in 0..300 {
        let l = rng.gen_range(10, 100);
        let pat_long = format!("{}b", "a".repeat(l));
        let pat_short = "a".repeat(l / 2);
        let pat_single = "a".to_string();
        let patterns = vec![pat_long, pat_short, pat_single];

        // Haystack with near misses: a^(l-1) c
        let reps = rng.gen_range(10, 50);
        let block = format!("{}c", "a".repeat(l - 1));
        let haystack = block.repeat(reps);

        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_fuzz_stress_large_corpus() {
    let mut rng = Rng::new(0x55555555);
    let alphabet = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];

    for round in 0..100 {
        let num_pats = rng.gen_range(50, 200);
        let mut patterns = Vec::with_capacity(num_pats);
        for _ in 0..num_pats {
            let pat = rng.gen_string(&alphabet, 1, 15);
            if !patterns.contains(&pat) {
                patterns.push(pat);
            }
        }
        let haystack = rng.gen_string(&alphabet, 1000, 10000);
        differential_fuzz_round(&patterns, &haystack, round);
    }
}

#[test]
fn test_debug_trace() {
    let patterns = vec![
        "da".to_string(),
        "dbacbc".to_string(),
        "a".to_string(),
        "bbdadd".to_string(),
        "d".to_string(),
        "dbc".to_string(),
        "dbacbd".to_string(),
        "bad".to_string(),
        "cbac".to_string(),
        "cc".to_string(),
        "ddacad".to_string(),
        "b".to_string(),
    ];
    let pma_linear: daachorse::CharwiseDoubleArrayAhoCorasick<usize> =
        daachorse::CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(daachorse::MatchKind::LeftmostLongestExperimental)
            .build(&patterns)
            .unwrap();

    let haystack = "abbddbac";
    println!("Tracing haystack: {}", haystack);
    for m in pma_linear.leftmost_longest_find_iter(haystack) {
        println!(
            "Match: [{}..{}] = {}",
            m.start(),
            m.end(),
            patterns[m.value()]
        );
    }

    let pma_legacy: daachorse::CharwiseDoubleArrayAhoCorasick<usize> =
        daachorse::CharwiseDoubleArrayAhoCorasickBuilder::new()
            .match_kind(daachorse::MatchKind::LeftmostLongest)
            .build(&patterns)
            .unwrap();
    println!("Legacy matches:");
    for m in pma_legacy.leftmost_find_iter(haystack) {
        println!(
            "Legacy Match: [{}..{}] = {}",
            m.start(),
            m.end(),
            patterns[m.value()]
        );
    }
}
