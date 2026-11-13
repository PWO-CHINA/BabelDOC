use babeldoc_p2_stages::translation::{cross_boundary::{build_plan, reflow, reflow_with_cancellation, select, Decision, Join, Location, SelectedPart, Source}, ordinary_translation::{FormulaInput, InputPlaceholder, PlaceholderRegex, RegexMatch, RichInput, TranslateInput}};
use std::collections::HashSet;

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
        Ok(self.0[pattern as usize].find_iter(text).map(|found| RegexMatch { full: found.as_str().into(), start: text[..found.start()].chars().count(), end: text[..found.end()].chars().count(), groups: vec![] }).collect())
    }
}

#[test]
fn colliding_formula_and_style_tokens_keep_object_identity() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\}", false).unwrap(), regex.compile("<s>", false).unwrap(), regex.compile("</s>", false).unwrap()];
    let input = TranslateInput { text: "<s>word</s>{v}".into(), placeholders: vec![
        InputPlaceholder::Rich(RichInput { id: 0, composition_index: 0, left: "<s>".into(), right: "</s>".into(), left_pattern: "<s>".into(), right_pattern: "</s>".into(), source_text: "word".into() }),
        InputPlaceholder::Formula(FormulaInput { id: 0, composition_index: 1, token: "{v}".into(), pattern: r"\{v\}".into(), source_chars: "x".into(), hint_source: false }),
    ], original_tokens: vec![] };
    let parts = (0..2).map(|paragraph| {
        let source = Source::new(Location { page: 0, paragraph }, &input, &patterns, &mut regex).unwrap().unwrap();
        let end = source.text.len();
        SelectedPart { source, range: 0..end }
    }).collect();
    let plan = build_plan(parts, vec![Join::Space]).unwrap();
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output[0].1.trim(), input.text);
    assert_eq!(output[1].1.trim(), input.text);
    let mut bad = plan.items.clone();
    bad[0] = bad[0].replace("_0_1_F]]", "_1_1_F]]");
    assert!(reflow(&plan, &bad).is_err());
}

