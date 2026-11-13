use babeldoc_p2_stages::translation::{
    cross_boundary::{
        DocumentPlan, DocumentSelection, Join, Location, OutputPart, SelectedPart, Source,
    },
    ordinary_translation::{
        FormulaInput, InputPlaceholder, PlaceholderRegex, RegexMatch, RichInput, TranslateInput,
    },
};

#[derive(Default)]
struct RegexService(Vec<regex::Regex>);
impl PlaceholderRegex for RegexService {
    type Error = regex::Error;
    fn compile(&mut self, pattern: &str, ignore_case: bool) -> Result<u64, Self::Error> {
        self.0.push(
            regex::RegexBuilder::new(pattern)
                .case_insensitive(ignore_case)
                .build()?,
        );
        Ok((self.0.len() - 1) as u64)
    }
    fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> {
        Ok(self.0[pattern as usize]
            .find(text)
            .is_some_and(|found| found.start() == 0))
    }
    fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<RegexMatch>, Self::Error> {
        Ok(self.0[pattern as usize]
            .find_iter(text)
            .map(|found| RegexMatch {
                full: found.as_str().into(),
                start: text[..found.start()].chars().count(),
                end: text[..found.end()].chars().count(),
                groups: vec![],
            })
            .collect())
    }
}

fn source(page: usize, paragraph: usize, text: &str) -> Source {
    let mut regex = RegexService::default();
    let pattern = regex.compile("never-matches", false).unwrap();
    Source::new(
        Location { page, paragraph },
        &TranslateInput {
            text: text.into(),
            placeholders: vec![],
            original_tokens: vec![],
        },
        &[pattern; 3],
        &mut regex,
    )
    .unwrap()
    .unwrap()
}

fn part(source: &Source, text: &str) -> SelectedPart {
    let start = source.text.find(text).unwrap();
    SelectedPart {
        source: source.clone(),
        range: start..start + text.len(),
    }
}

fn text(output: &[babeldoc_p2_stages::translation::cross_boundary::ResolvedParagraph]) -> String {
    output
        .iter()
        .flat_map(|paragraph| &paragraph.parts)
        .map(|part| match part {
            OutputPart::Text { text, .. } => text.as_str(),
            OutputPart::Formula(_) => "{formula}",
        })
        .collect()
}

#[test]
fn enhanced_sentence_preserves_extended_numeric_expressions() {
    for (original, damaged) in [
        ("–7", "7"), ("7٪", "7"), ("−7", "7"), ("-7", "7"),
        ("≪7", "7"), ("7%", "7"), ("1e-3", "1e3"),
        ("1,000", "1.000"), ("7 × 3", "7 ÷ 3"), ("7 +/- 3", "7 - 3"),
        ("7 $", "7"), ("7", "-7"), ("7-", "7"),
    ] {
        let left = source(0, 0, &format!("The value reached {original} during"));
        let right = source(1, 0, "11 experiments.");
        let selection = DocumentSelection { parts: vec![
            part(&left, &left.text), part(&right, &right.text),
        ], joins: vec![Join::Space] };
        let plan = DocumentPlan::new(vec![selection]).unwrap();
        assert!(plan.validate_items(0, &[format!("测量值{original}，涉及11次实验。")]).is_ok(), "{original}");
        assert!(plan.validate_items(0, &[format!("测量值{damaged}，涉及11次实验。")]).is_err(), "numeric damage accepted: {original} -> {damaged}");
    }
}

#[test]
fn shared_paragraph_has_two_independent_sentences_and_one_writeback() {
    let first = source(0, 0, "Earlier. Alpha beta");
    let middle = source(1, 0, "gamma delta. Middle remains. Epsilon zeta");
    let last = source(2, 0, "eta theta. Following.");
    let plan = DocumentPlan::new(vec![
        DocumentSelection {
            parts: vec![part(&first, "Alpha beta"), part(&middle, "gamma delta.")],
            joins: vec![Join::Space],
        },
        DocumentSelection {
            parts: vec![part(&middle, "Epsilon zeta"), part(&last, "eta theta.")],
            joins: vec![Join::Space],
        },
    ])
    .unwrap();
    assert_eq!(
        &plan.items[..2],
        &["Alpha beta gamma delta.", "Epsilon zeta eta theta."]
    );
    assert!(plan.items.contains(&" Middle remains. ".to_owned()));
    let output = plan.resolve(&plan.items).unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(
        text(&output),
        "Earlier. Alpha beta gamma delta. Middle remains. Epsilon zeta eta theta. Following."
    );
}

