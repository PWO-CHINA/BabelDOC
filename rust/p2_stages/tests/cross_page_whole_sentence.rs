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
        text: "<style id='2'>11 samples were checked (Fig. </style><style id='3'>6e</style><style id='4'> and Supplementary Fig. 60).</style>".into(),
        placeholders: vec![rich(2), rich(3), rich(4)], original_tokens: vec![],
    };
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
    (left, right, regex, patterns)
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
    assert_eq!(plan.inputs.right.text, "<style id='2'>After analysis, 7 of the recovered 11 samples were checked (Fig. </style><style id='3'>6e</style><style id='4'> and Supplementary Fig. 60).</style>");
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
fn reflows_complete_numeric_translation_to_original_page_styles() {
    let (left, right, mut regex, patterns) = fixture();
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let translated_left = "<style id='1'>此前已经介绍了相关工作。</style>";
    let translated_right = "<style id='2'>分析后，回收的11份样本中有7份通过检查（图</style><style id='3'>6e</style><style id='4'>及补充图60）。</style>";
    let (first, second) = cross_page_protocol::reflow_whole_sentence_response(
        &plan, translated_left, translated_right, &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert!(first.starts_with("<style id='1'>此前已经介绍了相关工作。"));
    assert!(second.starts_with("<style id='2'>"));
    assert!(second.contains("<style id='3'>6e</style>"));
    let visible = |value: &str| regex::Regex::new(r"</?style(?: id='\d+')?>").unwrap().replace_all(value, "").into_owned();
    assert_eq!(format!("{}{}", visible(&first), visible(&second)), format!("{}{}", visible(translated_left), visible(translated_right)));
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
    let plan = cross_page_protocol::plan_whole_sentence_reflow(&left, &right, &[], &patterns, &mut regex).unwrap().unwrap();
    let result = cross_page_protocol::reflow_whole_sentence_response(
        &plan, "先前的研究结果清楚。", "跨页的句子在这里结束。下一句话完全属于本页。", &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert!(!result.0.contains("下一句话"));
    assert!(result.1.contains("下一句话完全属于本页。"));
    assert_eq!(format!("{}{}", result.0, result.1), "先前的研究结果清楚。跨页的句子在这里结束。下一句话完全属于本页。");
}