#[test]
fn selects_and_reflows_three_objects_and_rejects_unsafe_responses() {
    let mut regex = RegexService::default();
    let pattern = regex.compile("never-matches", false).unwrap();
    let sources = ["Earlier. The value", "is +3.5%", "today. Later."].iter().enumerate().map(|(paragraph, text)| {
        Source::new(Location { page: 0, paragraph }, &TranslateInput { text: (*text).into(), placeholders: vec![], original_tokens: vec![] }, &[pattern; 3], &mut regex).unwrap().unwrap()
    }).collect::<Vec<_>>();
    let decision: Decision = serde_json::from_str(r#"{"status":"selected","ranges":[{"page":0,"paragraph":0,"text":"The value"},{"page":0,"paragraph":1,"text":"is +3.5%"},{"page":0,"paragraph":2,"text":"today."}],"joins":["space","space"]}"#).unwrap();
    let left = HashSet::from([sources[0].location, sources[1].location]);
    let right = HashSet::from([sources[2].location]);
    let (parts, joins) = select(decision, &sources, &left, &right).unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    assert_eq!(plan.items.len(), 3);
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output.iter().map(|item| item.1.as_str()).collect::<String>(), "Earlier. The value is +3.5% today. Later.");
    let mut unsafe_output = plan.items.clone();
    unsafe_output[1] = unsafe_output[1].replace("+3.5%", "-3.5%");
    assert!(reflow(&plan, &unsafe_output).is_err());
    let mut whitespace = plan.items.clone();
    whitespace[0] = whitespace[0].trim_end().to_owned();
    assert_eq!(reflow(&plan, &whitespace).unwrap().iter().map(|item| item.1.as_str()).collect::<String>(), "Earlier. The value is +3.5% today. Later.");
    assert!(reflow(&plan, &[]).is_err());
    assert!(serde_json::from_str::<Decision>(r#"{"status":"none","extra":1}"#).is_err());
    let cut_number: Decision = serde_json::from_str(r#"{"status":"selected","ranges":[{"page":0,"paragraph":0,"text":"The value"},{"page":0,"paragraph":1,"text":"is +3"},{"page":0,"paragraph":2,"text":"today."}],"joins":["space","space"]}"#).unwrap();
    assert!(select(cut_number, &sources, &left, &right).is_err());
    let _ = Join::None;
}

fn plain_sources(values: &[(usize, usize, &str)]) -> Vec<Source> {
    let mut regex = RegexService::default();
    let pattern = regex.compile("never-matches", false).unwrap();
    values.iter().map(|&(page, paragraph, text)| Source::new(
        Location { page, paragraph }, &TranslateInput { text: text.into(), placeholders: vec![], original_tokens: vec![] },
        &[pattern; 3], &mut regex,
    ).unwrap().unwrap()).collect()
}

fn formula_sources(left: &str, right: &str) -> Vec<Source> {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    [(left, "{v1}", r"\{v1\}"), (right, "{v2}", r"\{v2\}")].into_iter().enumerate().map(|(page, (text, token, pattern))| {
        let placeholders = if text.contains(token) { vec![InputPlaceholder::Formula(FormulaInput {
            id: page as i64, composition_index: 0, token: token.into(), pattern: pattern.into(), source_chars: "x.".into(), hint_source: false,
        })] } else { vec![] };
        Source::new(Location { page, paragraph: 0 }, &TranslateInput { text: text.into(), placeholders, original_tokens: vec![] }, &patterns, &mut regex).unwrap().unwrap()
    }).collect()
}

fn choose(sources: &[Source], first: &str, last: &str) -> Result<Option<(Vec<SelectedPart>, Vec<Join>)>, &'static str> {
    let decision = Decision::Selected { ranges: vec![
        babeldoc_p2_stages::translation::cross_boundary::SelectionRange { page: sources[0].location.page, paragraph: sources[0].location.paragraph, text: first.into() },
        babeldoc_p2_stages::translation::cross_boundary::SelectionRange { page: sources[1].location.page, paragraph: sources[1].location.paragraph, text: last.into() },
    ], joins: vec![Join::Space] };
    select(decision, sources, &HashSet::from([sources[0].location]), &HashSet::from([sources[1].location]))
}

#[test]
fn unchanged_formula_prefix_and_suffix_keep_adjacent_whitespace() {
    for (left, right) in [
        ("Value {v1} Start", "continues."),
        ("Start", "continues. {v2} Value"),
        ("Value \t{v1}  Start", "continues.\u{a0} {v2}\t Value"),
        ("{v1} Start", "continues. {v2}"),
        ("Value {v1}Start", "continues.{v2} Value"),
    ] {
        let sources = formula_sources(left, right);
        let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
        let plan = build_plan(parts, joins).unwrap();
        let output = reflow(&plan, &plan.items).unwrap();
        assert_eq!(output[0].1, format!("{left} "));
        assert_eq!(output[1].1, right);
    }
}

#[test]
fn chinese_translation_restores_separators_around_formula_items() {
    let sources = formula_sources("Value {v1}  Start", "continues.\t {v2} Value");
    let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let responses = vec![
        plan.items[0].trim_end().replace("Value", "数值"),
        "开始继续。".into(),
        plan.items[2].trim_start().replace("Value", "数值"),
    ];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), "数值 {v1}  开始继续。\t {v2} 数值");
    assert!(output[0].1.contains("{v1}"));
    assert!(output[1].1.contains("{v2}"));
}

#[test]
fn boundary_whitespace_keeps_its_style_and_formula_neighbors() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    let rich = |id| InputPlaceholder::Rich(RichInput {
        id, composition_index: id as usize, left: format!("<s{id}>"), right: format!("</s{id}>"),
        left_pattern: format!("<s{id}>"), right_pattern: format!("</s{id}>"), source_text: String::new(),
    });
    let formula = |id, token: &str, pattern: &str| InputPlaceholder::Formula(FormulaInput {
        id, composition_index: id as usize, token: token.into(), pattern: pattern.into(), source_chars: "x.".into(), hint_source: false,
    });
    let left_input = TranslateInput {
        text: "<s1>Value  </s1>{v1} \t<s2>end  </s2> \u{a0} Start".into(),
        placeholders: vec![rich(1), formula(1, "{v1}", r"\{v1\}"), rich(2)], original_tokens: vec![],
    };
    let right_input = TranslateInput {
        text: "continues.\t <s3>  Later </s3> {v2}\t End".into(),
        placeholders: vec![rich(3), formula(2, "{v2}", r"\{v2\}")], original_tokens: vec![],
    };
    let sources = vec![
        Source::new(Location { page: 0, paragraph: 0 }, &left_input, &patterns, &mut regex).unwrap().unwrap(),
        Source::new(Location { page: 1, paragraph: 0 }, &right_input, &patterns, &mut regex).unwrap().unwrap(),
    ];
    let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let unchanged = reflow(&plan, &plan.items).unwrap();
    assert_eq!(unchanged[0].1, format!("{} ", left_input.text));
    assert_eq!(unchanged[1].1, right_input.text);
    let responses = vec![
        plan.items[0].trim_end().replace("Value", "数值").replace("end", "前文"),
        "开始继续。".into(),
        plan.items[2].trim_start().replace("Later", "后文").replace("End", "结尾"),
    ];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(),
        "<s1>数值  </s1>{v1} \t<s2>前文  </s2> \u{a0} 开始继续。\t <s3>  后文 </s3> {v2}\t 结尾");
}

