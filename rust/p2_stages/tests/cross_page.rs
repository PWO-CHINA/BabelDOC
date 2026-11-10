use babeldoc_p2_stages::translation::ordinary_translation;
use babeldoc_p2_stages::translation::cross_page_style;

#[path = "../src/translation/cross_page.rs"]
mod cross_page;

#[path = "../src/translation/cross_page_protocol.rs"]
mod cross_page_protocol;

#[test]
fn context_sentence_evidence_is_conservative() {
    use cross_page::has_context_sentence_evidence as accepts;
    assert!(accepts("A complete earlier sentence. The unfinished sentence continues across", "the next page and ends here. Another sentence follows."));
    assert!(accepts("Earlier evidence is encouraging. The treatment could be useful for", "circadian disorders and related conditions. Another observation follows."));
    assert!(accepts("A prior result was observed321. The treatment could be useful for", "circadian disorders and related conditions. Another observation follows."));
    assert!(accepts("The first finding was observed321. A second observation was reported322. The treatment could be useful for", "circadian disorders and related conditions. Another observation follows."));
    assert!(accepts("The treatment was associated with improved", "survival in the study population. Another observation follows."));
    for (left, right) in [
        ("An incomplete heading", "the continuation ends here."),
        ("Dr. Smith studied the unfinished", "the continuation ends here."),
        ("• A list item continues across", "the next page and ends here."),
        ("A heading: the results were observed", "the continuation ends here."),
        ("the treatment was associated with improved", "survival in the study population."),
        ("Earlier sentence. Heading", "the continuation ends here."),
        ("Earlier sentence. The unfinished portion", "the following heading"),
        ("A value 3.14 indicates the unfinished", "the continuation ends here."),
        ("Earlier sentence. The unfinished portion", "the value is 3.14 only"),
        ("Earlier sentence. The unfinished portion", "• list item ends here."),
        ("Earlier sentence. The unfinished portion", "1. list item ends here."),
        ("Earlier sentence. The unfinished portion", "New paragraph begins here."),
    ] { assert!(!accepts(left, right), "{left:?} {right:?}"); }
}

#[test]
fn numeric_continuation_requires_special_handling() {
    use cross_page::numeric_continuation_needs_review as needs_review;
    assert!(needs_review("Earlier results were clear. After analysis, 7 of the recovered", "11 samples were checked successfully."));
    assert!(needs_review("The first 3 examples include", "42 samples in the final analysis."));
    assert!(!needs_review("Earlier results were clear. The recovered", "11 samples were checked successfully."));
    assert!(!needs_review("Earlier results were clear. After analysis, 7 of the recovered", "samples were checked successfully."));
    assert!(!needs_review("Earlier results were clear. After analysis, 7 of the recovered", "11 samples have no ending"));
    assert!(!needs_review("Earlier results were clear. After analysis, 7 of the recovered", "1. A new numbered section begins."));
}

#[test]
fn extracts_only_an_unambiguous_complete_sentence() {
    use cross_page::extract_context_sentence as extract;
    assert_eq!(extract("Earlier result was clear. The treatment could be useful for", "circadian disorders and related conditions. Another observation follows."),
        Some("The treatment could be useful for circadian disorders and related conditions.".into()));
    assert_eq!(extract("Earlier result was clear. The café treatment could be useful for", "éventual disorders and related conditions. Another observation follows."),
        Some("The café treatment could be useful for éventual disorders and related conditions.".into()));
    for (left, right) in [
        ("Dr. Smith studied the unfinished", "the continuation ends here."),
        ("Earlier result was clear. The value of 3.14 continues across", "the page and ends here."),
        ("Earlier result was clear. The unfinished sentence continues across", "the value is 3.14 here."),
        ("Earlier result was clear. The unfinished sentence continues across", "the following page"),
    ] { assert_eq!(extract(left, right), None); }
}

