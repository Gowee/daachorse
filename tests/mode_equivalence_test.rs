use daachorse::{CharwiseDoubleArrayAhoCorasick, CharwiseDoubleArrayAhoCorasickBuilder, MatchKind};

#[test]
fn test_experimental_mode_equivalence() {
    let patterns = vec![
        "apple",
        "app",
        "application",
        "banana",
        "band",
        "bandana",
        "an",
        "na",
        "orange",
        "ge",
        "range",
        "ran",
        "a",
        "b",
        "c",
    ];

    let pma_leg: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&patterns)
        .unwrap();

    let pma_exp: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongestExperimental)
        .build(&patterns)
        .unwrap();

    let rep_a = "a".repeat(100);
    let rep_b = "banana".repeat(50);
    let test_texts = [
        "banana apple application band bandana orange rang an na",
        "xyz non matching text here 12345",
        "app apple application banana band bandana a b c",
        "a",
        "an",
        "na",
        "orange",
        "application",
        &rep_a,
        &rep_b,
    ];

    for text in &test_texts {
        let leg_matches: Vec<_> = pma_leg.leftmost_find_iter(text).collect();
        let exp_matches: Vec<_> = pma_exp.leftmost_find_iter_experimental(text).collect();

        assert_eq!(
            leg_matches, exp_matches,
            "Experimental mismatch on {:?}",
            text
        );
    }
}

#[test]
fn test_experimental_mode_equivalence_on_words() {
    let words_path = "/home/lotus/Workspace/stringmatch-bench/data/words_5000.txt";
    let sherlock_path = "/home/lotus/Workspace/stringmatch-bench/data/sherlock.txt";
    if !std::path::Path::new(words_path).exists() || !std::path::Path::new(sherlock_path).exists() {
        return;
    }

    let words: Vec<String> = std::fs::read_to_string(words_path)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect();
    let text = std::fs::read_to_string(sherlock_path).unwrap();

    let pma_leg: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&words)
        .unwrap();

    let pma_exp: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongestExperimental)
        .build(&words)
        .unwrap();

    let leg_matches: Vec<_> = pma_leg.leftmost_find_iter(&text).collect();
    let exp_matches: Vec<_> = pma_exp.leftmost_find_iter_experimental(&text).collect();

    assert_eq!(leg_matches.len(), exp_matches.len());
    assert_eq!(leg_matches, exp_matches);
}

#[test]
fn test_experimental_mode_equivalence_on_unidic() {
    let unidic_path = "/home/lotus/Workspace/stringmatch-bench/data/unidic/unidic";
    let wagahai_path = "/home/lotus/Workspace/stringmatch-bench/data/wagahaiwa_nekodearu.txt";
    if !std::path::Path::new(unidic_path).exists() || !std::path::Path::new(wagahai_path).exists() {
        return;
    }

    let words: Vec<String> = std::fs::read_to_string(unidic_path)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect();
    let text = std::fs::read_to_string(wagahai_path).unwrap();

    let pma_leg: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&words)
        .unwrap();

    let pma_exp: CharwiseDoubleArrayAhoCorasick<u32> = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongestExperimental)
        .build(&words)
        .unwrap();

    let leg_matches: Vec<_> = pma_leg.leftmost_find_iter(&text).collect();
    let exp_matches: Vec<_> = pma_exp.leftmost_find_iter_experimental(&text).collect();

    assert_eq!(leg_matches.len(), exp_matches.len());
    assert_eq!(leg_matches, exp_matches);
}