#[test]
fn translated_content_can_move_past_boundary_styles_without_losing_inner_spaces() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile("never-matches", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    let rich = |id| InputPlaceholder::Rich(RichInput {
        id, composition_index: id as usize, left: format!("<s{id}>"), right: format!("</s{id}>"),
        left_pattern: format!("<s{id}>"), right_pattern: format!("</s{id}>"), source_text: String::new(),
    });
    let left = TranslateInput { text: "<s1>Earlier</s1> Start".into(), placeholders: vec![rich(1)], original_tokens: vec![] };
    let right = TranslateInput { text: "continues. <s2>Later</s2>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    let sources = vec![
        Source::new(Location { page: 0, paragraph: 0 }, &left, &patterns, &mut regex).unwrap().unwrap(),
        Source::new(Location { page: 1, paragraph: 0 }, &right, &patterns, &mut regex).unwrap().unwrap(),
    ];
    let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let responses = vec![
        format!("{} 中", plan.items[0].trim_end().replace("Earlier", "之前   ")),
        "开始继续。".into(),
        format!("尾 {}", plan.items[2].trim_start().replace("Later", "  之后")),
    ];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(),
        "<s1>之前   </s1> 中 开始继续。 尾 <s2>  之后</s2>");

    let left = TranslateInput { text: "<s1>Earlier</s1> tail Start".into(), placeholders: vec![rich(1)], original_tokens: vec![] };
    let right = TranslateInput { text: "continues. before <s2>Later</s2>".into(), placeholders: vec![rich(2)], original_tokens: vec![] };
    let sources = vec![
        Source::new(Location { page: 0, paragraph: 0 }, &left, &patterns, &mut regex).unwrap().unwrap(),
        Source::new(Location { page: 1, paragraph: 0 }, &right, &patterns, &mut regex).unwrap().unwrap(),
    ];
    let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let responses = vec![
        plan.items[0].trim_end().replace("Earlier", "之前   ").replace(" tail", ""),
        "开始继续。".into(),
        plan.items[2].trim_start().replace("before ", "").replace("Later", "  之后"),
    ];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(),
        "<s1>之前   </s1> 开始继续。 <s2>  之后</s2>");
}

#[test]
fn shared_styles_keep_boundary_spacing_next_to_formulas() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    let rich = |id| InputPlaceholder::Rich(RichInput {
        id, composition_index: id as usize, left: format!("<s{id}>"), right: format!("</s{id}>"),
        left_pattern: format!("<s{id}>"), right_pattern: format!("</s{id}>"), source_text: String::new(),
    });
    let formula = |id, token: &str, pattern: &str| InputPlaceholder::Formula(FormulaInput {
        id, composition_index: id as usize, token: token.into(), pattern: pattern.into(), source_chars: "x.".into(), hint_source: false,
    });
    let left = TranslateInput {
        text: "<s1>Value  </s1>{v1}<s2> Previous  Start</s2>".into(),
        placeholders: vec![rich(1), formula(1, "{v1}", r"\{v1\}"), rich(2)], original_tokens: vec![],
    };
    let right = TranslateInput {
        text: "<s3>continues.   Next</s3>{v2}  End".into(),
        placeholders: vec![rich(3), formula(2, "{v2}", r"\{v2\}")], original_tokens: vec![],
    };
    let sources = vec![
        Source::new(Location { page: 0, paragraph: 0 }, &left, &patterns, &mut regex).unwrap().unwrap(),
        Source::new(Location { page: 1, paragraph: 0 }, &right, &patterns, &mut regex).unwrap().unwrap(),
    ];
    let (parts, joins) = choose(&sources, "Start", "continues.").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let unchanged = reflow(&plan, &plan.items).unwrap();
    assert_eq!(unchanged[0].1, format!("{} ", left.text));
    assert_eq!(unchanged[1].1, right.text);
    let responses = vec![
        plan.items[0].replace("Value", "数值").replace("Previous  ", "前文"),
        plan.items[1].replace("Start", "开始").replace("continues.", "继续。"),
        plan.items[2].replace("   Next", "后文").replace("End", "结尾"),
    ];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output[0].1, "<s1>数值  </s1>{v1}<s2> 前文  开始</s2> ");
    assert_eq!(output[1].1, "<s3>继续。   后文</s3>{v2}  结尾");
}

