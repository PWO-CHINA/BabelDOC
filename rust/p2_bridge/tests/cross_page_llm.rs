#![allow(dead_code)]

mod ordinary_translation {
    use babeldoc_p2_stages::translation::ordinary_translation::{PlaceholderRegex, RegexMatch, TranslateInput};
    use pyo3::prelude::*;
    use std::marker::PhantomData;

    #[derive(Clone)]
    pub(crate) struct Job {
        pub page: usize, pub paragraph: usize, pub eligible: bool, pub vertical: bool, pub text: String,
        pub missing_font_style: Option<()>, pub input: Option<TranslateInput>,
        pub title: Option<(String, String)>, pub local_title: Option<(String, String)>,
    }
    pub(crate) struct RegexService<'a>(pub Vec<String>, pub PhantomData<&'a ()>);
    impl PlaceholderRegex for RegexService<'_> {
        type Error = PyErr;
        fn compile(&mut self, pattern: &str, _: bool) -> PyResult<u64> {
            self.0.push(pattern.to_owned()); Ok((self.0.len() - 1) as u64)
        }
        fn matches(&mut self, pattern: u64, text: &str) -> PyResult<bool> { Ok(!self.find_all(pattern, text)?.is_empty()) }
        fn find_all(&mut self, pattern: u64, text: &str) -> PyResult<Vec<RegexMatch>> {
            let pattern = &self.0[pattern as usize];
            Ok(text.match_indices(pattern).map(|(start, found)| RegexMatch {
                full: found.to_owned(), start: text[..start].chars().count(),
                end: text[..start + found.len()].chars().count(), groups: vec![],
            }).collect())
        }
    }
}
mod cross_page {
    use crate::ordinary_translation::RegexService;
    use babeldoc_ir_core::Document;
    use babeldoc_p2_stages::translation::ordinary_translation::TranslateInput;
    use pyo3::prelude::*;
    use std::ops::Range;
    pub(crate) type PageKey = (usize, usize);
    pub(crate) fn find_pairs(_: &mut Document, _: &Bound<'_, PyAny>) -> PyResult<Vec<(PageKey, PageKey)>> { unreachable!() }
    pub(crate) fn protected_ranges(_: &mut RegexService<'_>, _: &TranslateInput, _: &[u64]) -> PyResult<Option<Vec<Range<usize>>>> { Ok(Some(vec![])) }
}
#[path = "../src/cross_page_llm.rs"]
mod cross_page_llm;

#[test]
fn selects_only_unique_eligible_ordered_pair() {
    use babeldoc_p2_stages::translation::ordinary_translation::TranslateInput;
    let job = |page| ordinary_translation::Job {
        page, paragraph: 0, eligible: true, vertical: false, missing_font_style: None, text: "source text".into(),
        input: Some(TranslateInput { text: "续句 text".into(), placeholders: vec![], original_tokens: vec![] }),
        title: None, local_title: None,
    };
    let pair = ((0, 0), (1, 0));
    let batch = [pair.0, pair.1];
    let jobs = vec![job(0), job(1)];
    let select = |pairs: &[_], batch: &[_], jobs: &[_], length| cross_page_llm::is_context_pair(pairs, batch, jobs, length);
    assert!(select(&[pair], &batch, &jobs, 7));
    assert!(!select(&[pair], &[pair.1, pair.0], &jobs, 1));
    assert!(!select(&[pair, pair], &batch, &jobs, 1));
    assert!(!select(&[pair, (pair.0, (2, 0))], &batch, &jobs, 1));
    assert!(!select(&[pair, ((2, 0), pair.1)], &batch, &jobs, 1));
    assert!(!select(&[pair], &[pair.0], &jobs, 1));
    assert!(!select(&[pair], &[pair.0, pair.1, (2, 0)], &jobs, 1));
    assert!(!select(&[pair], &batch, &jobs[..1], 1));
    assert!(!select(&[pair], &batch, &[job(0), job(0), job(1)], 1));
    assert!(!select(&[pair], &batch, &jobs, 8));
    for change in 0..4 {
        let mut invalid = jobs.clone();
        match change {
            0 => invalid[0].eligible = false,
            1 => invalid[0].vertical = true,
            2 => invalid[0].missing_font_style = Some(()),
            _ => invalid[0].input = None,
        }
        assert!(!select(&[pair], &batch, &invalid, 1));
    }
}

#[test]
fn prioritizes_pairs_across_batches_without_losing_inputs() {
    use babeldoc_p2_stages::translation::{llm_only::{BatchPlan, GroupKind, ParagraphId}, ordinary_translation::TranslateInput, token_count::count_tokens};
    let job = |page| ordinary_translation::Job {
        page, paragraph: 0, eligible: true, vertical: false, missing_font_style: None, text: format!("source {page}"),
        input: Some(TranslateInput { text: "translation input".into(), placeholders: vec![], original_tokens: vec![] }),
        title: None, local_title: None,
    };
    let id = |page| ParagraphId { page, index: 0 };
    let jobs = vec![job(0), job(1), job(2), job(3)];
    let original = vec![
        BatchPlan { kind: GroupKind::Page, id: 1, paragraphs: vec![id(0), id(2)], token_count: 999 },
        BatchPlan { kind: GroupKind::CrossPage, id: 2, paragraphs: vec![id(1), id(3)], token_count: 999 },
    ];
    let mut plans = original.clone();
    cross_page_llm::prioritize_context_pairs(&mut plans, &[((0, 0), (1, 0))], &jobs, 1);
    assert_eq!(plans[0], BatchPlan { kind: GroupKind::CrossPage, id: 3, paragraphs: vec![id(0), id(1)], token_count: count_tokens(&jobs[0].text) + count_tokens(&jobs[1].text) });
    assert_eq!(plans[1].paragraphs, vec![id(2)]);
    assert_eq!(plans[1].token_count, count_tokens(&jobs[2].text));
    assert_eq!(plans[2].paragraphs, vec![id(3)]);
    assert_eq!(plans[2].token_count, count_tokens(&jobs[3].text));
    let mut conflict = original.clone();
    cross_page_llm::prioritize_context_pairs(&mut conflict, &[((0, 0), (1, 0)), ((0, 0), (2, 0))], &jobs, 1);
    assert_eq!(conflict, original);
    let mut duplicate = original.clone();
    duplicate[1].paragraphs.push(id(0));
    let before = duplicate.clone();
    cross_page_llm::prioritize_context_pairs(&mut duplicate, &[((0, 0), (1, 0))], &jobs, 1);
    assert_eq!(duplicate, before);
}

#[test]
fn plans_synthetic_pair_and_keeps_originals_for_fallback() {
    use babeldoc_p2_stages::translation::{cross_page_batch::{BatchFailure, BatchResponse}, ordinary_translation::{InputPlaceholder, PlaceholderRegex, RichInput, TranslateInput}};
    let rich = InputPlaceholder::Rich(RichInput { id: 2, composition_index: 0, left: "<s2>".into(), right: "</s2>".into(), left_pattern: "<s2>".into(), right_pattern: "</s2>".into(), source_text: String::new() });
    let job = |page, text: &str, placeholders| ordinary_translation::Job {
        page, paragraph: 0, eligible: true, vertical: false, missing_font_style: None, text: text.into(),
        input: Some(TranslateInput { text: text.into(), placeholders, original_tokens: vec![] }),
        title: None, local_title: None,
    };
    let jobs = vec![job(0, "Earlier sentence. An unfinished", vec![]), job(1, "<s2>sentence here. Another sentence</s2>", vec![rich])];
    let mut regex = ordinary_translation::RegexService(vec![], std::marker::PhantomData);
    let patterns = [regex.compile("{v7}", false).unwrap(), regex.compile("<s2>", false).unwrap(), regex.compile("</s2>", false).unwrap()];
    let work = cross_page_llm::plan_pairs(&[((0, 0), (1, 0))], &jobs, &mut regex, &patterns, 1, "Translate", "Chinese", &[]).unwrap();
    assert_eq!(work.len(), 1);
    assert!(work[0].prompt.contains("An unfinished sentence here."));
    assert!(work[0].prompt.contains("Item id 1 is one complete sentence; translate it as a whole."));
    assert!(work[0].prompt.contains("Translate each item independently and return its original id."));
    assert!(work[0].prompt.contains("Keep all placeholders and tags unchanged in their original item."));
    assert!(work[0].prompt.contains("with no explanation."));
    assert_eq!(work[0].originals.0.input.as_ref().unwrap().text, "Earlier sentence. An unfinished");
    assert_eq!(work[0].originals.1.input.as_ref().unwrap().text, "<s2>sentence here. Another sentence</s2>");
    assert_eq!(work[0].candidate(BatchResponse::Cancelled, &patterns, &mut regex).unwrap(), Err(BatchFailure::Cancelled));
    assert_eq!(work[0].candidate(BatchResponse::Json("[]"), &patterns, &mut regex).unwrap(), Err(BatchFailure::InvalidResponse));
    assert!(cross_page_llm::plan_pairs(&[((0, 0), (1, 0)), ((0, 0), (2, 0))], &jobs, &mut regex, &patterns, 1, "Translate", "Chinese", &[]).unwrap().is_empty());
}
