use babeldoc_p2_stages::translation::{cross_page_context, cross_page_protocol, llm_only, ordinary_translation::{self, FormulaInput, InputPlaceholder, ParsedPart, PlaceholderRegex, RegexMatch, RichInput, TranslateInput}};

#[test]
fn context_token_budget_counts_sentence_and_rejects_boundary_overflow() {
    let sentence_tokens = babeldoc_p2_stages::translation::token_count::count_tokens(
        &cross_page_context::read_only_sentence_block("A sentence continues on the next page."),
    );
    assert!(sentence_tokens > 0);
    assert_eq!(cross_page_context::context_token_budget(10, 20, sentence_tokens, 31 + sentence_tokens), Some(30 + sentence_tokens));
    assert_eq!(cross_page_context::context_token_budget(10, 20, sentence_tokens, 30 + sentence_tokens), None);
    assert_eq!(cross_page_context::context_token_budget(usize::MAX, 1, 0, usize::MAX), None);
    assert_eq!(cross_page_context::context_token_budget(usize::MAX - 1, 1, 1, usize::MAX), None);
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
        Ok(self.0[pattern as usize].captures_iter(text).map(|captures| {
            let found = captures.get(0).unwrap();
            RegexMatch {
            full: found.as_str().into(), start: text[..found.start()].chars().count(), end: text[..found.end()].chars().count(),
            groups: captures.iter().skip(1).map(|group| group.map(|value| (text[..value.start()].chars().count(), text[..value.end()].chars().count()))).collect(),
        }}).collect())
    }
}

#[test]
fn real_translator_placeholder_shapes_survive_validation_and_parsing() {
    for base in [false, true] {
        let (formula_token, formula_pattern, left, left_pattern, right, right_pattern, generic_formula, generic_left, generic_right) = if base {
            ("<b1>", "<b1>", "<b2>", "<b2>", "</b2>", "</b2>", r"<b\d+>", r"$^", r"</b\d+>")
        } else {
            ("{v1}", r"\{\s*v\s*1\s*\}", "<style id='2'>", r"<\s*style\s*id\s*=\s*'\s*2\s*'\s*>", "</style>", r"<\s*/\s*style\s*>", r"\{\s*v\s*\d+\s*\}", r"<\s*style\s*id\s*=\s*'\s*\d+\s*'\s*>", r"<\s*/\s*style\s*>")
        };
        let formula = InputPlaceholder::Formula(FormulaInput { id: 1, composition_index: 4, token: formula_token.into(), pattern: formula_pattern.into(), source_chars: "x".into(), hint_source: false });
        let rich = InputPlaceholder::Rich(RichInput { id: 2, composition_index: 8, left: left.into(), right: right.into(), left_pattern: left_pattern.into(), right_pattern: right_pattern.into(), source_text: "styled".into() });
        let input = TranslateInput { text: format!("Alpha {formula_token} {left}styled{right} omega"), placeholders: vec![formula, rich], original_tokens: vec![] };
        let second = TranslateInput { text: "Beta text".into(), placeholders: vec![], original_tokens: vec![] };
        let mut regex = RegexService::default();
        let patterns = [regex.compile(generic_formula, true).unwrap(), regex.compile(generic_left, true).unwrap(), regex.compile(generic_right, true).unwrap()];
        assert!(cross_page_protocol::validate_response(&mut regex, &input, &input.text, &patterns).unwrap());
        let checklist = cross_page_context::placeholder_checklist(&[input.clone(), second.clone()], &patterns, &mut regex).unwrap().unwrap();
        let expected = serde_json::to_string(&[vec![formula_token, left, right], vec![]]).unwrap();
        assert!(checklist.contains(&expected));
        let output = format!("前文 {formula_token} {left}译文{right} 后文");
        let response = serde_json::json!([{ "id": 0, "output": output }, { "id": 1, "output": "下一段" }]).to_string();
        let validated = cross_page_context::validate_pair(&response, &[input.clone(), second.clone()], &patterns, &mut regex).unwrap().unwrap();
        let mut removed = Vec::new();
        let (parts, full) = ordinary_translation::parse_output(&mut regex, &input, &validated[0], &patterns, &mut removed).unwrap();
        assert!(full && removed.is_empty(), "{parts:?} {removed:?}");
        assert!(matches!(&parts[..], [ParsedPart::Text(before), ParsedPart::Formula { composition_index: 4 }, ParsedPart::Text(middle), ParsedPart::Rich { composition_index: 8, text, .. }, ParsedPart::Text(after)] if before == "前文 " && middle == " " && text == "译文" && after == " 后文"), "{parts:?}");
        for damaged in [output.replace(formula_token, ""), format!("{output}{right}"), format!("{left}译文{right} {formula_token}")] {
            let response = serde_json::json!([{ "id": 0, "output": damaged }, { "id": 1, "output": "下一段" }]).to_string();
            assert!(cross_page_context::validate_pair(&response, &[input.clone(), second.clone()], &patterns, &mut regex).unwrap().is_none(), "{response}");
        }
    }
}