#[test]
fn chinese_translation_restores_sentence_separators() {
    for (left, right, first, last, sentence) in [
        ("Earlier. \tA state-of-the-art", "system works.\u{a0} Later.", "A state-of-the-art", "system works.", "这是最先进的系统。"),
        ("Earlier. A client/server", "model works. Later.", "A client/server", "model works.", "客户端和服务器模型有效。"),
    ] {
        let sources = plain_sources(&[(0, 0, left), (1, 0, right)]);
        let (parts, joins) = choose(&sources, first, last).unwrap().unwrap();
        let plan = build_plan(parts, joins).unwrap();
        let responses = vec!["之前。   ".into(), sentence.into(), "\t之后。".into()];
        let output = reflow(&plan, &responses).unwrap();
        let prefix_space = &left["Earlier.".len()..left.find(first).unwrap()];
        let suffix_space = &right[last.len()..right.find("Later.").unwrap()];
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), format!("之前。{prefix_space}{sentence}{suffix_space}之后。"));
    }
}

#[test]
fn chinese_translation_keeps_formula_marker_and_numeric_values() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    let input = TranslateInput { text: "Earlier. A state-of-the-art {v}".into(), placeholders: vec![InputPlaceholder::Formula(FormulaInput {
        id: 0, composition_index: 0, token: "{v}".into(), pattern: r"\{v\}".into(), source_chars: "x²".into(), hint_source: false,
    })], original_tokens: vec![] };
    let left = Source::new(Location { page: 0, paragraph: 0 }, &input, &patterns, &mut regex).unwrap().unwrap();
    let right = plain_sources(&[(1, 0, "system has 3.5% growth. Later.")]).remove(0);
    let start = left.text.find("A state").unwrap();
    let end = right.text.find(" Later.").unwrap();
    let left_end = left.text.len();
    let plan = build_plan(vec![SelectedPart { source: left, range: start..left_end }, SelectedPart { source: right, range: 0..end }], vec![Join::Space]).unwrap();
    let marker = regex::Regex::new(r"\[\[BABEL_[^\]]+\]\]").unwrap().find(&plan.items[1]).unwrap().as_str();
    let responses = vec!["之前。".into(), format!("这是最先进的{marker}系统，增长3.5%。"), "之后。".into()];
    let output = reflow(&plan, &responses).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), "之前。 这是最先进的{v}系统，增长3.5%。 之后。");
    let mut damaged = responses.clone();
    damaged[1] = damaged[1].replace("3.5%", "3.6%");
    assert!(reflow(&plan, &damaged).is_err());
    damaged[1] = responses[1].replace(marker, "");
    assert!(reflow(&plan, &damaged).is_err());
}


#[test]
fn production_sentence_boundary_space_survives_typesetting_capture() {
    use babeldoc_ir_core as ir;
    use babeldoc_p2_stages::{font_mapper::font_mapper_from_asset_paths, stages::typesetting::{PreviewRejection, Typesetting}, NoopCancellation, NoopProgress, StageContext};
    let sources = plain_sources(&[(0, 0, "Alpha beta across"), (1, 0, "gamma delta continue.")]);
    let (parts, joins) = choose(&sources, &sources[0].text, &sources[1].text).unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output[0].1, "Alpha beta across ");
    assert_eq!(output[1].1, "gamma delta continue.");
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file());
    let mapper = font_mapper_from_asset_paths(&serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]}), &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1) }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let bounds = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(300.0), y2: Some(50.0) }.into_value();
        page.set_pdf_paragraph(&mut view, output.iter().map(|entry| ir::PdfParagraphInit {
            r#box: Some(bounds()), unicode: Some(entry.1.clone()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some(entry.1.clone()), pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()).collect()).unwrap();
    }
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let (previews, _) = stage.audit_fork_with_document(&document, &[(0, 0), (0, 1)], &mut context).unwrap();
    assert!(!previews[0].rejections.contains(&PreviewRejection::TextMismatch), "reflow={output:?}; preview={:?}", previews[0]);
    assert!(!previews[1].rejections.contains(&PreviewRejection::TextMismatch), "reflow={output:?}; preview={:?}", previews[1]);
    let first_text = previews[0].glyphs.iter().filter_map(|glyph| glyph.text.as_deref()).collect::<String>();
    let second_text = previews[1].glyphs.iter().filter_map(|glyph| glyph.text.as_deref()).collect::<String>();
    assert_eq!(first_text, output[0].1);
    assert_eq!(second_text, output[1].1);
}

#[test]
fn equal_distance_whitespace_stays_with_previous_object() {
    let sources = plain_sources(&[(0, 0, "one"), (0, 1, "two"), (1, 0, "three")]);
    let parts = sources.into_iter().map(|source| {
        let end = source.text.len();
        SelectedPart { source, range: 0..end }
    }).collect();
    let plan = build_plan(parts, vec![Join::Space, Join::Space]).unwrap();
    for sentence in ["one  two\u{a0}three", "one\t two  three"] {
        let output = reflow(&plan, &[sentence.into()]).unwrap();
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), sentence);
        assert!(output[0].1.ends_with(char::is_whitespace));
        assert!(output[1].1.ends_with(char::is_whitespace));
        assert!(!output[1].1.starts_with(char::is_whitespace));
        assert!(!output[2].1.starts_with(char::is_whitespace));
    }
}

