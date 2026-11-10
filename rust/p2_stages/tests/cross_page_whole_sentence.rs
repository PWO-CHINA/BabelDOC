use babeldoc_p2_stages::translation::{
    cross_page_protocol::{self, plan_whole_sentence_on_right},
    ordinary_translation::{FormulaInput, InputPlaceholder, PlaceholderRegex, RegexMatch, RichInput, TranslateInput},
};
use serde_json::{json, Value};

#[derive(Default)]
struct RegexService(Vec<regex::Regex>);

impl PlaceholderRegex for RegexService {
    type Error = regex::Error;

    fn compile(&mut self, pattern: &str, ignore_case: bool) -> Result<u64, Self::Error> {
        self.0.push(regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build()?);
        Ok((self.0.len() - 1) as u64)
    }

    fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> {
        Ok(self.0[pattern as usize].is_match(text))
    }

    fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<RegexMatch>, Self::Error> {
        Ok(self.0[pattern as usize].find_iter(text).map(|found| RegexMatch {
            full: found.as_str().into(), start: text[..found.start()].chars().count(),
            end: text[..found.end()].chars().count(), groups: vec![],
        }).collect())
    }
}

fn rich(id: i64) -> InputPlaceholder {
    InputPlaceholder::Rich(RichInput {
        id, composition_index: id as usize, left: format!("<style id='{id}'>"), right: "</style>".into(),
        left_pattern: format!(r"<style id='{id}'>"), right_pattern: r"</style>".into(), source_text: String::new(),
    })
}

fn fixture() -> (TranslateInput, TranslateInput, RegexService, [u64; 3]) {
    let left = TranslateInput {
        text: "<style id='1'>Earlier work was described. After analysis, 7 of the recovered</style>".into(),
        placeholders: vec![rich(1)], original_tokens: vec![],
    };
    let right = TranslateInput {
        text: "<style id='2'>11 samples were checked (Fig. 6e and Supplementary Fig. 60).</style><style id='3'> Additional observations</style><style id='4'> follow.</style>".into(),
        placeholders: vec![rich(2), rich(3), rich(4)], original_tokens: vec![],
    };
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    (left, right, regex, patterns)
}

fn separate_following(input: &mut TranslateInput, following: &str) {
    let InputPlaceholder::Rich(first) = input.placeholders.first().unwrap() else { panic!("missing first style") };
    let start = input.text.find(&first.left).unwrap() + first.left.len();
    let close = start + input.text[start..].find(&first.right).unwrap();
    let split = start + input.text[start..close].find(following).unwrap();
    let suffix = input.text[split..close].to_owned();
    input.text.replace_range(split..close, "");
    let end = split + first.right.len();
    let next = rich(9);
    let InputPlaceholder::Rich(next_style) = &next else { unreachable!() };
    input.text.insert_str(end, &format!("{}{suffix}{}", next_style.left, next_style.right));
    input.placeholders.insert(1, next);
}

fn evidence(left: &mut TranslateInput, right: &mut TranslateInput) -> Value {
    let (Some(InputPlaceholder::Rich(left_rich)), Some(InputPlaceholder::Rich(right_rich))) =
        (left.placeholders.last_mut(), right.placeholders.first_mut()) else { panic!("missing rich boundary") };
    let left_text = left.text.split_once(&left_rich.left).unwrap().1.strip_suffix(&left_rich.right).unwrap();
    let right_text = right.text.strip_prefix(&right_rich.left).unwrap().split_once(&right_rich.right).unwrap().0;
    left_rich.source_text = left_text.into();
    right_rich.source_text = right_text.into();
    let page = |font: &str, text: &str, index: usize| {
        let style = json!({"font_id": font, "font_size": 10.0, "graphic_state": null});
        let mut compositions = vec![json!({}); index + 1];
        compositions[index] = json!({"pdf_same_style_characters": {
            "pdf_style": style, "pdf_character": [{"char_unicode": text, "pdf_style": style, "xobj_id": 0, "vertical": false,
                "scale": null, "visual_roundtrip_text_render_mode": null, "visual_roundtrip_text_scale_x": null, "visual_roundtrip_text_scale_y": null}]
        }});
        json!({"pdf_font": [{"font_id": font, "xref_id": 1}], "pdf_xobject": [],
            "pdf_paragraph": [{"xobj_id": 0, "pdf_paragraph_composition": compositions}]})
    };
    json!({"document": {"page": [page("left", left_text, left_rich.composition_index), page("right", right_text, right_rich.composition_index)]}})
}