#[test]
fn shared_openai_closers_keep_both_rich_parts_in_order() {
    let make_rich = |id: usize| InputPlaceholder::Rich(RichInput {
        id: id as i64, composition_index: id, left: format!("<style id='{id}'>"), right: "</style>".into(),
        left_pattern: format!(r"<\s*style\s*id\s*=\s*'\s*{id}\s*'\s*>"),
        right_pattern: r"<\s*/\s*style\s*>".into(), source_text: "source".into(),
    });
    let input = TranslateInput { text: "<style id='2'>one</style> and <style id='3'>two</style>".into(), placeholders: vec![make_rich(2), make_rich(3)], original_tokens: vec![] };
    let second = TranslateInput { text: "Next".into(), placeholders: vec![], original_tokens: vec![] };
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{\s*v\s*\d+\s*\}", true).unwrap(), regex.compile(r"<\s*style\s*id\s*=\s*'\s*\d+\s*'\s*>", true).unwrap(), regex.compile(r"<\s*/\s*style\s*>", true).unwrap()];
    let output = "<style id='2'>一</style> 和 <style id='3'>二</style>";
    let checklist = cross_page_context::placeholder_checklist(&[input.clone(), second.clone()], &patterns, &mut regex).unwrap().unwrap();
    assert!(checklist.contains(r#"[["<style id='2'>","</style>","<style id='3'>","</style>"],[]]"#));
    let response = serde_json::json!([{ "id": 0, "output": output }, { "id": 1, "output": "下一段" }]).to_string();
    assert!(cross_page_context::validate_pair(&response, &[input.clone(), second.clone()], &patterns, &mut regex).unwrap().is_some());
    let mut removed = Vec::new();
    let (parts, full) = ordinary_translation::parse_output(&mut regex, &input, output, &patterns, &mut removed).unwrap();
    assert!(full && removed.is_empty());
    assert!(matches!(&parts[..], [ParsedPart::Rich { composition_index: 2, text: first, .. }, ParsedPart::Text(middle), ParsedPart::Rich { composition_index: 3, text: last, .. }] if first == "一" && middle == " 和 " && last == "二"));
    let damaged = serde_json::json!([{ "id": 0, "output": output.replacen("</style>", "", 1) }, { "id": 1, "output": "下一段" }]).to_string();
    assert!(cross_page_context::validate_pair(&damaged, &[input, second], &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn style_aliases_round_trip_and_reject_damaged_responses() {
    let make_rich = |id: usize| InputPlaceholder::Rich(RichInput {
        id: id as i64, composition_index: id, left: format!("<style id='{id}'>"), right: "</style>".into(),
        left_pattern: format!(r"<\s*style\s*id\s*=\s*'\s*{id}\s*'\s*>"),
        right_pattern: r"<\s*/\s*style\s*>".into(), source_text: "source".into(),
    });
    let formula = InputPlaceholder::Formula(FormulaInput {
        id: 1, composition_index: 1, token: "{v0}".into(), pattern: r"\{\s*v\s*0\s*\}".into(),
        source_chars: "x".into(), hint_source: false,
    });
    let inputs = [
        TranslateInput {
            text: "Before {v0} <style id='2'>one</style> and <style id='3'>two</style>".into(),
            placeholders: vec![formula, make_rich(2), make_rich(3)], original_tokens: vec![],
        },
        TranslateInput { text: "Next <style id='4'>three</style> after".into(), placeholders: vec![make_rich(4)], original_tokens: vec![] },
    ];
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{\s*v\s*\d+\s*\}", true).unwrap(), regex.compile(r"<\s*style\s*id\s*=\s*'\s*\d+\s*'\s*>", true).unwrap(), regex.compile(r"<\s*/\s*style\s*>", true).unwrap()];
    let aliases = cross_page_context::StyleAliases::prepare(&inputs, "context {v1}", &patterns, &mut regex).unwrap().unwrap();
    assert!(aliases.inputs[0].text.contains("{v2}"));
    assert!(!aliases.inputs[0].text.contains("<style"));
    assert!(!aliases.inputs[1].text.contains("</style>"));
    assert!(cross_page_context::placeholder_checklist(&aliases.inputs, &patterns, &mut regex).unwrap().is_some());
    let left = aliases.inputs[0].text.replace("Before", "之前").replace("one", "一").replace("and", "和").replace("two", "二");
    let right = aliases.inputs[1].text.replace("Next", "接着").replace("three", "三").replace("after", "之后");
    let response = |first: &str, second: &str| serde_json::json!([{"id": 0, "output": first}, {"id": 1, "output": second}]).to_string();
    let decoded = aliases.decode(&response(&left, &right), &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(cross_page_context::validate_pair(&decoded, &inputs, &patterns, &mut regex).unwrap(), Some([
        "之前 {v0} <style id='2'>一</style> 和 <style id='3'>二</style>".into(),
        "接着 <style id='4'>三</style> 之后".into(),
    ]));
    for damaged in [
        left.replace("{v2}", ""),
        left.replace("{v2}", "{v3}"),
        format!("{left}{{v9999}}"),
        left.replace("{v2}一{v3}", "{v2}{v3}"),
    ] {
        assert!(aliases.decode(&response(&damaged, &right), &patterns, &mut regex).unwrap().is_none());
    }
    let swapped = response(&right, &left);
    assert!(aliases.decode(&swapped, &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn three_independent_items_reject_swapped_or_missing_following_text() {
    let (inputs, mut regex, patterns) = fixture();
    let third = TranslateInput { text: "A later sentence remains on this page.".into(), placeholders: vec![], original_tokens: vec![] };
    let items = [inputs[0].clone(), inputs[1].clone(), third];
    let aliases = cross_page_context::StyleAliases::prepare(&items, "context", &patterns, &mut regex).unwrap().unwrap();
    let response = serde_json::json!([
        {"id": 2, "output": "后续句子。"},
        {"id": 1, "output": aliases.inputs[1].text},
        {"id": 0, "output": aliases.inputs[0].text},
    ]).to_string();
    let decoded = aliases.decode(&response, &patterns, &mut regex).unwrap().unwrap();
    assert!(cross_page_context::validate_items(&decoded, &items, &patterns, &mut regex).unwrap().is_some());
    let missing = serde_json::json!([
        {"id": 0, "output": aliases.inputs[0].text},
        {"id": 1, "output": aliases.inputs[1].text},
    ]).to_string();
    assert!(aliases.decode(&missing, &patterns, &mut regex).unwrap().is_none());
    let swapped = serde_json::json!([
        {"id": 0, "output": aliases.inputs[0].text},
        {"id": 1, "output": "后续句子。"},
        {"id": 2, "output": aliases.inputs[1].text},
    ]).to_string();
    assert!(aliases.decode(&swapped, &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn style_aliases_require_compatible_formula_tokens() {
    let (inputs, mut regex, _) = fixture();
    let inputs = [inputs[1].clone(), TranslateInput { text: "Other paragraph".into(), placeholders: vec![], original_tokens: vec![] }];
    let patterns = [regex.compile(r"<b\d+>", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    assert!(cross_page_context::StyleAliases::prepare(&inputs, "context", &patterns, &mut regex).unwrap().is_none());
}

fn fixture() -> ([TranslateInput; 2], RegexService, [u64; 3]) {
    let formula = InputPlaceholder::Formula(FormulaInput { id: 7, composition_index: 4, token: "{v7}".into(), pattern: r"\{v7\}".into(), source_chars: "x".into(), hint_source: false });
    let rich = InputPlaceholder::Rich(RichInput { id: 2, composition_index: 8, left: "<s2>".into(), right: "</s2>".into(), left_pattern: "<s2>".into(), right_pattern: "</s2>".into(), source_text: "continuation".into() });
    let inputs = [
        TranslateInput { text: "Earlier {v7} continues".into(), placeholders: vec![formula], original_tokens: vec![] },
        TranslateInput { text: "<s2>across pages.</s2>".into(), placeholders: vec![rich], original_tokens: vec![] },
    ];
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    (inputs, regex, patterns)
}

#[test]
fn context_prompt_preserves_normal_prompt() {
    let normal = llm_only::render_batch_prompt("role", "Chinese", "[]", "", None, None, &[]);
    let context = cross_page_context::render_context_batch_prompt("role", "Chinese", "[]", "", None, None, &[]);
    assert!(normal.contains("independent, fixed unit"));
    assert!(!context.contains("independent, fixed unit"));
    assert!(context.contains("## Cross-page continuation"));
    assert!(context.contains("continuing across a page boundary"));
    assert!(context.contains("Use sentence-level context, while keeping each paragraph's content and placeholders in its original output"));
    assert!(normal.contains("Inside each paragraph, you may adjust word order for fluency"));
    assert!(context.contains("Keep the same \"id\""));
}

#[test]
fn read_only_sentence_is_json_escaped_and_does_not_change_input_ids() {
    let input = r#"[{"id":0,"input":"first"},{"id":1,"input":"second"}]"#;
    let prompt = cross_page_context::render_context_batch_prompt("role", "Chinese", input, "", None, None, &[]);
    let sentence = "A \"quoted\" café path \\ continues across pages.";
    let augmented = cross_page_context::add_read_only_sentence(&prompt, Some(sentence));
    let (_, context) = augmented.split_once("## Read-only cross-page sentence context\n").unwrap();
    let (context, original_input) = context.split_once("\n\n## Here is the input:\n\n").unwrap();
    assert_eq!(serde_json::from_str::<String>(context.lines().last().unwrap()).unwrap(), sentence);
    assert!(context.contains("not an input item"));
    assert!(context.contains("Do not copy the complete sentence verbatim"));
    assert_eq!(cross_page_context::add_read_only_sentence(&prompt, None), prompt);
    assert_eq!(original_input, input);
    assert_eq!(serde_json::from_str::<serde_json::Value>(original_input).unwrap().as_array().unwrap().len(), 2);
    assert_eq!(cross_page_context::add_read_only_sentence("no marker", Some(sentence)), "no marker");
}

#[test]
fn extracted_sentence_enters_prompt_as_a_single_context_value() {
    let left = "Earlier result was clear. The treatment could be useful for";
    let right = "circadian disorders and related conditions. Another observation follows.";
    let sentence = babeldoc_p2_stages::translation::cross_page::extract_context_sentence(left, right).unwrap();
    let prompt = cross_page_context::add_read_only_sentence("## Here is the input:\n\n[]", Some(&sentence));
    let context = prompt.split_once("\n\n## Here is the input:").unwrap().0;
    let actual = serde_json::from_str::<String>(context.lines().last().unwrap()).unwrap();
    assert_eq!(actual, "The treatment could be useful for circadian disorders and related conditions.");
    assert_eq!(cross_page_context::add_read_only_sentence("original", babeldoc_p2_stages::translation::cross_page::extract_context_sentence(left, "no terminal punctuation").as_deref()), "original");
}

#[test]
fn checklist_precedes_input_and_preserves_terminal_json() {
    let json = r###"[{"id":0,"input":"## Here is the input: $value"},{"id":1,"input":"next"}]"###;
    let checklist = "\n## Placeholder checklist\n[[\"$value\"],[]]";
    let normal = llm_only::render_batch_prompt("role", "Chinese", json, "", None, None, &[]);
    let prompt = cross_page_context::render_context_batch_prompt_with_checklist("role", "Chinese", json, "", None, None, &[], checklist);
    let marker = "## Here is the input:\n\n";
    let marker_position = prompt.find(marker).unwrap();
    assert!(prompt.find("## Placeholder checklist").unwrap() < marker_position);
    assert!(prompt.contains("[[\"$value\"],[]]"));
    assert_eq!(&prompt[marker_position + marker.len()..], json);
    assert!(serde_json::from_str::<serde_json::Value>(&prompt[marker_position + marker.len()..]).is_ok());
    assert!(!normal.contains("## Placeholder checklist"));
    assert!(normal.ends_with(json));
}

#[test]
fn complete_sentence_prompt_identifies_the_actual_input_unit() {
    let json = r#"[{"id":0,"input":"Previous."},{"id":1,"input":"A complete sentence."}]"#;
    let prompt = cross_page_context::render_whole_sentence_batch_prompt_with_checklist("role", "Chinese", json, "", None, None, &[], "checklist");
    assert!(prompt.contains("The second input starts with a complete sentence"));
    assert!(!prompt.contains("The two input paragraphs contain a sentence continuing"));
    assert!(prompt.ends_with(json));
}

#[test]
fn accepts_reordered_outputs_and_preserves_placeholders() {
    let (inputs, mut regex, patterns) = fixture();
    let response = r#"[{"id":1,"output":"<s2>跨越页面。</s2>"},{"id":0,"output":"之前{v7}继续"}]"#;
    assert_eq!(cross_page_context::validate_pair(response, &inputs, &patterns, &mut regex).unwrap(), Some(["之前{v7}继续".into(), "<s2>跨越页面。</s2>".into()]));
}

#[test]
fn retry_decision_is_single_context_only_and_never_includes_response() {
    use cross_page_context::{should_retry, RETRY_INSTRUCTION};
    assert!(should_retry(true, true, false, false, false));
    for args in [(false, true, false, false, false), (true, false, false, false, false),
        (true, true, true, false, false), (true, true, false, true, false),
        (true, true, false, false, true)] {
        assert!(!should_retry(args.0, args.1, args.2, args.3, args.4));
    }
    let (inputs, mut regex, patterns) = fixture();
    let damaged = r#"[{"id":0,"output":"BAD_RESPONSE_SECRET"},{"id":1,"output":"<s2>后文</s2>"}]"#;
    let valid = r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"<s2>后文</s2>"}]"#;
    let prompt = "original prompt";
    let retry_prompt = format!("{RETRY_INSTRUCTION}{prompt}");
    assert!(!retry_prompt.contains("BAD_RESPONSE_SECRET"));
    for (first, second, expected_calls, success) in [(valid, valid, 1, true), (damaged, valid, 2, true), (damaged, damaged, 2, false)] {
        let first_valid = cross_page_context::validate_pair(first, &inputs, &patterns, &mut regex).unwrap().is_some();
        let retry = should_retry(true, true, first_valid, false, false);
        let final_valid = if retry { cross_page_context::validate_pair(second, &inputs, &patterns, &mut regex).unwrap().is_some() } else { first_valid };
        assert_eq!(1 + usize::from(retry), expected_calls);
        assert_eq!(final_valid, success);
    }
}

#[test]
fn accepts_only_optional_string_or_null_layout_label() {
    let (inputs, mut regex, patterns) = fixture();
    for response in [
        r#"[{"id":0,"output":"前文{v7}","layout_label":"text"},{"id":1,"output":"<s2>后文</s2>","layout_label":null}]"#,
        r#"[{"id":1,"output":"<s2>后文</s2>"},{"id":0,"output":"前文{v7}","layout_label":"other"}]"#,
    ] {
        assert!(cross_page_context::validate_pair(response, &inputs, &patterns, &mut regex).unwrap().is_some(), "{response}");
    }
    for response in [
        r#"[{"id":0,"output":"前文{v7}","layout_label":1},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":true},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":{}},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":[]},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":null,"layout_label":"text"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":"text","layout_\u006cabel":"text"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":"text","input":"extra"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文","layout_label":"text"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}","layout_label":"text"},{"id":0,"output":"<s2>后文</s2>"}]"#,
    ] {
        assert!(cross_page_context::validate_pair(response, &inputs, &patterns, &mut regex).unwrap().is_none(), "{response}");
    }
}

#[test]
fn checklist_preserves_order_duplicates_and_rejects_invalid_or_long_input() {
    let (inputs, mut regex, patterns) = fixture();
    let checklist = cross_page_context::placeholder_checklist(&inputs, &patterns, &mut regex).unwrap().unwrap();
    assert!(checklist.contains(r#"[["{v7}"],["<s2>","</s2>"]]"#));
    let empty = TranslateInput { text: "plain".into(), placeholders: vec![], original_tokens: vec![] };
    assert!(cross_page_context::placeholder_checklist(&[empty.clone(), empty.clone()], &patterns, &mut regex).unwrap().unwrap().contains("[[],[]]"));
    let repeated = TranslateInput { text: "{v9} and {v9}".into(), placeholders: vec![], original_tokens: vec![("{v9}".into(), 2)] };
    assert!(cross_page_context::placeholder_checklist(&[repeated, empty.clone()], &patterns, &mut regex).unwrap().unwrap().contains(r#"[["{v9}","{v9}"],[]]"#));
    let mut invalid = inputs.clone();
    invalid[0].text = "missing".into();
    assert!(cross_page_context::placeholder_checklist(&invalid, &patterns, &mut regex).unwrap().is_none());
    let long_token = format!("{{v{}}}", "1".repeat(8300));
    let long = TranslateInput { text: long_token.clone(), placeholders: vec![], original_tokens: vec![(long_token, 1)] };
    assert!(cross_page_context::placeholder_checklist(&[long, empty], &patterns, &mut regex).unwrap().is_none());
}

#[test]
fn checklist_escapes_literals_and_rejects_overlapping_patterns() {
    let token = "<tag=\"x\\y\">";
    let input = TranslateInput { text: token.into(), placeholders: vec![], original_tokens: vec![(token.into(), 1)] };
    let empty = TranslateInput { text: "plain".into(), placeholders: vec![], original_tokens: vec![] };
    let mut regex = RegexService::default();
    let patterns = [regex.compile(&regex::escape(token), false).unwrap(), regex.compile("NEVER", false).unwrap(), regex.compile("OTHER", false).unwrap()];
    let checklist = cross_page_context::placeholder_checklist(&[input.clone(), empty.clone()], &patterns, &mut regex).unwrap().unwrap();
    assert!(checklist.contains(&serde_json::to_string(&[vec![token], vec![]]).unwrap()));
    let overlapping = [patterns[0], regex.compile("tag", false).unwrap(), patterns[2]];
    assert!(cross_page_context::placeholder_checklist(&[input, empty], &overlapping, &mut regex).unwrap().is_none());
}

#[test]
fn rejects_invalid_pair_atomically() {
    let (inputs, mut regex, patterns) = fixture();
    for response in [
        "[]", "{}", "```json\n[]\n```", r#"[{"id":0,"output":"a"}]"#,
        r#"[{"id":0,"output":"a"},{"id":0,"output":"b"}]"#,
        r#"[{"id":0,"output":"a"},{"id":2,"output":"b"}]"#,
        r#"[{"id":-1,"output":"a"},{"id":1,"output":"b"}]"#,
        r#"[{"id":true,"output":"a"},{"id":1,"output":"b"}]"#,
        r#"[{"id":0,"id":0,"output":"a"},{"id":1,"output":"b"}]"#,
        r#"[{"id":0,"output":"a","extra":1},{"id":1,"output":"b"}]"#,
        r#"[{"id":0,"output":1},{"id":1,"output":"b"}]"#,
        r#"[{"id":0,"output":" "},{"id":1,"output":"b"}]"#,
        r#"[{"id":0,"output":"前文"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"<s2> </s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"<s2>后文</s2>{v7}"}]"#,
    ] {
        assert_eq!(cross_page_context::validate_pair(response, &inputs, &patterns, &mut regex).unwrap(), None, "{response}");
    }
}

#[test]
fn rejects_missing_readable_content_without_fixing_text_segments() {
    let (inputs, mut regex, patterns) = fixture();
    for response in [
        r#"[{"id":0,"output":"{v7}"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"\u200b{v7}。"},{"id":1,"output":"<s2>后文</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"<s2>。</s2>"}]"#,
        r#"[{"id":0,"output":"前文{v7}"},{"id":1,"output":"<s2>\u200b</s2>"}]"#,
    ] {
        assert!(cross_page_context::validate_pair(response, &inputs, &patterns, &mut regex).unwrap().is_none());
    }
    let reordered = r#"[{"id":0,"output":"{v7}译文"},{"id":1,"output":"<s2>译文</s2>"}]"#;
    assert!(cross_page_context::validate_pair(reordered, &inputs, &patterns, &mut regex).unwrap().is_some());
}