#[test]
fn selection_handles_page_boundaries_and_ambiguous_ranges() {
    for page in [0, 1] {
        let sources = plain_sources(&[(0, 0, "Previous. Starting"), (page, 1, "continued. Next.")]);
        let (parts, joins) = choose(&sources, "Starting", "continued.").unwrap().unwrap();
        let plan = build_plan(parts, joins).unwrap();
        let output = reflow(&plan, &plan.items).unwrap();
        assert_eq!(output[0].0.page, 0);
        assert_eq!(output[1].0.page, page);
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), "Previous. Starting continued. Next.");
    }
    let ambiguous = plain_sources(&[(0, 0, "aaa"), (1, 0, "continues.")]);
    assert!(choose(&ambiguous, "aa", "continues.").is_err());
    let repeated = plain_sources(&[(0, 0, "foo foo"), (1, 0, "continues.")]);
    let (selected, _) = choose(&repeated, "foo", "continues.").unwrap().unwrap();
    assert_eq!(selected[0].range, 4..7);
    assert!(matches!(select(Decision::Uncertain, &ambiguous, &HashSet::new(), &HashSet::new()), Err(_)));
}

#[test]
fn embedded_formula_and_distinct_styles_survive_colliding_ids() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
    let rich = |id| InputPlaceholder::Rich(RichInput { id, composition_index: id as usize, left: format!("<s{id}>"), right: format!("</s{id}>"), left_pattern: format!("<s{id}>"), right_pattern: format!("</s{id}>"), source_text: String::new() });
    let formula = InputPlaceholder::Formula(FormulaInput { id: 1, composition_index: 1, token: "{v1}".into(), pattern: r"\{v1\}".into(), source_chars: "x²".into(), hint_source: false });
    let input = TranslateInput { text: "<s1>Earlier. First</s1>{v1}<s2>last</s2>".into(), placeholders: vec![rich(1), formula, rich(2)], original_tokens: vec![] };
    let left = Source::new(Location { page: 0, paragraph: 0 }, &input, &patterns, &mut regex).unwrap().unwrap();
    let right_input = TranslateInput { text: "<s1>continues. Later.</s1>".into(), placeholders: vec![rich(1)], original_tokens: vec![] };
    let right = Source::new(Location { page: 1, paragraph: 0 }, &right_input, &patterns, &mut regex).unwrap().unwrap();
    assert_eq!(left.formulas()[0].1, "x²");
    let start = left.text.find("First").unwrap();
    let end = right.text.find(" Later.").unwrap();
    let left_end = left.text.len();
    let plan = build_plan(vec![SelectedPart { source: left, range: start..left_end }, SelectedPart { source: right, range: 0..end }], vec![Join::Space]).unwrap();
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output[0].1.trim(), input.text);
    assert_eq!(output[1].1.trim(), right_input.text);
    let responses = plan.items.iter().map(|item| item.replace("Earlier. ", "之前。")
        .replace("First", "开始").replace("last", "部分").replace("continues.", "继续。")
        .replace(" Later.", "之后。")).collect::<Vec<_>>();
    let translated = reflow(&plan, &responses).unwrap();
    assert_eq!(translated[0].1, "<s1>之前。 开始</s1>{v1}<s2>部分</s2> ");
    assert_eq!(translated[1].1, "<s1>继续。 之后。</s1>");
    let sentence = 1;
    let original = &plan.items[sentence];
    let marker = regex::Regex::new(r"\[\[BABEL_[^\]]+\]\]").unwrap().find(original).unwrap().as_str();
    for changed in [original.replacen(marker, "", 1), original.replacen(marker, "[[BABEL_bad]]", 1), format!("{original}[[BABEL_extra]]"), format!("{original}[[BABEL_incomplete"), format!("{original}[[babel_fake]]"), format!("{original}[BABEL_fake]")] {
        let mut responses = plan.items.clone();
        responses[sentence] = changed;
        assert!(reflow(&plan, &responses).is_err());
    }
}

