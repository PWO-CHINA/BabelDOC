use babeldoc_p2_stages::translation::{cross_page_sentence, ordinary_translation::{FormulaInput, InputPlaceholder, PlaceholderRegex, RegexMatch, RichInput, TranslateInput}};

#[test]
fn enumerates_conservative_natural_splits_without_changing_text() {
    for (text, expected) in [
        ("前半句，后半句；结尾。", vec![("前半句，", "后半句；结尾。"), ("前半句，后半句；", "结尾。")]),
        ("α😀,  β", vec![("α😀,", "  β")]),
        ("甲、乙", vec![("甲、", "乙")]),
        ("没有断点。", vec![]),
        ("123,456", vec![]),
        ("甲，，乙", vec![]),
        ("甲，  ，乙", vec![]),
        ("“甲，乙”", vec![]),
        ("甲，   ", vec![]),
        ("甲{v1}，乙", vec![]),
        ("<b>甲</b>，乙", vec![]),
        ("甲 $x+y$，乙", vec![]),
        ("甲 \\alpha，乙", vec![]),
        ("甲 %s，乙", vec![]),
        ("甲 x_1，乙", vec![]),
        ("甲 x+y，乙", vec![]),
        ("甲 x≤y，乙", vec![]),
        ("甲 x/y，乙", vec![]),
        ("1, 234", vec![]),
        ("甲，\u{200b}乙", vec![]),
        ("甲，\n乙", vec![]),
        ("甲，\u{a0}乙", vec![("甲，", "\u{a0}乙")]),
    ] {
        let candidates = cross_page_sentence::natural_split_candidates(text);
        assert_eq!(candidates, expected);
        for (left, right) in candidates {
            assert_eq!(format!("{left}{right}"), text);
            assert!(!left.trim().is_empty() && !right.trim().is_empty());
        }
    }
}

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
            full: found.as_str().into(), start: text[..found.start()].chars().count(),
            end: text[..found.end()].chars().count(), groups: vec![],
        }).collect())
    }
}

fn rich(id: i64, index: usize) -> InputPlaceholder {
    InputPlaceholder::Rich(RichInput { id, composition_index: index, left: format!("<s{id}>"), right: format!("</s{id}>"),
        left_pattern: format!("<s{id}>"), right_pattern: format!("</s{id}>"), source_text: String::new() })
}
fn fixture() -> (TranslateInput, TranslateInput) {
    let formula = InputPlaceholder::Formula(FormulaInput { id: 7, composition_index: 4, token: "{v7}".into(), pattern: r"\{v7\}".into(), source_chars: "x".into(), hint_source: false });
    (TranslateInput { text: "{v7}<s1>Earlier sentence. This unfinished".to_owned() + "</s1>", placeholders: vec![formula.clone(), rich(1, 5)], original_tokens: vec![] },
     TranslateInput { text: "<s2>continues here. Another sentence</s2>{v7}".into(), placeholders: vec![rich(2, 8), formula], original_tokens: vec![] })
}
fn patterns(regex: &mut RegexService) -> [u64; 3] {
    [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()]
}