#[test]
fn continuation_with_citation_is_provided_as_one_continuous_sentence() {
    let left = "A prior finding was reported321. Although these early results need further confirmation, the proposed treatment may help patients with circadian";
    let right = "disorders and related conditions. Another observation follows.";
    assert_eq!(cross_page::extract_context_sentence(left, right).as_deref(), Some("Although these early results need further confirmation, the proposed treatment may help patients with circadian disorders and related conditions."));
}

#[test]
fn boundary_shapes_keep_complete_context_and_ambiguity() {
    use cross_page::extract_context_sentence as extract;
    assert_eq!(extract("Previous results were observed (Fig. 6d). After analysis, 7 of the recovered", "11 samples were checked successfully (Fig. 6e and supplementary Fig. 60)."),
        Some("After analysis, 7 of the recovered 11 samples were checked successfully (Fig. 6e and supplementary Fig. 60).".into()));
    assert_eq!(extract("Earlier methods were effective. Combining records from imaging,", "pathology and screening could improve patient care (supplementary information)."),
        Some("Combining records from imaging, pathology and screening could improve patient care (supplementary information).".into()));
    assert_eq!(extract("Previous evidence was reported (14). Yet, sample selection continued during develop-", "ment (15–17) after the initial screening phase."),
        Some("Yet, sample selection continued during develop-\nment (15–17) after the initial screening phase.".into()));
    for (left, right) in [
        ("The list contains the following", "1. A numbered section begins here."),
        ("The analysis includes state-of-", "the-art techniques are used here."),
        ("The treatment was associated with improved", "survival (Smith et al. 2024) in the population."),
        ("The treatment was associated with improved", "survival (3.14) in the population."),
        ("The treatment was associated with improved", "survival (Fig. 3 in the population."),
    ] {
        assert_eq!(extract(left, right), None, "{left:?} {right:?}");
    }
}

#[test]
fn reference_boundary_is_not_a_continuation() {
    let left = "The organizers are required for development. Nature 403, 658-661";
    let right = "Sample, A. B. (1994). A fictional reference title.";
    assert!(!cross_page::has_context_sentence_evidence(left, right));
    assert_eq!(cross_page::extract_context_sentence(left, right), None);
}

use ordinary_translation::{PlaceholderRegex, RegexMatch, TranslateInput};

#[derive(Default)]
struct TestRegex(Vec<regex::Regex>);

impl PlaceholderRegex for TestRegex {
    type Error = regex::Error;

    fn compile(&mut self, pattern: &str, ignore_case: bool) -> Result<u64, Self::Error> {
        self.0.push(regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build()?);
        Ok((self.0.len() - 1) as u64)
    }

    fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> {
        Ok(self.0[pattern as usize].is_match(text))
    }

    fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<RegexMatch>, Self::Error> {
        Ok(self.0[pattern as usize].find_iter(text).map(|item| RegexMatch {
            full: item.as_str().into(),
            start: text[..item.start()].chars().count(),
            end: text[..item.end()].chars().count(),
            groups: vec![],
        }).collect())
    }
}

fn input(text: &str) -> TranslateInput {
    TranslateInput { text: text.into(), placeholders: vec![], original_tokens: vec![] }
}

#[test]
fn redistributed_english_keeps_word_boundary_and_all_text() {
    let left = input("An incomplete sentence");
    let right = input("continues here. Another sentence.");
    let moved = cross_page_protocol::move_sentence_prefix(&left, &right, &[]).unwrap();
    let mut regex = TestRegex::default();
    let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<b\d+>", false).unwrap(), regex.compile(r"</b\d+>", false).unwrap()];
    let (first, second) = cross_page_protocol::redistribute_response(
        &moved.left, "alpha beta gamma", &moved.right, "next sentence.", &left, &right, &patterns, &mut regex,
    ).unwrap().unwrap();
    assert_eq!(format!("{first}{second}"), "alpha beta gamma next sentence.");
}

#[test]
fn protected_formula_in_continuation_refuses_move() {
    let left = input("An incomplete sentence");
    let right = input("continues with {v1} here. Another sentence.");
    let start = right.text.find("{v1}").unwrap();
    assert!(cross_page_protocol::move_sentence_prefix(&left, &right, &[start..start + 4]).is_none());
}