#[test]
fn numeric_reordering_preserves_signed_exponents_decimals_and_symbols() {
    let sources = plain_sources(&[(0, 0, "Results +1.25e-3 and"), (1, 0, "−4.5% ≈ 7.0.")]);
    let (parts, joins) = choose(&sources, &sources[0].text, &sources[1].text).unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    let response = "Results −4.5% ≈ 7.0 and +1.25e-3.";
    let output = reflow(&plan, &[response.into()]).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), response);
    for changed in ["Results −4.5% ≈ 7.0 and +1.25e-2.", "Results −4.5 ≈ 7.0 and +1.25e-3.", "Results −4.5% = 7.0 and +1.25e-3."] {
        assert!(reflow(&plan, &[changed.into()]).is_err());
    }
}

#[test]
fn no_split_inside_numeric_expression_or_word() {
    let sources = plain_sources(&[(0, 0, "Value"), (1, 0, "continues")]);
    let (parts, joins) = choose(&sources, "Value", "continues").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    assert!(reflow(&plan, &["unbreakable".into()]).is_err());
    assert!(reflow(&plan, &["élaboration".into()]).is_err());
    assert!(reflow(&plan, &["e\u{301}laboration".into()]).is_err());
    assert!(reflow(&plan, &["can't".into()]).is_err());
    assert!(reflow(&plan, &["pre-existing".into()]).is_err());
    assert!(reflow(&plan, &["中文翻译".into()]).is_ok());
    let sources = plain_sources(&[(0, 0, "Value 1.25e-3"), (1, 0, "continues")]);
    assert!(choose(&sources, "e-3", "continues").is_err());
    let (parts, joins) = choose(&sources, "Value 1.25e-3", "continues").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    assert!(reflow(&plan, &["1.25e-3".into()]).is_err());
}

#[test]
fn selection_cannot_cut_a_word_or_combining_character() {
    for (first, selected) in [("unfinished", "finished"), ("can't", "t"), ("pre-existing", "existing"), ("e\u{301}clair", "\u{301}clair")] {
        let sources = plain_sources(&[(0, 0, first), (1, 0, "continues.")]);
        assert!(choose(&sources, selected, "continues.").is_err());
    }
    for (last, selected) in [("unfinished", "un"), ("can't", "can"), ("pre-existing", "pre"), ("e\u{301}clair", "e")] {
        let sources = plain_sources(&[(0, 0, "Starts"), (1, 0, last)]);
        assert!(choose(&sources, "Starts", selected).is_err());
    }
}

#[test]
fn non_latin_words_and_graphemes_are_not_cut() {
    for (word, fragment) in [
        ("العربية", "عربية"),
        ("שָׁלוֹם", "לוֹם"),
        ("नमस्ते", "मस्ते"),
        ("क्\u{200d}ष", "ष"),
        ("می\u{200c}رود", "رود"),
        ("בית־ספר", "ספר"),
    ] {
        let sources = plain_sources(&[(0, 0, word), (1, 0, "continues")]);
        assert!(choose(&sources, fragment, "continues").is_err(), "selection cut {word}");
        let sources = plain_sources(&[(0, 0, "Starts"), (1, 0, word)]);
        assert!(choose(&sources, "Starts", fragment).is_err(), "selection cut {word}");
    }
    let sources = plain_sources(&[(0, 0, "Start"), (1, 0, "continues")]);
    let (parts, joins) = choose(&sources, "Start", "continues").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    for word in ["العربية", "שָׁלוֹם", "नमस्ते", "क्\u{200d}ष", "می\u{200c}رود", "בית־ספר", "👍🏽", "🇮🇳", "👩\u{200d}💻"] {
        assert!(reflow(&plan, &[word.into()]).is_err(), "split {word}");
    }
    for sentence in ["العربية جميلة", "שָׁלוֹם עולם", "नमस्ते दुनिया", "中文翻译", "日本語の文章", "か\u{3099}く"] {
        let output = reflow(&plan, &[sentence.into()]).unwrap();
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), sentence);
        assert!(!output[1].1.starts_with('\u{3099}'));
    }
}