#[test]
fn subset_selection_is_deduplicated() {
    let values = [
        source(0, 0, "We found"),
        source(1, 0, "that the"),
        source(1, 1, "method"),
        source(1, 2, "works."),
    ];
    let parts = values
        .iter()
        .map(|source| part(source, &source.text))
        .collect::<Vec<_>>();
    let plan = DocumentPlan::new(vec![
        DocumentSelection {
            parts: parts.clone(),
            joins: vec![Join::Space; 3],
        },
        DocumentSelection {
            parts: parts[1..3].to_vec(),
            joins: vec![Join::Space],
        },
    ])
    .unwrap();
    assert_eq!(plan.items, vec!["We found that the method works."]);
    assert_eq!(text(&plan.resolve(&plan.items).unwrap()), plan.items[0]);
}

#[test]
fn short_translation_can_leave_empty_source_container() {
    let values = [
        source(0, 0, "It"),
        source(1, 0, "is"),
        source(1, 1, "true."),
    ];
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: values
            .iter()
            .map(|source| part(source, &source.text))
            .collect(),
        joins: vec![Join::Space; 2],
    }])
    .unwrap();
    let output = plan.resolve(&["是。".into()]).unwrap();
    assert_eq!(text(&output), "是。");
    assert_eq!(output.len(), 3);
}

#[test]
fn dehyphenation_and_unbreakable_target_do_not_require_word_split() {
    let first = source(0, 0, "regu-");
    let last = source(1, 0, "lation");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)],
        joins: vec![Join::Dehyphenate],
    }])
    .unwrap();
    assert_eq!(plan.items, vec!["regulation"]);
    assert_eq!(text(&plan.resolve(&plan.items).unwrap()), "regulation");
}

#[test]
fn fraction_order_and_internal_separator_are_preserved() {
    let first = source(0, 0, "The ratio 1/2");
    let last = source(1, 0, "holds.");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)],
        joins: vec![Join::Space],
    }])
    .unwrap();
    for response in ["比例2/1成立。", "比例1 2成立。"] {
        assert!(plan.resolve(&[response.into()]).is_err());
    }
    let output = plan.resolve(&["比例1/2成立。".into()]).unwrap();
    assert_eq!(text(&output), "比例1/2成立。");
    assert!(output.iter().any(|paragraph| {
        paragraph
            .parts
            .iter()
            .any(|part| matches!(part,OutputPart::Text{text,..} if text.contains("1/2")))
    }));
}

#[test]
fn formula_can_reorder_inside_page_without_changing_identity() {
    let mut regex = RegexService::default();
    let formula_pattern = regex.compile(r"\{v\d+\}", false).unwrap();
    let never = regex.compile("never-matches", false).unwrap();
    let input = TranslateInput {
        text: "Compare {v1} with {v2}".into(),
        placeholders: (1..=2)
            .map(|index| {
                InputPlaceholder::Formula(FormulaInput {
                    id: index,
                    composition_index: index as usize,
                    token: format!("{{v{index}}}"),
                    pattern: format!(r"\{{v{index}\}}"),
                    source_chars: format!("x{index}"),
                    hint_source: true,
                })
            })
            .collect(),
        original_tokens: vec![],
    };
    let first = Source::new(
        Location {
            page: 0,
            paragraph: 0,
        },
        &input,
        &[formula_pattern, never, never],
        &mut regex,
    )
    .unwrap()
    .unwrap();
    let last = source(0, 1, "carefully.");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)],
        joins: vec![Join::Space],
    }])
    .unwrap();
    let formulas = plan.formula_context();
    let response = format!("仔细比较{}和{}。", formulas[1].0, formulas[0].0);
    let output = plan.resolve(&[response]).unwrap();
    let resources = output
        .iter()
        .flat_map(|paragraph| &paragraph.parts)
        .filter_map(|part| match part {
            OutputPart::Formula(resource) => Some(resource.composition),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resources, vec![2, 1]);
}

