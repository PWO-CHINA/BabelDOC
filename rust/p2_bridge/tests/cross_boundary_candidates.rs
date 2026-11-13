#![allow(dead_code)]

#[path = "../src/cross_boundary_candidates.rs"]
mod cross_boundary_candidates;

#[test]
fn focuses_actual_boundaries_in_saved_acceptance_metadata() {
    use cross_boundary_candidates::{Boundary, focus};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (sample, sequence, expected_left, expected_right) in [
        ("03-Cassetta", 11, (0, 14), (1, 5)),
        ("04-Katoh", 7, (0, 10), (1, 5)),
        ("05-Patke", 20, (0, 6), (1, 0)),
        ("02-Zhang", 17, (0, 7), (1, 1)),
    ] {
        let path = root.join(".crossboundary-acceptance-20261112").join(sample).join("requests.jsonl");
        if !path.is_file() { continue; }
        let records = std::fs::read_to_string(path).unwrap();
        let record = records.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|record| record["sequence"] == sequence).unwrap();
        let metadata = record["prompt"].as_str().unwrap().split_once("Metadata: ").unwrap().1
            .split_once(" Formula markers and source characters: ").unwrap().0;
        let original: Boundary = serde_json::from_str(metadata).unwrap();
        let original_count = original.left.len() + original.right.len();
        let focused = focus(original).unwrap();
        assert!(focused.left.iter().any(|candidate| (candidate.page, candidate.paragraph) == expected_left), "{sample} left target missing");
        assert!(focused.right.iter().any(|candidate| (candidate.page, candidate.paragraph) == expected_right), "{sample} right target missing");
        assert!(focused.left.len() + focused.right.len() < original_count, "{sample} was not narrowed");
    }
}