#[test]
fn extended_grapheme_boundaries_are_preserved() {
    let sources = plain_sources(&[(0, 0, "Start"), (1, 0, "continues")]);
    let (parts, joins) = choose(&sources, "Start", "continues").unwrap().unwrap();
    let plan = build_plan(parts, joins).unwrap();
    for cluster in ["🏴\u{e0067}\u{e0062}\u{e007f}", "\u{600}中", "👩\u{200d}💻", "🇮🇳", "か\u{3099}", "각"] {
        assert!(reflow(&plan, &[cluster.into()]).is_err(), "split {cluster:?}");
        let source = plain_sources(&[(0, 0, cluster), (1, 0, "continues")]);
        let last = cluster.char_indices().last().unwrap().0;
        assert!(choose(&source, &cluster[last..], "continues").is_err(), "selection split {cluster:?}");
    }
    for sentence in ["🏴\u{e0067}\u{e0062}\u{e007f} 中文", "\u{600}中 文", "👩\u{200d}💻 工作", "🇮🇳 中文", "नमस्ते दुनिया"] {
        let output = reflow(&plan, &[sentence.into()]).unwrap();
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), sentence);
    }
    let source = plain_sources(&[(0, 0, "start\r\n"), (1, 0, "continues")]);
    assert!(choose(&source, "\n", "continues").is_err());
}

#[test]
fn formula_only_object_can_be_reflowed() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    let formula = TranslateInput { text: "{v}".into(), placeholders: vec![InputPlaceholder::Formula(FormulaInput {
        id: 0, composition_index: 0, token: "{v}".into(), pattern: r"\{v\}".into(), source_chars: "x".into(), hint_source: false,
    })], original_tokens: vec![] };
    let left = Source::new(Location { page: 0, paragraph: 0 }, &formula, &patterns, &mut regex).unwrap().unwrap();
    let right = plain_sources(&[(1, 0, "continues")]).remove(0);
    let left_end = left.text.len();
    let right_end = right.text.len();
    let plan = build_plan(vec![SelectedPart { source: left, range: 0..left_end }, SelectedPart { source: right, range: 0..right_end }], vec![Join::Space]).unwrap();
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output[0].1.trim(), "{v}");
    assert_eq!(output[1].1.trim(), "continues");
}

#[test]
fn reflow_reserves_nonempty_parts_for_later_objects() {
    for (values, response, expected) in [
        (vec!["Supercalifragilisticexpialidocious", "b", "c"], "one two three", vec!["one", "two", "three"]),
        (vec!["Supercalifragilisticexpialidocious", "b", "c", "d"], "one two three four", vec!["one", "two", "three", "four"]),
    ] {
        let input = values.iter().enumerate().map(|(index, value)| (index, 0, *value)).collect::<Vec<_>>();
        let sources = plain_sources(&input);
        let parts = sources.into_iter().map(|source| { let end = source.text.len(); SelectedPart { source, range: 0..end } }).collect::<Vec<_>>();
        let plan = build_plan(parts, vec![Join::Space; values.len() - 1]).unwrap();
        let output = reflow(&plan, &[response.into()]).unwrap();
        assert_eq!(output.iter().map(|entry| entry.1.trim()).collect::<Vec<_>>(), expected);
        assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), response);
        assert!(reflow(&plan, &["one two".into()]).is_err());
    }
}

#[test]
fn reflow_reserves_formula_only_middle_object() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    let input = TranslateInput { text: "{v}".into(), placeholders: vec![InputPlaceholder::Formula(FormulaInput {
        id: 0, composition_index: 0, token: "{v}".into(), pattern: r"\{v\}".into(), source_chars: "x".into(), hint_source: false,
    })], original_tokens: vec![] };
    let formula = Source::new(Location { page: 1, paragraph: 0 }, &input, &patterns, &mut regex).unwrap().unwrap();
    let mut sources = plain_sources(&[(0, 0, "Supercalifragilisticexpialidocious"), (2, 0, "end")]);
    sources.insert(1, formula);
    let parts = sources.into_iter().map(|source| { let end = source.text.len(); SelectedPart { source, range: 0..end } }).collect();
    let plan = build_plan(parts, vec![Join::Space, Join::Space]).unwrap();
    let marker = regex::Regex::new(r"\[\[BABEL_[^\]]+\]\]").unwrap().find(&plan.items[0]).unwrap().as_str();
    let output = reflow(&plan, &[format!("one {marker} two")]).unwrap();
    assert_eq!(output.iter().map(|entry| entry.1.trim()).collect::<Vec<_>>(), vec!["one", "{v}", "two"]);
    assert_eq!(output.iter().map(|entry| entry.1.as_str()).collect::<String>(), "one {v} two");
    assert_eq!(output[0].1, "one ");
    assert_eq!(output[1].1, "{v} ");
    assert_eq!(output[2].1, "two");
    assert!(reflow(&plan, &[format!("one {marker}")]).is_err());
}

