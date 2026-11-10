use crate::{cross_page, ordinary_translation::{Job, RegexService}};
use babeldoc_ir_core::Document;
use babeldoc_p2_stages::translation::{
    cross_page_batch::{self, BatchFailure, BatchResponse},
    cross_page_sentence::{self, SentencePlan},
    cross_page_context::StyleAliases,
    cross_page_protocol::{self, WholeSentenceMove},
    glossary::Glossary,
    llm_only,
    token_count::count_tokens,
};
use pyo3::prelude::*;
use std::collections::HashMap;

pub(crate) fn reflow_response(
    plan: &WholeSentenceMove,
    response: &str,
    aliases: Option<&StyleAliases>,
    originals: &[babeldoc_p2_stages::translation::ordinary_translation::TranslateInput; 2],
    patterns: &[u64],
    regex: &mut RegexService<'_>,
) -> PyResult<Option<String>> {
    let decoded = match aliases {
        Some(aliases) => match aliases.decode(response, patterns, regex)? {
            Some(value) => value,
            None => return Ok(None),
        },
        None => response.to_owned(),
    };
    let moved = [plan.inputs.left.clone(), plan.inputs.right.clone()];
    let Some(outputs) = babeldoc_p2_stages::translation::cross_page_context::validate_pair(
        &decoded, &moved, patterns, regex,
    )? else { return Ok(None) };
    let Some((left, right)) = cross_page_protocol::reflow_whole_sentence_response(
        plan, &outputs[0], &outputs[1], &originals[0], &originals[1], patterns, regex,
    )? else { return Ok(None) };
    let response = serde_json::json!([
        {"id": 0, "output": left}, {"id": 1, "output": right},
    ]).to_string();
    if babeldoc_p2_stages::translation::cross_page_context::validate_pair(
        &response, originals, patterns, regex,
    )?.is_none() { return Ok(None); }
    Ok(Some(response))
}

pub(crate) fn is_context_pair(
    pairs: &[(cross_page::PageKey, cross_page::PageKey)],
    batch: &[cross_page::PageKey],
    jobs: &[Job],
    min_length: usize,
) -> bool {
    let [left, right] = batch else { return false };
    if left == right || pairs.iter().filter(|(first, second)| first == left && second == right).count() != 1
        || pairs.iter().filter(|(first, second)| first == left || second == left).count() != 1
        || pairs.iter().filter(|(first, second)| first == right || second == right).count() != 1 {
        return false;
    }
    [left, right].into_iter().all(|key| {
        let mut matches = jobs.iter().filter(|job| (job.page, job.paragraph) == *key);
        let Some(job) = matches.next() else { return false };
        matches.next().is_none() && job.eligible && !job.vertical && job.missing_font_style.is_none()
            && job.input.as_ref().is_some_and(|input| input.text.chars().count() >= min_length)
    })
}

pub(crate) fn prioritize_context_pairs(
    plans: &mut Vec<llm_only::BatchPlan>,
    pairs: &[(cross_page::PageKey, cross_page::PageKey)],
    jobs: &[Job],
    min_length: usize,
) {
    let mut next_id = plans.iter().map(|plan| plan.id).max().unwrap_or(0);
    let mut selected = Vec::new();
    for &(left, right) in pairs {
        if !is_context_pair(pairs, &[left, right], jobs, min_length) { continue; }
        let occurrences = |key| plans.iter().flat_map(|plan| &plan.paragraphs)
            .filter(|paragraph| (paragraph.page, paragraph.index) == key).count();
        if occurrences(left) != 1 || occurrences(right) != 1 { continue; }
        let affected = plans.iter().filter(|plan| plan.paragraphs.iter().any(|paragraph| {
            [(left.0, left.1), (right.0, right.1)].contains(&(paragraph.page, paragraph.index))
        }));
        if affected.flat_map(|plan| &plan.paragraphs).any(|paragraph| {
            jobs.iter().filter(|job| (job.page, job.paragraph) == (paragraph.page, paragraph.index)).count() != 1
        }) { continue; }
        let Some(id) = next_id.checked_add(1) else { break };
        next_id = id;
        let token_count = jobs.iter().filter(|job| [(left.0, left.1), (right.0, right.1)].contains(&(job.page, job.paragraph)))
            .map(|job| count_tokens(&job.text)).sum();
        for plan in plans.iter_mut() {
            let before = plan.paragraphs.len();
            plan.paragraphs.retain(|paragraph| ![left, right].contains(&(paragraph.page, paragraph.index)));
            if plan.paragraphs.len() != before {
                plan.token_count = plan.paragraphs.iter().map(|paragraph| {
                    let job = jobs.iter().find(|job| (job.page, job.paragraph) == (paragraph.page, paragraph.index)).expect("validated job");
                    count_tokens(&job.text)
                }).sum();
            }
        }
        plans.retain(|plan| !plan.paragraphs.is_empty());
        selected.push(llm_only::BatchPlan {
            kind: llm_only::GroupKind::CrossPage, id,
            paragraphs: vec![llm_only::ParagraphId { page: left.0, index: left.1 }, llm_only::ParagraphId { page: right.0, index: right.1 }],
            token_count,
        });
    }
    selected.append(plans);
    *plans = selected;
}

