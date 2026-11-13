use babeldoc_p2_stages::translation::llm_only::interpret_response;

#[test]
fn baseline_never_uses_negative_or_coerced_response_ids() {
    let inputs = vec!["Alpha beta".into(), "Gamma delta".into()];
    for invalid in ["-1", "2", "0.5", "true", "\"1\"", "null"] {
        let response = format!(r#"[{{"id":0,"output":"基础甲乙"}},{{"id":{invalid},"output":"基础丙丁"}}]"#);
        let result = interpret_response(&response, &inputs, true);
        assert!(result.items.is_empty(), "accepted ID {invalid}: {result:?}");
        assert_eq!(result.fallback_indices, vec![0, 1]);
        assert!(result.error.is_some());
    }
}

#[test]
fn baseline_rejects_duplicate_ids_and_accepts_reordered_known_ids() {
    let inputs = vec!["Alpha beta".into(), "Gamma delta".into()];
    for response in [
        r#"[{"id":0,"output":"基础甲乙"},{"id":0,"output":"损坏"},{"id":1,"output":"基础丙丁"}]"#,
        r#"[{"id":0,"id":1,"output":"基础甲乙"},{"id":0,"output":"基础丙丁"}]"#,
    ] {
        let result = interpret_response(response, &inputs, true);
        assert!(result.items.is_empty());
        assert_eq!(result.fallback_indices, vec![0, 1]);
        assert!(result.error.is_some());
    }
    let result = interpret_response(r#"[{"id":1,"output":"基础丙丁"},{"id":0,"output":"基础甲乙"}]"#, &inputs, true);
    assert!(result.error.is_none());
    assert_eq!(result.items.iter().map(|item| item.input_index).collect::<Vec<_>>(), vec![1, 0]);
    assert!(result.fallback_indices.is_empty());
}

#[test]
fn echoed_input_without_output_is_not_a_translation() {
    let inputs = vec!["Alpha beta".into()];
    let result = interpret_response(r#"[{"id":0,"input":"Alpha beta"}]"#, &inputs, true);
    assert!(result.items.is_empty());
    assert_eq!(result.fallback_indices, vec![0]);
}