#[test]
fn large_mixed_objects_preserve_output_and_reject_corruption() {
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    let input = TranslateInput {
        text: format!("{}{{v0}} {}{{v1}}", "alpha +1.25e-3 中文 👩\u{200d}💻 ".repeat(8), "か\u{3099} −4.5% ".repeat(8)),
        placeholders: (0..2).map(|index| InputPlaceholder::Formula(FormulaInput {
            id: index as _, composition_index: index, token: format!("{{v{index}}}"), pattern: format!(r"\{{v{index}\}}"), source_chars: "x²".into(), hint_source: false,
        })).collect(),
        original_tokens: vec![],
    };
    let parts = (0..48).map(|paragraph| {
        let source = Source::new(Location { page: paragraph / 24, paragraph }, &input, &patterns, &mut regex).unwrap().unwrap();
        let end = source.text.len();
        SelectedPart { source, range: 0..end }
    }).collect();
    let plan = build_plan(parts, vec![Join::Space; 47]).unwrap();
    let output = reflow(&plan, &plan.items).unwrap();
    assert_eq!(output.len(), 48);
    assert!(output.iter().all(|(_, value)| value.trim() == input.text));
    let mut bad_number = plan.items.clone();
    bad_number[0] = bad_number[0].replacen("+1.25e-3", "+1.25e-2", 1);
    assert_eq!(reflow(&plan, &bad_number).unwrap_err(), "unsafe response");
    let mut bad_marker = plan.items.clone();
    bad_marker[0] = bad_marker[0].replacen("[[BABEL_", "[[BABEL_bad", 1);
    assert_eq!(reflow(&plan, &bad_marker).unwrap_err(), "unsafe response");
}

#[test]
fn reflow_cancellation_returns_no_partial_output() {
    let text = "alpha beta ".repeat(400);
    let sources = plain_sources(&[(0, 0, &text), (1, 0, &text)]);
    let parts = sources.into_iter().map(|source| {
        let end = source.text.len();
        SelectedPart { source, range: 0..end }
    }).collect();
    let plan = build_plan(parts, vec![Join::Space]).unwrap();
    assert_eq!(reflow_with_cancellation(&plan, &plan.items, || true).unwrap_err(), "reflow cancelled");
    let mut checks = 0;
    let cancel_after = 3 + 2 * plan.items[0].chars().count() / 256;
    let result = reflow_with_cancellation(&plan, &plan.items, || {
        checks += 1;
        checks >= cancel_after
    });
    assert_eq!(result.unwrap_err(), "reflow cancelled");
    assert_eq!(checks, cancel_after);
    assert_eq!(reflow_with_cancellation(&plan, &plan.items, || false).unwrap(), reflow(&plan, &plan.items).unwrap());
}

#[test]
#[ignore]
fn synthetic_reflow_scaling() {
    let objects: usize = std::env::var("REFLOW_OBJECTS").unwrap().parse().unwrap();
    let repetitions: usize = std::env::var("REFLOW_WORDS").unwrap().parse().unwrap();
    let formulas: usize = std::env::var("REFLOW_FORMULAS").unwrap().parse().unwrap();
    let mut regex = RegexService::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile("never-matches", false).unwrap(), regex.compile("never-matches", false).unwrap()];
    let input = TranslateInput {
        text: format!("{}{}", "alpha ".repeat(repetitions), (0..formulas).map(|index| format!("{{v{index}}} ")).collect::<String>()),
        placeholders: (0..formulas).map(|index| InputPlaceholder::Formula(FormulaInput {
            id: index as _, composition_index: index, token: format!("{{v{index}}}"), pattern: format!(r"\{{v{index}\}}"), source_chars: "x".into(), hint_source: false,
        })).collect(),
        original_tokens: vec![],
    };
    let parts = (0..objects).map(|paragraph| {
        let source = Source::new(Location { page: 0, paragraph }, &input, &patterns, &mut regex).unwrap().unwrap();
        let end = source.text.len();
        SelectedPart { source, range: 0..end }
    }).collect();
    let plan = build_plan(parts, vec![Join::Space; objects - 1]).unwrap();
    let start = std::time::Instant::now();
    let output = reflow(&plan, &plan.items).unwrap();
    println!("objects={objects} words_per_object={repetitions} formulas_per_object={formulas} bytes={} elapsed_ms={:.3}", plan.items[0].len(), start.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(output.len(), objects);
    assert!(output.iter().all(|(_, value)| value.trim() == input.text.trim()));
}