#[test]
fn keeps_the_numeric_relationship_in_one_actual_input() {
    let (mut left, mut right, mut regex, patterns) = fixture();
    let document = evidence(&mut left, &mut right);
    let plan = plan_whole_sentence_on_right(&left, &right, &[], &document, (0, 0), (1, 0), &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(plan.sentence, "After analysis, 7 of the recovered 11 samples were checked (Fig. 6e and Supplementary Fig. 60).");
    assert_eq!(plan.inputs.left.text, "<style id='1'>Earlier work was described. </style>");
    assert_eq!(plan.inputs.right.text, "<style id='2'>After analysis, 7 of the recovered 11 samples were checked (Fig. 6e and Supplementary Fig. 60).</style><style id='3'> Additional observations</style><style id='4'> follow.</style>");
    assert_eq!(plan.inputs.left.placeholders, left.placeholders);
    assert_eq!(plan.inputs.right.placeholders, right.placeholders);
}

#[test]
fn accepts_normalized_spaces_when_raw_style_source_matches_the_document() {
    let (mut left, mut right, mut regex, patterns) = fixture();
    let mut document = evidence(&mut left, &mut right);
    let InputPlaceholder::Rich(left_rich) = left.placeholders.last_mut().unwrap() else { unreachable!() };
    left_rich.source_text = left_rich.source_text.replace("Earlier work", "Earlier\nwork");
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][1]["pdf_same_style_characters"]["pdf_character"][0]["char_unicode"] = json!(left_rich.source_text);
    assert!(plan_whole_sentence_on_right(&left, &right, &[], &document, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_some());
}

#[test]
fn rejects_unsafe_or_ambiguous_moves() {
    let (mut left, mut right, mut regex, patterns) = fixture();
    let document = evidence(&mut left, &mut right);
    assert!(plan_whole_sentence_on_right(&left, &right, &[], &json!({}), (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let start = left.text.find("After").unwrap();
    assert!(plan_whole_sentence_on_right(&left, &right, &[start..start + 2], &document, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    assert!(plan_whole_sentence_on_right(&left, &right, &[0..left.text.len() + 1], &document, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let mut missing_prefix = left.clone();
    let mut matching_right = right.clone();
    missing_prefix.text = "<style id='1'>After analysis, 7 of the recovered</style>".into();
    let missing_evidence = evidence(&mut missing_prefix, &mut matching_right);
    assert!(plan_whole_sentence_on_right(&missing_prefix, &matching_right, &[], &missing_evidence, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let mut hyphen = left.clone();
    hyphen.text = "<style id='1'>Earlier work was described. This develop-</style>".into();
    let mut continuation = right.clone();
    continuation.text = "<style id='2'>ment continued successfully (Fig. </style><style id='3'>6e</style><style id='4'> and Supplementary Fig. 60).</style>".into();
    let hyphen_evidence = evidence(&mut hyphen, &mut continuation);
    assert!(plan_whole_sentence_on_right(&hyphen, &continuation, &[], &hyphen_evidence, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let mut formula = right.clone();
    formula.text = formula.text.replace("11 samples", "11 {v9} samples");
    formula.placeholders.insert(1, InputPlaceholder::Formula(FormulaInput {
        id: 9, composition_index: 9, token: "{v9}".into(), pattern: r"\{v9\}".into(), source_chars: "x".into(), hint_source: false,
    }));
    let mut matching_left = left.clone();
    let formula_evidence = evidence(&mut matching_left, &mut formula);
    assert!(plan_whole_sentence_on_right(&matching_left, &formula, &[], &formula_evidence, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let mut forged = right.clone();
    forged.text = forged.text.replace("11 samples", "11 {v9} samples");
    let mut matching_left = left.clone();
    let forged_evidence = evidence(&mut matching_left, &mut forged);
    assert!(plan_whole_sentence_on_right(&matching_left, &forged, &[], &forged_evidence, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
    let mut forged_style = right.clone();
    forged_style.text = forged_style.text.replace("11 samples", "11 <style id='3'>samples");
    let mut matching_left = left.clone();
    let forged_style_evidence = evidence(&mut matching_left, &mut forged_style);
    assert!(plan_whole_sentence_on_right(&matching_left, &forged_style, &[], &forged_style_evidence, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn incomplete_fragments_never_claim_a_complete_sentence() {
    let (mut left, mut right, mut regex, patterns) = fixture();
    left.text = "<style id='1'>Earlier work was described. After analysis, 7 of the recovered</style>".into();
    right.text = "<style id='2'>11 samples were checked successfully and </style><style id='3'>returned</style><style id='4'> to</style>".into();
    let document = evidence(&mut left, &mut right);
    assert!(cross_page_protocol::validate_response(&mut regex, &left, &left.text, &patterns).unwrap());
    assert!(cross_page_protocol::validate_response(&mut regex, &right, &right.text, &patterns).unwrap());
    assert!(plan_whole_sentence_on_right(&left, &right, &[], &document, (0, 0), (1, 0), &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn refuses_numeric_translation_without_a_first_style_boundary() {
    let (left, right, mut regex, patterns) = fixture();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let translated_left = "<style id='1'>此前已经介绍了相关工作。</style>";
    let translated_right = "<style id='2'>分析后，回收的11份样本中有7份通过检查（图</style><style id='3'>6e</style><style id='4'>及补充图60）。</style>";
    let result = cross_page_protocol::reflow_whole_sentence_response(
        &plan, translated_left, translated_right, &left, &right, &patterns, &mut regex,
    ).unwrap();
    assert!(result.is_none());
    assert!(cross_page_protocol::reflow_whole_sentence_response(
        &plan, translated_left, "<style id='2'>错误的译文</style>", &left, &right, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn reflow_refuses_unsplittable_first_style_and_invented_formula() {
    let (left, right, mut regex, patterns) = fixture();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let prefix = "<style id='1'>此前的工作。</style>";
    for translated in [
        "<style id='2'>UnbreakableEnglishWord</style><style id='3'>6e</style><style id='4'>补充图60。</style>",
        "<style id='2'>最后{v9}译文</style><style id='3'>6e</style><style id='4'>补充图60。</style>",
    ] {
        assert!(cross_page_protocol::reflow_whole_sentence_response(&plan, prefix, translated, &left, &right, &patterns, &mut regex).unwrap().is_none());
    }
}

#[test]
fn plain_paragraphs_translate_the_complete_sentence_before_reflow() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous research improved patient outcomes. Integration of data from genomics,".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "transcriptomics and proteomics would improve clinical benefit for patients.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(plan.inputs.left.text, "Previous research improved patient outcomes. ");
    assert_eq!(plan.inputs.right.text, "Integration of data from genomics, transcriptomics and proteomics would improve clinical benefit for patients.");
    let prefix = "此前的研究改善了患者结局。";
    let sentence = "整合基因组学、转录组学和蛋白质组学的数据有助于提高患者的临床获益。";
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, prefix, sentence, &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert_eq!(format!("{first}{second}"), format!("{prefix}{sentence}"));
    assert!(first.starts_with(prefix));
    assert!(!second.is_empty());
    assert!(cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[60..65], &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn reflow_split_ignores_sentences_after_the_first() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier work was described. The treatment improved outcomes across".into(), placeholders: vec![], original_tokens: vec![] };
    let mut right = TranslateInput { text: "<style id='2'>multiple independent groups. Further results follow.</style>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    separate_following(&mut right, " Further");
    let sentence = "The treatment improved outcomes across multiple independent groups.";
    let plan = cross_page_protocol::plan_plain_to_styled_sentence(&left, &right, &[], sentence, &patterns, &mut regex).unwrap().unwrap();
    let first_sentence = "Lane (see Fig. 2) reported significant improvements across multiple groups.";
    let mut splits = Vec::new();
    for following in [" Another result.", " A much longer subsequent result describes unrelated findings and adds enough words to alter the target considerably."] {
        let output = format!("<style id='2'>{first_sentence}</style><style id='9'>{following}</style>");
        let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
            &plan, "Earlier work was described.", &output, &left, &right, &patterns, &mut regex,
        ).unwrap().unwrap();
        assert_eq!(format!("{first}{}", second.replace("<style id='2'>", "").replace("<style id='9'>", "").replace("</style>", "")), format!("Earlier work was described. {first_sentence}{following}"));
        splits.push(first);
    }
    assert_eq!(splits[0], splits[1]);
    assert!(splits[0].contains("Lane (see Fig. 2)"));
}

#[test]
fn reflow_rejects_first_style_without_sentence_ending() {
    let (left, right, mut regex, patterns) = fixture();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let result = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "<style id='1'>Earlier work was described.</style>",
        "<style id='2'>Lane (see Fig. 2) reported significant improvements across groups</style><style id='3'>6e</style><style id='4'> and further findings.</style>",
        &left, &right, &patterns, &mut regex,
    ).unwrap();
    assert!(result.is_none());
}

#[test]
fn reflow_rejects_missing_boundary_across_multiple_styles() {
    let (left, mut right, mut regex, patterns) = fixture();
    right.text = "<style id='2'>11 samples were checked successfully.</style><style id='3'> Another finding belongs</style><style id='4'> here.</style>".into();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let response = "<style id='2'>A later finding was moved ahead of the continued sentence</style><style id='3'> and the sentence ends.</style><style id='4'> More text stays here.</style>";
    assert!(cross_page_protocol::reflow_whole_sentence_response(
        &plan, "<style id='1'>Earlier work was described.</style>", response,
        &left, &right, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn declines_when_source_sentence_ends_in_a_later_style() {
    let (mut left, mut right, mut regex, patterns) = fixture();
    left.text = "<style id='1'>Earlier work was described. The treatment improved outcomes across</style>".into();
    right.text = "<style id='2'>multiple independent</style><style id='3'> groups.</style><style id='4'> Another finding follows.</style>".into();
    assert!(cross_page_protocol::plan_whole_sentence_reflow(
        &left, &right, &[], &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn reflow_rejects_translated_boundary_before_later_styles() {
    let (left, mut right, mut regex, patterns) = fixture();
    right.text = "<style id='2'>11 samples were checked successfully.</style><style id='3'> Another finding belongs</style><style id='4'> here.</style>".into();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let response = "<style id='2'>Another finding was moved first. The continued sentence ends here.</style><style id='3'> Additional finding</style><style id='4'> remains.</style>";
    assert!(cross_page_protocol::reflow_whole_sentence_response(
        &plan, "<style id='1'>Earlier work was described.</style>", response,
        &left, &right, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn reflow_preserves_numeric_tokens_within_the_translated_sentence() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier findings were clear. After analysis, 7 of the recovered".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "11 samples were checked successfully.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let valid = "检查后，7份来自回收的11份样本，通过了检查。";
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "先前结果明确。", valid, &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert_eq!(format!("{first}{second}"), format!("先前结果明确。{valid}"));
    assert!(first.contains('7'));
    assert!(second.contains("11"));
    for response in ["检查后，回收的11份样本通过。", "检查后，回收的12份样本中有7份通过。", "检查后，回收的11份样本中有7份和7份通过。", "检查后，回收的11份样本中有7份通过。"] {
        assert!(cross_page_protocol::reflow_whole_sentence_response(
            &plan, "先前结果明确。", response, &left, &right, &patterns, &mut regex,
        ).unwrap().is_none());
    }
}

#[test]
fn reflow_preserves_unicode_numbers_on_their_original_page() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier findings were clear. After analysis, ７ of the recovered".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "samples were checked successfully.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "先前结果明确。", "分析后７份回收样本通过了检查。", &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert!(first.contains('７'));
    assert!(!second.contains('７'));
    assert!(cross_page_protocol::reflow_whole_sentence_response(
        &plan, "先前结果明确。", "分析后7份回收样本通过了检查。", &left, &right, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn joins_plain_and_styled_boundaries_only_for_the_verified_sentence() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let formula = |id| InputPlaceholder::Formula(FormulaInput {
        id, composition_index: id as usize, token: format!("{{v{id}}}"), pattern: format!(r"\{{v{id}\}}"), source_chars: "x".into(), hint_source: false,
    });
    let left = TranslateInput {
        text: "Prior results showed improvements{v1}. Although it remains uncertain, treatment has potential for circadian".into(),
        placeholders: vec![formula(1)], original_tokens: vec![],
    };
    let mut right = TranslateInput {
        text: "<style id='2'>disorders and associated conditions. The next finding is supported</style>{v3}.".into(),
        placeholders: vec![rich(2), formula(3)], original_tokens: vec![],
    };
    separate_following(&mut right, " The next");
    let start = left.text.find("{v1}").unwrap();
    let ranges = [start..start + 4];
    let sentence = "Although it remains uncertain, treatment has potential for circadian disorders and associated conditions.";
    let plan = cross_page_protocol::plan_plain_to_styled_sentence(&left, &right, &ranges, sentence, &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(plan.inputs.right.text, "<style id='2'>Although it remains uncertain, treatment has potential for circadian disorders and associated conditions.</style><style id='9'> The next finding is supported</style>{v3}.");
    let left_output = "此前的结果显示改善{v1}。";
    let right_output = "<style id='2'>虽然仍不确定，该疗法有望治疗昼夜节律障碍及相关疾病。</style><style id='9'>下一发现得到支持</style>{v3}。";
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, left_output, right_output, &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert!(!first.contains("<style"));
    assert!(second.starts_with("<style id='2'>"));
    assert!(second.contains("</style>{v3}。"));
    assert_eq!(format!("{first}{}", second.replace("<style id='2'>", "").replace("<style id='9'>", "").replace("</style>", "")),
        format!("{left_output}{}", right_output.replace("<style id='2'>", "").replace("<style id='9'>", "").replace("</style>", "")));
    assert!(cross_page_protocol::plan_plain_to_styled_sentence(
        &left, &right, &ranges, "A different sentence ends here.", &patterns, &mut regex,
    ).unwrap().is_none());
    assert!(cross_page_protocol::plan_plain_to_styled_sentence(
        &left, &right, &[start..left.text.len()], sentence, &patterns, &mut regex,
    ).unwrap().is_none());
    let mut broken = right.clone();
    broken.text = broken.text.replace("associated conditions.", "associated</style><style id='4'> conditions.");
    broken.placeholders.insert(1, rich(4));
    assert!(cross_page_protocol::plan_plain_to_styled_sentence(
        &left, &broken, &ranges, sentence, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn reflow_declines_formulas_or_unknown_tokens_in_the_cross_page_sentence() {
    let (left, mut right, mut regex, patterns) = fixture();
    right.text = right.text.replace("11 samples", "11 {v9} samples");
    right.placeholders.insert(1, InputPlaceholder::Formula(FormulaInput {
        id: 9, composition_index: 9, token: "{v9}".into(), pattern: r"\{v9\}".into(), source_chars: "x".into(), hint_source: false,
    }));
    assert!(cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().is_none());
    right.placeholders.remove(1);
    assert!(cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn reflow_does_not_move_following_sentence_to_the_previous_page() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous findings were clear. A continuation starts with several words from the previous".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "following page. The next sentence belongs entirely to this page.".into(), placeholders: vec![], original_tokens: vec![] };
    assert!(cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn reflow_does_not_move_the_sentence_after_a_short_final_word() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous findings were clear. A continuation starts with several words from the previous".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "following page.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let next_sentence = " Another sentence contains unrelated findings and must remain entirely on the following page.";
    for final_word in ["end", "use", "low", "ms"] {
        let first_sentence = format!("The first translated sentence ends with {final_word}.");
        let result = cross_page_protocol::reflow_whole_sentence_response(
            &plan, "Earlier findings were clear.", &format!("{first_sentence}{next_sentence}"), &left, &right, &patterns, &mut regex,
        ).unwrap();
        assert!(result.is_none(), "final word: {final_word}");
    }
}

#[test]
fn reflow_stops_at_quoted_multilingual_and_decimal_sentence_boundaries() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous findings were clear. A continuation starts with several words from the previous".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "following page.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    for (first_sentence, next_sentence) in [
        ("The first translated sentence ends here.\"", " Another sentence contains unrelated findings."),
        ("The first translated sentence ends here.", "下一句话应留在本页。"),
        ("The first translated sentence ends here؟", " Another sentence contains unrelated findings."),
        ("The first translated sentence ends here۔", " Another sentence contains unrelated findings."),
        ("The first translated sentence ends here…", " Another sentence contains unrelated findings."),
        ("यह पहला वाक्य यहीं समाप्त होता है।", " यह अगला वाक्य अगले पृष्ठ पर रहना चाहिए।"),
        ("यह पहला वाक्य यहीं समाप्त होता है॥", " यह अगला वाक्य अगले पृष्ठ पर रहना चाहिए॥"),
        ("The first translated sentence ends here\u{104b}", " Another sentence contains unrelated findings."),
        ("The first translated sentence ends here։", " Another sentence contains unrelated findings."),
        ("The first translated sentence ends here።", " Another sentence contains unrelated findings."),
        ("The measured value was 3.14 across both groups.", " Another sentence contains unrelated findings."),
    ] {
        let without_next = cross_page_protocol::reflow_whole_sentence_response(
            &plan, "Earlier findings were clear.", first_sentence, &left, &right, &patterns, &mut regex,
        ).unwrap();
        let with_next = cross_page_protocol::reflow_whole_sentence_response(
            &plan, "Earlier findings were clear.", &format!("{first_sentence}{next_sentence}"), &left, &right, &patterns, &mut regex,
        ).unwrap();
        if !first_sentence.ends_with('"') { assert!(without_next.is_some(), "first sentence: {first_sentence}"); }
        assert!(with_next.is_none(), "next sentence: {next_sentence}");
    }
}

#[test]
fn reflow_preserves_the_following_sentence_in_a_styled_output() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier work was described. The treatment improved outcomes across".into(), placeholders: vec![], original_tokens: vec![] };
    let mut right = TranslateInput { text: "<style id='2'>multiple independent groups. Further results follow.</style>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    separate_following(&mut right, " Further");
    let sentence = "The treatment improved outcomes across multiple independent groups.";
    let plan = cross_page_protocol::plan_plain_to_styled_sentence(&left, &right, &[], sentence, &patterns, &mut regex).unwrap().unwrap();
    let first_sentence = "The first translated sentence has come to an end.";
    let next_sentence = " Another sentence contains unrelated findings and remains on this page.";
    let output = format!("<style id='2'>{first_sentence}</style><style id='9'>{next_sentence}</style>");
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "Earlier work was described.", &output, &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert!(!first.contains("Another sentence"));
    assert!(second.contains(next_sentence));
    assert_eq!(format!("{first}{}", second.replace("<style id='2'>", "").replace("<style id='9'>", "").replace("</style>", "")),
        format!("Earlier work was described. {first_sentence}{next_sentence}"));
}

#[test]
fn reflow_declines_missing_boundary_in_a_complete_first_style() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier work was described. The treatment improved outcomes across".into(), placeholders: vec![], original_tokens: vec![] };
    let mut right = TranslateInput { text: "<style id='2'>multiple independent groups. Further results follow.</style>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    separate_following(&mut right, " Further");
    let sentence = "The treatment improved outcomes across multiple independent groups.";
    let plan = cross_page_protocol::plan_plain_to_styled_sentence(&left, &right, &[], sentence, &patterns, &mut regex).unwrap().unwrap();
    let response = "<style id='2'>The first sentence lost its punctuation</style><style id='9'>Another unrelated sentence follows</style>";
    assert!(cross_page_protocol::reflow_whole_sentence_response(
        &plan, "Earlier work was described.", response, &left, &right, &patterns, &mut regex,
    ).unwrap().is_none());
}

#[test]
fn reflow_rejects_an_early_later_sentence_in_the_same_style() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Earlier work was described. The treatment improved outcomes across".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "<style id='2'>multiple independent groups. A later finding follows.</style>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    let sentence = "The treatment improved outcomes across multiple independent groups.";
    assert!(cross_page_protocol::plan_plain_to_styled_sentence(&left, &right, &[], sentence, &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn reflow_handles_a_long_whitespace_run() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous findings were clear. A continuation starts with several words from the previous".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "following page.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let translation = format!("alpha{}beta. Another sentence remains on its page.", " ".repeat(16_384));
    let result = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "Earlier findings were clear.", &translation, &left, &right, &patterns, &mut regex,
    ).unwrap();
    assert!(result.is_none());
}

#[test]
fn reflow_declines_ambiguous_abbreviations_and_parenthesized_sentences() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    let left = TranslateInput { text: "Previous findings were clear. A continuation starts with several words from the previous".into(), placeholders: vec![], original_tokens: vec![] };
    let right = TranslateInput { text: "following page.".into(), placeholders: vec![], original_tokens: vec![] };
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    for translated in [
        "Dr. Another sentence contains unrelated findings and must remain on the following page.",
        "Dr. Lane reported several independent observations in this study.",
        "The final title was Mr. Another sentence contains unrelated findings.",
        "The earlier researchers were listed as et al. Another finding belongs on the next page.",
        "The analysis is shown in Fig. 2 and the following observations continue.",
        "The results include e.g. several independent observations.",
        "The results compare treatment vs. controls across several groups.",
        "The translated response has no reliable sentence boundary",
        "The earlier work (First sentence ends here. Another sentence must remain on the next page.) continues.",
    ] {
        assert!(cross_page_protocol::reflow_whole_sentence_response(
            &plan, "Earlier findings were clear.", translated, &left, &right, &patterns, &mut regex,
        ).unwrap().is_none(), "translation: {translated}");
    }
}