#[test]
fn mathematical_formula_context_uses_ordinary_translation() {
    let mut regex = RegexService::default();
    let formula_pattern = regex.compile(r"\{v\d+\}", false).unwrap();
    let never = regex.compile("never-matches", false).unwrap();
    let input = TranslateInput {
        text: "The difference {v1}-{v2} remains".into(),
        placeholders: (1..=2).map(|index| InputPlaceholder::Formula(FormulaInput {
            id: index, composition_index: index as usize, token: format!("{{v{index}}}"),
            pattern: format!(r"\{{v{index}\}}"), source_chars: if index == 1 { "x".into() } else { "y".into() },
            hint_source: true,
        })).collect(), original_tokens: vec![],
    };
    let first = Source::new(Location { page: 0, paragraph: 0 }, &input,
        &[formula_pattern, never, never], &mut regex).unwrap().unwrap();
    let last = source(0, 1, "positive.");
    assert!(DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)], joins: vec![Join::Space],
    }]).is_err());
    for text in ["The difference {v1}-({v2}) remains", "The value -{v1} remains", "The difference {v1}-α remains"] {
        let mut input = input.clone();
        input.text = text.into();
        if !text.contains("{v2}") { input.placeholders.truncate(1); }
        let first = Source::new(Location { page: 0, paragraph: 0 }, &input,
            &[formula_pattern, never, never], &mut regex).unwrap().unwrap();
        assert!(DocumentPlan::new(vec![DocumentSelection {
            parts: vec![part(&first, &first.text), part(&last, &last.text)], joins: vec![Join::Space],
        }]).is_err(), "unsafe math context accepted: {text}");
    }
}

#[test]
fn variable_exponent_sign_and_numeric_style_ownership_are_preserved() {
    let first = source(0, 0, "The factor 10^-x stays");
    let last = source(1, 0, "positive.");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)], joins: vec![Join::Space],
    }]).unwrap();
    assert!(plan.validate_items(0, &["因子10^x为正。".into()]).is_err());
    assert!(plan.validate_items(0, &["因子10^-x为正。".into()]).is_ok());

    let mut regex = RegexService::default();
    let never = regex.compile("never-matches", false).unwrap();
    let left = regex.compile("<s>", false).unwrap();
    let right = regex.compile("</s>", false).unwrap();
    let input = TranslateInput { text: "Rate 10<s>3</s> stays".into(),
        placeholders: vec![InputPlaceholder::Rich(RichInput {
            id: 0, composition_index: 1, left: "<s>".into(), right: "</s>".into(),
            left_pattern: "<s>".into(), right_pattern: "</s>".into(), source_text: "3".into(),
        })], original_tokens: vec![],
    };
    let first = Source::new(Location { page: 0, paragraph: 0 }, &input,
        &[never, left, right], &mut regex).unwrap().unwrap();
    let last = source(1, 0, "constant.");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)], joins: vec![Join::Space],
    }]).unwrap();
    let input = &plan.items[0];
    let markers = regex::Regex::new(r"\[\[BABEL_[^\]]*\]\]").unwrap();
    let markers = markers.find_iter(input).map(|marker| marker.as_str()).collect::<Vec<_>>();
    let wrong = format!("{}Rate{} 10 3 stays constant.", markers[0], markers[1]);
    assert!(plan.validate_items(0, &[wrong]).is_err());
    assert!(plan.validate_items(0, std::slice::from_ref(input)).is_ok());
}

#[test]
fn source_range_cannot_select_only_fraction_denominator() {
    let first = source(0, 0, "Ratio 1/2");
    let last = source(1, 0, "holds.");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, "2"), part(&last, &last.text)],
        joins: vec![Join::Space],
    }]);
    assert!(plan.is_err());
}

#[test]
fn nonempty_style_must_still_cover_translated_text() {
    let mut regex = RegexService::default();
    let never = regex.compile("never-matches", false).unwrap();
    let left = regex.compile("<s>", false).unwrap();
    let right = regex.compile("</s>", false).unwrap();
    let input = TranslateInput {
        text: "<s>important</s>".into(),
        placeholders: vec![InputPlaceholder::Rich(RichInput {
            id: 0,
            composition_index: 0,
            left: "<s>".into(),
            right: "</s>".into(),
            left_pattern: "<s>".into(),
            right_pattern: "</s>".into(),
            source_text: "important".into(),
        })],
        original_tokens: vec![],
    };
    let first = Source::new(
        Location {
            page: 0,
            paragraph: 0,
        },
        &input,
        &[never, left, right],
        &mut regex,
    )
    .unwrap()
    .unwrap();
    let last = source(0, 1, "result");
    let plan = DocumentPlan::new(vec![DocumentSelection {
        parts: vec![part(&first, &first.text), part(&last, &last.text)],
        joins: vec![Join::Space],
    }])
    .unwrap();
    let markers = regex::Regex::new(r"\[\[BABEL_[^\]]+\]\]")
        .unwrap()
        .find_iter(&plan.items[0])
        .map(|marker| marker.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        plan.resolve(&[format!("重要结果{}{}", markers[0], markers[1])])
            .is_err()
    );
    assert!(
        plan.resolve(&[format!("{}重要{}结果", markers[0], markers[1])])
            .is_ok()
    );
}
