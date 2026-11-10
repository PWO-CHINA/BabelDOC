use babeldoc_p2_stages::translation::{cross_page_batch::{self, BatchFailure, BatchResponse}, cross_page_sentence, ordinary_translation::{FormulaInput, InputPlaceholder, PlaceholderRegex, RegexMatch, RichInput, TranslateInput}};

#[derive(Default)]
struct RegexService(Vec<regex::Regex>);
impl PlaceholderRegex for RegexService {
    type Error = regex::Error;
    fn compile(&mut self, pattern: &str, ignore_case: bool) -> Result<u64, Self::Error> {
        self.0.push(regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build()?);
        Ok((self.0.len() - 1) as u64)
    }
    fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> { Ok(self.0[pattern as usize].is_match(text)) }
    fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<RegexMatch>, Self::Error> {
        Ok(self.0[pattern as usize].find_iter(text).map(|found| RegexMatch {
            full: found.as_str().into(), start: text[..found.start()].chars().count(), end: text[..found.end()].chars().count(), groups: vec![],
        }).collect())
    }
}
fn fixture() -> (cross_page_sentence::SentencePlan, RegexService, [u64; 3]) {
    let formula = InputPlaceholder::Formula(FormulaInput { id: 7, composition_index: 4, token: "{v7}".into(), pattern: r"\{v7\}".into(), source_chars: "x".into(), hint_source: false });
    let rich = InputPlaceholder::Rich(RichInput { id: 2, composition_index: 8, left: "<s2>".into(), right: "</s2>".into(), left_pattern: "<s2>".into(), right_pattern: "</s2>".into(), source_text: String::new() });
    let left = TranslateInput { text: "Earlier text {v7}. An unfinished".into(), placeholders: vec![formula.clone()], original_tokens: vec![] };
    let right = TranslateInput { text: "<s2>sentence here. Another sentence</s2>{v7}".into(), placeholders: vec![rich, formula], original_tokens: vec![] };
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    let plan = cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().unwrap();
    (plan, regex, patterns)
}

#[test]
fn returns_only_complete_valid_pair() {
    let (plan, mut regex, patterns) = fixture();
    let request = cross_page_batch::request(&plan);
    assert_eq!(request.map(|item| item.id), [0, 1, 2]);
    let result = cross_page_batch::candidate(&plan, BatchResponse::Json(r#"[{"id":2,"output":"<s2>后文</s2>{v7}"},{"id":0,"output":"前文{v7}。"},{"id":1,"output":"完整句子。"}]"#), &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(result, ("前文{v7}。".into(), "<s2>完整句子。后文</s2>{v7}".into()));
}

#[test]
fn rejects_invalid_json_ids_fields_and_placeholders() {
    let (plan, mut regex, patterns) = fixture();
    for json in [
        "[]", "{}", "not json", r#"[{"id":0,"output":"a"},{"id":0,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":"a"},{"id":1,"output":"b"}]"#, r#"[{"id":0,"output":"a"},{"id":1,"output":"b"},{"id":3,"output":"c"}]"#,
        r#"[{"id":true,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"id":0,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"\u0069d":0,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":-1,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0.0,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":"a"},{"id":1,"output":"b"},{"id":2,"output":"c"},{"id":3,"output":"d"}]"#,
        r#"[{"id":0,"output":"a","extra":1},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":"a"},{"id":1,"output":4},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":{}},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":["a"]},{"id":1,"output":"b"},{"id":2,"output":"c"}]"#,
        r#"[{"id":0,"output":"前文"},{"id":1,"output":"完整。"},{"id":2,"output":"<s2>后文</s2>{v7}"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"完整{v9}。"},{"id":2,"output":"<s2>后文</s2>{v7}"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"完整。"},{"id":2,"output":"<s2>后文</s2>"}]"#,
    ] { assert_eq!(cross_page_batch::candidate(&plan, BatchResponse::Json(json), &patterns, &mut regex).unwrap(), Err(BatchFailure::InvalidResponse), "{json}"); }
}

#[test]
fn cancellation_and_request_failure_never_return_partial_results() {
    let (plan, mut regex, patterns) = fixture();
    assert_eq!(cross_page_batch::candidate(&plan, BatchResponse::Cancelled, &patterns, &mut regex).unwrap(), Err(BatchFailure::Cancelled));
    assert_eq!(cross_page_batch::candidate(&plan, BatchResponse::RequestFailed, &patterns, &mut regex).unwrap(), Err(BatchFailure::RequestFailed));
}
