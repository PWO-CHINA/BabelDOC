use babeldoc_p2_stages::translation::cross_boundary::responses;

#[test]
fn partial_reordered_outputs_keep_identity_without_guessing_positions() {
    let expected = vec!["first".into(), "middle".into(), "last".into()];
    let output = responses::parse(
        r#"{"outputs":[{"id":"last","text":"C"},{"id":"first","text":"A"}]}"#,
        &expected,
    )
    .unwrap();
    assert_eq!(output["first"], "A");
    assert_eq!(output["last"], "C");
    assert!(!output.contains_key("middle"));
}

#[test]
fn ambiguous_or_foreign_identity_is_not_accepted() {
    let expected = vec!["first".into()];
    for response in [
        r#"{"outputs":["A"]}"#,
        r#"{"outputs":[{"id":"first","text":"A"},{"id":"first","text":"B"}]}"#,
        r#"{"outputs":[{"id":"other","text":"A"}]}"#,
        r#"{"outputs":[{"id":"first","text":null}]}"#,
        r#"{"outputs":[{"id":0,"text":"A"}]}"#,
    ] {
        assert!(responses::parse(response, &expected).is_err());
    }
}