#[test]
fn preserves_original_page_compositions_and_complete_sentence() {
    let (left, right) = fixture();
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    let plan = cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(plan.sentence, "This unfinished continues here.");
    assert_eq!(plan.left_prefix.text, "{v7}<s1>Earlier sentence. </s1>");
    assert_eq!(plan.right_suffix.text, "<s2>Another sentence</s2>{v7}");
    assert_eq!(plan.left_prefix.placeholders, left.placeholders);
    assert_eq!(plan.right_suffix.placeholders, right.placeholders);
    let result = cross_page_sentence::assemble(&plan, "{v7}<s1>前文。</s1>", "完整句子。", "<s2>后文</s2>{v7}", &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(result, ("{v7}<s1>前文。</s1>".into(), "<s2>完整句子。后文</s2>{v7}".into()));
    assert!(cross_page_sentence::assemble(&plan, "<s1>前文。</s1>", "完整句子。", "<s2>后文</s2>{v7}", &patterns, &mut regex).unwrap().is_none());
    assert!(cross_page_sentence::assemble(&plan, "{v7}<s1>前文。</s1>", "完整{v8}句子。", "<s2>后文</s2>{v7}", &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn rejects_missing_proofs_ambiguous_boundaries_and_protected_content() {
    let (left, right) = fixture();
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    assert!(cross_page_sentence::prepare(&left, &right, &[], &[], false, true, &patterns, &mut regex).unwrap().is_none());
    assert!(cross_page_sentence::prepare(&left, &right, &[], &[], true, false, &patterns, &mut regex).unwrap().is_none());
    let index = left.text.find("unfinished").unwrap();
    assert!(cross_page_sentence::prepare(&left, &right, &[index..index+3], &[], true, true, &patterns, &mut regex).unwrap().is_none());
    let mut ambiguous = right.clone(); ambiguous.text = ambiguous.text.replace("continues here.", "Dr. Smith continues.");
    assert!(cross_page_sentence::prepare(&left, &ambiguous, &[], &[], true, true, &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn handles_multiple_complete_sentences_without_moving_them() {
    let (mut left, right) = fixture();
    left.text = left.text.replace("Earlier sentence.", "Earlier sentence. Another complete sentence.");
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    let plan = cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().unwrap();
    assert!(plan.left_prefix.text.contains("Earlier sentence. Another complete sentence."));
    assert_eq!(plan.sentence, "This unfinished continues here.");
}

#[test]
fn rejects_ambiguous_source_and_empty_suffix() {
    let (left, right) = fixture();
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    for value in ["Prof. Smith studied. This unfinished", "A value 3.14 was measured. This unfinished", "Earlier sentence. A token {unknown} unfinished", "Earlier sentence. A link <unknown> unfinished"] {
        let mut candidate = left.clone(); candidate.text = format!("{{v7}}<s1>{value}</s1>");
        assert!(cross_page_sentence::prepare(&candidate, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().is_none(), "{value}");
    }
    let mut empty = right.clone(); empty.text = "<s2>continues here. </s2>{v7}".into();
    assert!(cross_page_sentence::prepare(&left, &empty, &[], &[], true, true, &patterns, &mut regex).unwrap().is_none());
    let mut plain = right.clone(); plain.text = "continues here. Another sentence {v7}".into(); plain.placeholders.remove(0);
    assert!(cross_page_sentence::prepare(&left, &plain, &[], &[], true, true, &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn rejects_missing_and_duplicate_structural_outputs() {
    let (left, right) = fixture();
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    let plan = cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().unwrap();
    for (first, sentence, last) in [
        ("", "完成。", "<s2>后文</s2>{v7}"),
        ("{v7}<s1>前文</s1>", "", "<s2>后文</s2>{v7}"),
        ("{v7}<s1>前文</s1>", "。", "<s2>后文</s2>{v7}"),
        ("{v7}{v7}<s1>前文</s1>", "完成。", "<s2>后文</s2>{v7}"),
        ("{v7}<s1>前文</s1>", "完成。", "<s2>后文</s2>{v7}{v7}"),
    ] {
        assert!(cross_page_sentence::assemble(&plan, first, sentence, last, &patterns, &mut regex).unwrap().is_none());
    }
}

#[test]
fn plain_left_after_formula_keeps_formula_on_original_page() {
    let (mut left, right) = fixture();
    left.text = "Earlier text {v7}. A continuous sentence".into();
    left.placeholders.retain(|placeholder| matches!(placeholder, InputPlaceholder::Formula(_)));
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    let plan = cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(plan.left_prefix.text, "Earlier text {v7}. ");
    assert_eq!(plan.sentence, "A continuous sentence continues here.");
    assert_eq!(plan.left_prefix.placeholders, left.placeholders);
    assert!(cross_page_sentence::assemble(&plan, "前文 {v7}。", "完整句子。", "<s2>后文</s2>{v7}", &patterns, &mut regex).unwrap().is_some());
    assert!(cross_page_sentence::assemble(&plan, "前文。", "完整句子。", "<s2>后文</s2>{v7}", &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn rejects_ambiguous_plain_left_after_formula() {
    let (mut left, right) = fixture();
    left.placeholders.retain(|placeholder| matches!(placeholder, InputPlaceholder::Formula(_)));
    let mut regex = RegexService::default(); let patterns = patterns(&mut regex);
    for text in ["Earlier {v7} unfinished sentence", "Earlier {v7}. Dr. Smith unfinished", "Earlier {v7}. A 3.14 value unfinished", "Earlier {v7}. A {unknown} unfinished"] {
        left.text = text.into();
        assert!(cross_page_sentence::prepare(&left, &right, &[], &[], true, true, &patterns, &mut regex).unwrap().is_none(), "{text}");
    }
}