pub(crate) struct CandidateWork {
    pub(crate) originals: (Job, Job),
    plan: SentencePlan,
    pub(crate) prompt: String,
}

impl CandidateWork {
    pub(crate) fn candidate(&self, response: BatchResponse<'_>, patterns: &[u64], regex: &mut RegexService<'_>) -> PyResult<Result<(String, String), BatchFailure>> {
        cross_page_batch::candidate(&self.plan, response, patterns, regex)
    }
}

pub(crate) fn diagnostic_plan(
    document: &mut Document, config: &Bound<'_, PyAny>, jobs: &[Job],
    regex: &mut RegexService<'_>, patterns: &[u64], min_length: usize,
    role: &str, language: &str, glossaries: &[Glossary],
) -> PyResult<Vec<CandidateWork>> {
    let pairs = cross_page::find_pairs(document, config)?;
    plan_pairs(&pairs, jobs, regex, patterns, min_length, role, language, glossaries)
}

pub(crate) fn plan_pairs(
    pairs: &[(cross_page::PageKey, cross_page::PageKey)], jobs: &[Job],
    regex: &mut RegexService<'_>, patterns: &[u64], min_length: usize,
    role: &str, language: &str, glossaries: &[Glossary],
) -> PyResult<Vec<CandidateWork>> {
    let mut pair_counts = HashMap::new();
    for &(left, right) in pairs {
        *pair_counts.entry(left).or_insert(0usize) += 1;
        *pair_counts.entry(right).or_insert(0usize) += 1;
    }
    let mut result = Vec::new();
    for &(left_key, right_key) in pairs {
        if left_key == right_key || pair_counts[&left_key] != 1 || pair_counts[&right_key] != 1 { continue; }
        let mut left_jobs = jobs.iter().filter(|job| (job.page, job.paragraph) == left_key);
        let mut right_jobs = jobs.iter().filter(|job| (job.page, job.paragraph) == right_key);
        let (Some(left), Some(right)) = (left_jobs.next(), right_jobs.next()) else { continue };
        if left_jobs.next().is_some() || right_jobs.next().is_some() { continue; }
        if [left, right].iter().any(|job| !job.eligible || job.vertical || job.missing_font_style.is_some()) { continue; }
        let (Some(left_input), Some(right_input)) = (&left.input, &right.input) else { continue };
        if left_input.text.chars().count() < min_length || right_input.text.chars().count() < min_length { continue; }
        let (Some(left_ranges), Some(right_ranges)) = (
            cross_page::protected_ranges(regex, left_input, patterns)?,
            cross_page::protected_ranges(regex, right_input, patterns)?,
        ) else { continue };
        let Some(sentence) = cross_page_sentence::prepare(left_input, right_input, &left_ranges, &right_ranges, true, true, patterns, regex)? else { continue };
        let items = cross_page_batch::request(&sentence);
        let json = serde_json::to_string(&items.iter().map(|item| serde_json::json!({"id": item.id, "input": item.input})).collect::<Vec<_>>())
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
        let glossary_text = items.iter().map(|item| item.input.as_str()).collect::<Vec<_>>().join("\n");
        let role = format!("{role}\n\nThis request contains exactly three independent items. Item id 1 is one complete sentence; translate it as a whole. Translate each item independently and return its original id. Keep all placeholders and tags unchanged in their original item. Return only the requested JSON array, with no explanation.");
        let prompt = llm_only::render_batch_prompt(&role, language, &json, &glossary_text, left.title.as_ref().map(|(id, text)| (id.as_str(), text.as_str())), left.local_title.as_ref().map(|(id, text)| (id.as_str(), text.as_str())), glossaries);
        result.push(CandidateWork { originals: (left.clone(), right.clone()), plan: sentence, prompt });
    }
    Ok(result)
}
