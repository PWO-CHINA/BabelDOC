use crate::{
    callbacks::{CancelFlag, PythonService, Recorder},
    font_mapper::FontMapperHandle,
    ordinary_translation::{self as ordinary, RegexService},
    translation_service::TranslatorPoolPair,
};
use babeldoc_ir_core::Document;
use babeldoc_p2_stages::{
    runtime::shared_context::SharedContextRoot,
    translation::{
        cross_boundary as boundary, ordinary_translation as stage, token_count::count_tokens,
    },
};
use pyo3::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) struct ResultSet {
    pub outputs: Vec<boundary::ResolvedParagraph>,
    pub keys: HashSet<(usize, usize)>,
    pub decisions: Vec<Value>,
    pub unresolved: Vec<Value>,
    pub groups: Vec<ResolvedGroup>,
}

pub(crate) struct ResolvedGroup {
    pub id: usize,
    pub outputs: Vec<boundary::ResolvedParagraph>,
    pub locations: HashSet<(usize, usize)>,
}

pyo3::create_exception!(
    babeldoc,
    RecoverableBoundaryError,
    pyo3::exceptions::PyRuntimeError
);

pub(crate) fn recoverable(message: impl std::fmt::Display) -> PyErr {
    RecoverableBoundaryError::new_err(message.to_string())
}

fn error(message: impl std::fmt::Display) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(message.to_string())
}

fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn auxiliary(label: Option<&str>) -> bool {
    matches!(
        label,
        Some(
            "header"
                | "footer"
                | "page_number"
                | "page number"
                | "footnote"
                | "page_header"
                | "page_footer"
                | "page_header_hybrid"
                | "page_footer_hybrid"
                | "page_number_hybrid"
                | "line_number_hybrid"
        )
    )
}

fn complete_selection(
    parts: &mut Vec<boundary::SelectedPart>,
    joins: &mut Vec<boundary::Join>,
    sources: &[boundary::Source],
    group: &crate::cross_boundary_candidates::Boundary,
    exclusions: &HashSet<boundary::Location>,
) -> Result<(), String> {
    let ordered = group.left.iter().chain(&group.right).collect::<Vec<_>>();
    let first = parts.first_mut().ok_or("empty selection")?;
    first.range.start = boundary::sentences::enclosing_start(&first.source, first.range.start);
    let last = parts.last_mut().unwrap();
    last.range.end = boundary::sentences::enclosing_end(&last.source, last.range.end);
    for prepend in [true, false] {
        loop {
            let edge = if prepend {
                parts.first().unwrap()
            } else {
                parts.last().unwrap()
            };
            if prepend && !edge.source.text[..edge.range.start].trim().is_empty()
                || !prepend && boundary::sentences::has_sentence_end(&edge.source, &edge.range)
            {
                break;
            }
            let position = ordered
                .iter()
                .position(|candidate| {
                    candidate.page == edge.source.location.page
                        && candidate.paragraph == edge.source.location.paragraph
                })
                .ok_or("selected source missing from reading order")?;
            let next = if prepend {
                ordered[..position].iter().rev().find(|candidate| {
                    !crate::cross_boundary_candidates::non_body_label(candidate)
                        && !exclusions.contains(&boundary::Location {
                            page: candidate.page,
                            paragraph: candidate.paragraph,
                        })
                })
            } else {
                ordered[position + 1..].iter().find(|candidate| {
                    !crate::cross_boundary_candidates::non_body_label(candidate)
                        && !exclusions.contains(&boundary::Location {
                            page: candidate.page,
                            paragraph: candidate.paragraph,
                        })
                })
            };
            let Some(candidate) = next else {
                break;
            };
            if candidate.heading_barrier || auxiliary(candidate.label.as_deref()) {
                break;
            }
            let location = boundary::Location {
                page: candidate.page,
                paragraph: candidate.paragraph,
            };
            let Some(source) = sources.iter().find(|source| source.location == location) else {
                return Err(format!(
                    "sentence continuation at page {}, paragraph {} could not be reconstructed",
                    location.page, location.paragraph
                ));
            };
            if prepend && boundary::sentences::ends_sentence(source) {
                break;
            }
            let range = if prepend {
                boundary::sentences::enclosing_start(source, source.text.trim_end().len())
                    ..source.text.trim_end().len()
            } else {
                let start = source.text.len() - source.text.trim_start().len();
                start
                    ..boundary::sentences::enclosing_end(
                        source,
                        start
                            + source.text[start..]
                                .chars()
                                .next()
                                .map_or(0, char::len_utf8),
                    )
            };
            if range.is_empty() {
                break;
            }
            let (left, right) = if prepend {
                (
                    &source.text[range.clone()],
                    &edge.source.text[edge.range.clone()],
                )
            } else {
                (
                    &edge.source.text[edge.range.clone()],
                    &source.text[range.clone()],
                )
            };
            let join = if left.ends_with(['-', '‐', '\u{ad}'])
                && right.starts_with(char::is_alphabetic)
            {
                boundary::Join::Dehyphenate
            } else {
                boundary::Join::Space
            };
            let part = boundary::SelectedPart {
                source: source.clone(),
                range,
            };
            if prepend {
                parts.insert(0, part);
                joins.insert(0, join);
            } else {
                parts.push(part);
                joins.push(join);
            }
        }
    }
    for part in parts.iter() {
        if !part.source.valid_range(&part.range) {
            return Err("expanded sentence cuts a protected source object".into());
        }
    }
    for part in parts.iter().take(parts.len() - 1) {
        if boundary::sentences::has_sentence_end(&part.source, &part.range) {
            return Err("selection crosses a completed sentence".into());
        }
    }
    Ok(())
}

fn parse_selection(
    response: &str,
    sources: &[boundary::Source],
    group: &crate::cross_boundary_candidates::Boundary,
    left: &HashSet<boundary::Location>,
    right: &HashSet<boundary::Location>,
) -> Result<Option<boundary::DocumentSelection>, String> {
    let mut wire: Value =
        serde_json::from_str(response).map_err(|_| "selection is not valid JSON")?;
    let skipped = wire
        .as_object_mut()
        .ok_or("selection must be an object")?
        .remove("skipped")
        .unwrap_or(json!([]));
    let skipped = skipped.as_array().ok_or("skipped must be an array")?;
    let mut exclusions = HashSet::new();
    for item in skipped {
        let page = item["page"].as_u64().ok_or("skipped page missing")? as usize;
        let paragraph = item["paragraph"]
            .as_u64()
            .ok_or("skipped paragraph missing")? as usize;
        let reason = item["reason"].as_str().ok_or("skipped reason missing")?;
        if reason.trim().is_empty() {
            return Err("skipped reason must describe the non-body object".into());
        }
        if let Some(candidate) = group
            .left
            .iter()
            .chain(&group.right)
            .find(|candidate| candidate.page == page && candidate.paragraph == paragraph)
        {
            if !crate::cross_boundary_candidates::can_skip(group, candidate) {
                return Err(format!(
                    "page {page}, paragraph {paragraph} is body content and cannot be skipped without layout evidence"
                ));
            }
        }
        exclusions.insert(boundary::Location { page, paragraph });
    }
    let decision: boundary::Decision =
        serde_json::from_value(wire).map_err(|_| "invalid selection schema")?;
    if matches!(decision, boundary::Decision::Uncertain) {
        return Err("model could not decide whether the sentence continues".into());
    }
    if let boundary::Decision::Selected { ranges, .. } = &decision {
        for range in ranges {
            if !sources.iter().any(|source| {
                source.location.page == range.page && source.location.paragraph == range.paragraph
            }) {
                return Err(format!(
                    "page {}, paragraph {} is not selectable in this boundary; use only the supplied selectable candidates",
                    range.page, range.paragraph
                ));
            }
        }
    }
    let Some((mut parts, mut joins)) =
        boundary::select(decision, sources, left, right).map_err(str::to_owned)?
    else {
        return Ok(None);
    };
    let ordered = group.left.iter().chain(&group.right).collect::<Vec<_>>();
    let chosen = parts
        .iter()
        .map(|part| part.source.location)
        .collect::<HashSet<_>>();
    let begin = ordered
        .iter()
        .position(|item| {
            chosen.contains(&boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
        })
        .ok_or("missing start")?;
    let finish = ordered
        .iter()
        .rposition(|item| {
            chosen.contains(&boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
        })
        .ok_or("missing end")?;
    for candidate in &ordered[begin..=finish] {
        let location = boundary::Location {
            page: candidate.page,
            paragraph: candidate.paragraph,
        };
        if crate::cross_boundary_candidates::non_body_label(candidate)
            && !chosen.contains(&location)
        {
            exclusions.insert(location);
        }
    }
    complete_selection(&mut parts, &mut joins, sources, group, &exclusions)?;
    let selected_start = ordered
        .iter()
        .position(|candidate| {
            candidate.page == parts[0].source.location.page
                && candidate.paragraph == parts[0].source.location.paragraph
        })
        .unwrap();
    let final_part = parts.last().unwrap();
    let selected_end = ordered
        .iter()
        .position(|candidate| {
            candidate.page == final_part.source.location.page
                && candidate.paragraph == final_part.source.location.paragraph
        })
        .unwrap();
    for candidate in &ordered[selected_start..=selected_end] {
        if crate::cross_boundary_candidates::non_body_label(candidate) {
            let location = boundary::Location {
                page: candidate.page,
                paragraph: candidate.paragraph,
            };
            if !parts.iter().any(|part| part.source.location == location) {
                exclusions.insert(location);
            }
        }
    }
    crate::cross_boundary_candidates::validate_selection(
        group,
        &parts
            .iter()
            .map(|part| (part.source.location.page, part.source.location.paragraph))
            .collect::<Vec<_>>(),
        &exclusions
            .iter()
            .map(|location| (location.page, location.paragraph))
            .collect::<Vec<_>>(),
    )
    .map_err(str::to_owned)?;
    let selected = parts
        .iter()
        .map(|part| part.source.location)
        .collect::<HashSet<_>>();
    let first = ordered
        .iter()
        .position(|item| {
            selected.contains(&boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
        })
        .ok_or("missing selected start")?;
    let last = ordered
        .iter()
        .rposition(|item| {
            selected.contains(&boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
        })
        .ok_or("missing selected end")?;
    for candidate in &ordered[first..=last] {
        let location = boundary::Location {
            page: candidate.page,
            paragraph: candidate.paragraph,
        };
        if candidate.heading_barrier {
            return Err("selection crosses a section heading".into());
        }
        if selected.contains(&location) {
            continue;
        }
        if !auxiliary(candidate.label.as_deref()) && !exclusions.contains(&location) {
            return Err("selection skips an unexplained intervening object".into());
        }
    }
    Ok(Some(boundary::DocumentSelection { parts, joins }))
}

pub(crate) fn process(
    py: Python<'_>,
    document: &mut Document,
    jobs: &[ordinary::Job],
    config: &Bound<'_, PyAny>,
    root: &SharedContextRoot,
    translator: &Bound<'_, PyAny>,
    patterns: &stage::PlaceholderPatterns,
    mapper: &FontMapperHandle,
    dispatcher: Py<PyAny>,
    flag: &CancelFlag,
    allow_rich: bool,
) -> PyResult<ResultSet> {
    let mut result = ResultSet {
        outputs: vec![],
        keys: HashSet::new(),
        decisions: vec![],
        unresolved: vec![],
        groups: vec![],
    };
    let page_numbers = {
        let view = document.read();
        view.document()
            .page_iter(&view)
            .map_err(error)?
            .map(|page| {
                page.map_err(error)?
                    .page_number(&view)
                    .map(|value| value.and_then(|number| number.checked_add(1)))
                    .map_err(error)
            })
            .collect::<PyResult<Vec<_>>>()?
    };
    let workers = config
        .getattr("pool_max_workers")?
        .extract::<usize>()?
        .max(1);
    let request_limit = config
        .getattr("cross_boundary_max_input_tokens")
        .ok()
        .and_then(|value| value.extract::<usize>().ok())
        .unwrap_or(24576)
        .max(1024);
    let batch_limit = (request_limit / 6).max(256);
    let item_limit = request_limit * 2 / 3;
    let pool = Arc::new(TranslatorPoolPair::new(
        PythonService {
            recorder: Recorder::for_dispatcher(py, dispatcher.bind(py)),
            dispatcher,
        },
        "Translate boundary sentences",
        workers,
        root.clone(),
    ));
    let mut regex = RegexService {
        service: translator.clone(),
    };
    let ids = ordinary::cross_page_patterns(translator, &mut regex)?;
    let mut prepared = HashMap::new();
    let mut preparation_errors = HashMap::new();
    let mut selections = Vec::new();
    let mut seen_groups = HashSet::new();
    let groups = crate::cross_boundary_candidates::collect(document, config)?
        .into_iter()
        .filter_map(crate::cross_boundary_candidates::focus)
        .filter(|group| {
            let signature = |candidates: &[crate::cross_boundary_candidates::Candidate]| {
                candidates
                    .iter()
                    .filter(|candidate| {
                        !crate::cross_boundary_candidates::known_non_body(candidate)
                    })
                    .map(|candidate| (candidate.page, candidate.paragraph))
                    .collect::<Vec<_>>()
            };
            seen_groups.insert((
                format!("{:?}", group.kind),
                signature(&group.left),
                signature(&group.right),
            ))
        })
        .collect::<Vec<_>>();
    let group_count = groups.len();
    for (group_index, mut group) in groups.into_iter().enumerate() {
        flag.checkpoint()?;
        log::info!(
            "Analyzing translation boundary {}/{}",
            group_index + 1,
            group_count
        );
        let mut sources = Vec::new();
        let mut unavailable = Vec::new();
        for candidate in group.left.iter().chain(&group.right) {
            let key = (candidate.page, candidate.paragraph);
            if sources.iter().any(|source: &boundary::Source| {
                source.location
                    == boundary::Location {
                        page: key.0,
                        paragraph: key.1,
                    }
            }) {
                continue;
            }
            if !prepared.contains_key(&key) {
                let mut trial = jobs.to_vec();
                let built = ordinary::rebuild_boundary_inputs_diagnostic(
                    document,
                    &mut trial,
                    &[key],
                    translator,
                    &mut regex,
                    patterns,
                    allow_rich,
                    mapper,
                )?;
                if let Err(failure) = &built {
                    preparation_errors.insert(key, failure.reason.clone());
                }
                let source = if built.is_ok() {
                    trial
                        .iter()
                        .find(|job| (job.page, job.paragraph) == key)
                        .and_then(|job| job.input.as_ref())
                        .map(|input| {
                            boundary::Source::new(
                                boundary::Location {
                                    page: key.0,
                                    paragraph: key.1,
                                },
                                input,
                                &ids,
                                &mut regex,
                            )
                        })
                        .transpose()?
                        .flatten()
                } else {
                    None
                };
                if source.is_none() && !preparation_errors.contains_key(&key) {
                    preparation_errors.insert(
                        key,
                        "source placeholder ownership could not be reconstructed".to_owned(),
                    );
                }
                prepared.insert(key, source);
            }
            let selectable = crate::cross_boundary_candidates::selectable(&group, candidate);
            match prepared.get(&key).and_then(Option::as_ref) {
                Some(source) if !auxiliary(candidate.label.as_deref()) && selectable => {
                    sources.push(source.clone());
                }
                _ => unavailable.push(json!({"page":key.0,"paragraph":key.1,"reason":if auxiliary(candidate.label.as_deref()) { "auxiliary page object" } else if !selectable { "non-body or independent numbered paragraph" } else { preparation_errors.get(&key).map(String::as_str).unwrap_or("source unavailable") }})),
            }
        }
        let available = sources
            .iter()
            .map(|source| source.location)
            .collect::<HashSet<_>>();
        for candidate in group.left.iter_mut().chain(&mut group.right) {
            if let Some(source) = sources.iter().find(|source| {
                source.location
                    == boundary::Location {
                        page: candidate.page,
                        paragraph: candidate.paragraph,
                    }
            }) {
                candidate.text = source.text.clone();
            }
        }
        let left = group
            .left
            .iter()
            .map(|item| boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
            .filter(|location| available.contains(location))
            .collect::<HashSet<_>>();
        let right = group
            .right
            .iter()
            .map(|item| boundary::Location {
                page: item.page,
                paragraph: item.paragraph,
            })
            .filter(|location| available.contains(location))
            .collect::<HashSet<_>>();
        let locations = group
            .left
            .iter()
            .chain(&group.right)
            .map(|item| json!({"page":item.page,"paragraph":item.paragraph}))
            .collect::<Vec<_>>();
        let mut trace = json!({"kind":group.kind,"locations":locations,"unavailable":unavailable});
        for (name, candidates) in [
            ("left_candidates", &group.left),
            ("right_candidates", &group.right),
        ] {
            trace[name]=json!(candidates.iter().map(|candidate|json!({"page":candidate.page,"paragraph":candidate.paragraph,"source_text_sha256":digest(&candidate.text)})).collect::<Vec<_>>());
        }
        if left.is_empty() || right.is_empty() {
            trace["result"] = json!("no_translatable_pair");
            if !unavailable.is_empty() {
                result.unresolved.push(json!({"kind":group.kind,"reason":"boundary has no complete translatable pair","locations":locations}));
            }
            result.decisions.push(trace);
            continue;
        }
        let mut metadata = serde_json::to_value(&group).map_err(error)?;
        for page in metadata["context"].as_array_mut().unwrap() {
            for candidate in page["paragraphs"].as_array_mut().unwrap() {
                candidate.as_object_mut().unwrap().remove("text");
            }
        }
        for side in ["left", "right"] {
            for candidate in metadata[side].as_array_mut().unwrap() {
                let location = boundary::Location {
                    page: candidate["page"].as_u64().unwrap() as usize,
                    paragraph: candidate["paragraph"].as_u64().unwrap() as usize,
                };
                candidate["selectable"] = available.contains(&location).into();
                if !available.contains(&location) {
                    candidate["unavailable_reason"] = json!(
                        preparation_errors
                            .get(&(location.page, location.paragraph))
                            .map(String::as_str)
                            .unwrap_or("auxiliary page object")
                    );
                }
            }
        }
        let formulas = sources.iter().map(|source|json!({"page":source.location.page,"paragraph":source.location.paragraph,"formulas":source.formulas()})).collect::<Vec<_>>();
        let prompt = format!(
            "Select one unfinished body sentence continuing across this reading-order boundary within the same section. Follow reading_region order; do not jump over an intervening body candidate or a heading. Select text only from selectable=true candidates using exact unique substrings. A formula or number can be part of a sentence. Labels may be wrong; use text, geometry and context. Return JSON {{\"status\":\"none\"}} only when definitely separate, {{\"status\":\"uncertain\"}} when unclear, or {{\"status\":\"selected\",\"ranges\":[{{\"page\":0,\"paragraph\":0,\"text\":\"exact source suffix\"}}],\"joins\":[\"space\"],\"skipped\":[]}}. The first range MUST be from left and the last MUST be from right, never two ranges from the same side. Include the entire sentence from its main clause through its final predicate, conjunctions, references and punctuation. First range is a suffix and last range a prefix; a dependent clause or phrase alone is insufficient. The application expands partial sentence endpoints using source context. Joins: space, none, or dehyphenate only for an actual line-break hyphen. List every intervening non-body object you skip as {{\"page\":0,\"paragraph\":1,\"reason\":\"caption\"}}; reason is a nonempty description such as figure_label, axis_label, footer or caption, not a fixed enumeration. Never skip body content. Formula source characters explain markers; do not replace markers. Metadata: {} Formula markers and source characters: {}",
            metadata,
            json!(formulas)
        );
        let mut last_error = None;
        let mut previous_response = None;
        let mut selected = None;
        if count_tokens(&prompt) > request_limit {
            last_error = Some(format!(
                "boundary candidate request exceeds {request_limit} input tokens"
            ));
        } else {
            for attempt in 0..2 {
                let request = if let Some(reason) = &last_error {
                    format!(
                        "The previous selection was invalid: {reason}. Previous JSON: {}. Preserve valid source ranges and correct only the stated problem; the first range must be from left and last from right. Choose uncertain if it cannot be selected.\n{prompt}",
                        previous_response.as_deref().unwrap_or("")
                    )
                } else {
                    prompt.clone()
                };
                let response = match crate::llm_only_translation::boundary_request(
                    py,
                    pool.clone(),
                    flag.clone(),
                    &request,
                    "boundary-selection",
                ) {
                    Ok(response) => response,
                    Err(failure) if failure.is_instance_of::<RecoverableBoundaryError>(py) => {
                        last_error = Some(failure.to_string());
                        break;
                    }
                    Err(failure) => return Err(failure),
                };
                trace["selection_request_sha256"] = json!(digest(&request));
                trace["selection_response_sha256"] = json!(digest(&response));
                match parse_selection(&response, &sources, &group, &left, &right) {
                    Ok(value) => {
                        selected = Some(value);
                        break;
                    }
                    Err(reason) => {
                        last_error = Some(reason);
                        previous_response = Some(response.clone());
                        if response.contains("\"uncertain\"") || attempt == 1 {
                            break;
                        }
                    }
                }
            }
        }
        match selected {
            Some(Some(selection)) => {
                trace["result"] = json!("selected");
                let last = selection.parts.last().unwrap();
                if !boundary::sentences::has_sentence_end(&last.source, &last.range) {
                    trace["sentence_coverage"] = json!("available_paragraph_context");
                    trace["result"] = json!("fallback");
                    trace["reason"] =
                        json!("complete sentence is unavailable; retained ordinary translation");
                    result.unresolved.push(json!({"kind":group.kind,"reason":"complete sentence is unavailable; retained ordinary translation","locations":locations}));
                    result.decisions.push(trace);
                    continue;
                } else {
                    trace["sentence_coverage"] = json!("sentence_end_included");
                }
                trace["joins"] = json!(
                    selection
                        .joins
                        .iter()
                        .map(|join| match join {
                            boundary::Join::Space => "space",
                            boundary::Join::None => "none",
                            boundary::Join::Dehyphenate => "dehyphenate",
                        })
                        .collect::<Vec<_>>()
                );
                trace["ranges"] = json!(selection.parts.iter().map(|part|json!({"page":part.source.location.page,"paragraph":part.source.location.paragraph,"start_byte":part.range.start,"end_byte":part.range.end,"source_text_sha256":digest(&part.source.text)})).collect::<Vec<_>>());
                selections.push((result.decisions.len(), selection));
            }
            Some(None) => trace["result"] = json!("none"),
            None => {
                let reason = last_error.unwrap_or("selection unresolved".into());
                trace["result"] = json!("uncertain");
                trace["reason"] = json!(reason);
                result
                    .unresolved
                    .push(json!({"kind":group.kind,"reason":reason,"locations":locations}));
            }
        }
        result.decisions.push(trace);
    }
    if selections.is_empty() {
        attach_page_numbers(&mut result, &page_numbers);
        return Ok(result);
    }
    for (group_index, selections) in dependency_groups(selections).into_iter().enumerate() {
        let traces = selections
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>();
        let locations = selections
            .iter()
            .flat_map(|(_, selection)| &selection.parts)
            .map(|part| (part.source.location.page, part.source.location.paragraph))
            .collect::<HashSet<_>>();
        let mut ordered_locations = locations.iter().copied().collect::<Vec<_>>();
        ordered_locations.sort_unstable();
        let locations_json = ordered_locations
            .iter()
            .map(|(page, paragraph)| json!({"page":page,"paragraph":paragraph}))
            .collect::<Vec<_>>();
        let mut batches = Vec::new();
        let translated = (|| -> PyResult<Vec<boundary::ResolvedParagraph>> {
            let mut plan = boundary::DocumentPlan::new(
                selections
                    .into_iter()
                    .map(|(_, selection)| selection)
                    .collect(),
            )
            .map_err(recoverable)?;
            translate_group(
                py,
                document,
                &mut plan,
                config,
                root,
                pool.clone(),
                flag,
                request_limit,
                batch_limit,
                item_limit,
                group_index,
                &mut batches,
            )
        })();
        for &index in &traces {
            let trace = &mut result.decisions[index];
            trace["group"] = json!(group_index);
            trace["translation_batches"] = json!(batches);
            if let Some(last) = batches.last() {
                trace["translation_request_sha256"] = last["request_sha256"].clone();
                trace["translation_response_sha256"] = last["response_sha256"].clone();
            }
        }
        match translated {
            Ok(outputs) => {
                result.keys.extend(locations.iter().copied());
                result.outputs.extend(outputs.clone());
                result.groups.push(ResolvedGroup {
                    id: group_index,
                    outputs,
                    locations,
                });
            }
            Err(failure) if failure.is_instance_of::<RecoverableBoundaryError>(py) => {
                let reason = failure.to_string();
                for &index in &traces {
                    result.decisions[index]["result"] = json!("fallback");
                    result.decisions[index]["reason"] = json!(reason);
                }
                result.unresolved.push(json!({"kind":"enhancement","group":group_index,"reason":reason,"locations":locations_json}));
            }
            Err(failure) => return Err(failure),
        }
    }
    attach_page_numbers(&mut result, &page_numbers);
    Ok(result)
}

fn dependency_groups(
    selections: Vec<(usize, boundary::DocumentSelection)>,
) -> Vec<Vec<(usize, boundary::DocumentSelection)>> {
    let mut groups: Vec<Vec<(usize, boundary::DocumentSelection)>> = Vec::new();
    for selection in selections {
        let mut joined = vec![selection];
        let mut index = 0;
        while index < groups.len() {
            let shared = joined
                .iter()
                .flat_map(|(_, selection)| &selection.parts)
                .any(|part| {
                    groups[index]
                        .iter()
                        .flat_map(|(_, selection)| &selection.parts)
                        .any(|other| other.source.location == part.source.location)
                });
            if shared {
                joined.extend(groups.remove(index));
                index = 0;
            } else {
                index += 1;
            }
        }
        joined.sort_by_key(|(index, _)| *index);
        groups.push(joined);
    }
    groups.sort_by_key(|group| group[0].0);
    groups
}

fn translate_group(
    py: Python<'_>,
    document: &mut Document,
    plan: &mut boundary::DocumentPlan,
    config: &Bound<'_, PyAny>,
    root: &SharedContextRoot,
    pool: Arc<TranslatorPoolPair>,
    flag: &CancelFlag,
    request_limit: usize,
    batch_limit: usize,
    item_limit: usize,
    group_index: usize,
    batches: &mut Vec<Value>,
) -> PyResult<Vec<boundary::ResolvedParagraph>> {
    let capacities = {
        let view = document.read();
        let mut capacities = HashMap::new();
        for location in plan.locations() {
            let paragraph = view
                .document()
                .page_at(&view, location.page)
                .map_err(error)?
                .pdf_paragraph_at(&view, location.paragraph)
                .map_err(error)?;
            if let (Some(bounds), Some(style)) = (
                paragraph.r#box(&view).map_err(error)?,
                paragraph.pdf_style(&view).map_err(error)?,
            ) {
                if let (Some(left), Some(right), Some(bottom), Some(top), Some(size)) = (
                    bounds.x(&view).map_err(error)?,
                    bounds.x2(&view).map_err(error)?,
                    bounds.y(&view).map_err(error)?,
                    bounds.y2(&view).map_err(error)?,
                    style.font_size(&view).map_err(error)?,
                ) {
                    let capacity = (right - left) * (top - bottom) / (size * size * 1.3);
                    if size > 0.0 && capacity.is_finite() && capacity > 0.0 {
                        capacities.insert(location, capacity.round().max(1.0) as usize);
                    }
                }
            }
        }
        capacities
    };
    plan.set_capacities(capacities);
    let lang = config.getattr("lang_out")?.extract::<String>()?;
    let custom = config
        .getattr("custom_system_prompt")
        .ok()
        .and_then(|value| value.extract::<String>().ok())
        .unwrap_or_default();
    let auto = config
        .getattr("auto_extract_glossary")
        .ok()
        .and_then(|value| value.extract::<bool>().ok())
        .unwrap_or(false);
    let glossaries = root.get_glossaries_for_translation(auto);
    let title = root
        .first_paragraph()
        .and_then(|value| value.unicode)
        .unwrap_or_default();
    let all_formulas = plan.formula_context();
    let mut responses: Vec<Option<String>> = vec![None; plan.items.len()];
    let identifiers = plan
        .items
        .iter()
        .enumerate()
        .map(|(index, text)| format!("g{group_index}-i{index}-{}", &digest(text)[..12]))
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < plan.items.len() {
        flag.checkpoint()?;
        let mut end = index;
        let mut tokens = 0;
        while end < plan.items.len() {
            let count = count_tokens(&plan.items[end]);
            if end > index && tokens + count > batch_limit {
                break;
            }
            if count > item_limit {
                return Err(recoverable(
                    "A complete sentence exceeds the configured boundary request capacity; no source text was committed.",
                ));
            }
            tokens += count;
            end += 1;
        }
        for item in index..end {
            if plan.items[item].trim().is_empty() {
                responses[item] = Some(plan.items[item].clone());
            }
        }
        let mut reason = "initial request".to_owned();
        for attempt in 0..3 {
            let pending = (index..end)
                .filter(|item| responses[*item].is_none())
                .collect::<Vec<_>>();
            if pending.is_empty() {
                break;
            }
            let requests = if attempt == 2 {
                pending
                    .into_iter()
                    .map(|item| vec![item])
                    .collect::<Vec<_>>()
            } else {
                vec![pending]
            };
            for pending in requests {
                flag.checkpoint()?;
                let input_text = pending
                    .iter()
                    .map(|item| plan.items[*item].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let glossary = stage::glossary_block(&input_text, &glossaries);
                let formulas = json!(
                    all_formulas
                        .iter()
                        .filter(|(marker, _)| input_text.contains(marker))
                        .collect::<Vec<_>>()
                );
                let items = pending
                    .iter()
                    .map(|item| json!({"id":identifiers[*item],"text":plan.items[*item]}))
                    .collect::<Vec<_>>();
                let request = format!(
                    "Translate each JSON array item as a complete logical text into fluent {lang}. Return one output for every supplied id, preserving the exact id. Never combine items or invent IDs. Preserve every [[BABEL_...]] marker once, formula meaning, numeric expressions, negation and conjunction scope. Paired style markers remain paired and nonnested. Return only {{\"outputs\":[{{\"id\":\"same input id\",\"text\":\"translated text\"}}]}}. This request includes only currently missing or invalid items. Attempt: {attempt}; previous validation: {reason}. Custom translation requirements: {custom}\nTitle context: {title}\n{glossary}\nRead-only formula meanings: {formulas}\n Input: {}",
                    json!(items)
                );
                if count_tokens(&request) > request_limit {
                    reason = "boundary translation context exceeds request capacity".into();
                    continue;
                }
                let response = match crate::llm_only_translation::boundary_request(
                    py,
                    pool.clone(),
                    flag.clone(),
                    &request,
                    "boundary-translation",
                ) {
                    Ok(response) => response,
                    Err(failure) if failure.is_instance_of::<RecoverableBoundaryError>(py) => {
                        reason = failure.to_string();
                        continue;
                    }
                    Err(failure) => return Err(failure),
                };
                let expected = pending
                    .iter()
                    .map(|item| identifiers[*item].clone())
                    .collect::<Vec<_>>();
                let mut accepted_ids = Vec::new();
                match boundary::responses::parse(&response, &expected) {
                    Ok(outputs) => {
                        let mut failures = Vec::new();
                        for item in &pending {
                            let identifier = &identifiers[*item];
                            match outputs.get(identifier) {
                                Some(output) => {
                                    match plan.validate_items(*item, std::slice::from_ref(output)) {
                                        Ok(()) => {
                                            responses[*item] = Some(output.clone());
                                            accepted_ids.push(identifier.clone());
                                        }
                                        Err(failure) => {
                                            failures.push(format!("{identifier}: {failure}"))
                                        }
                                    }
                                }
                                None => failures.push(format!("missing {identifier}")),
                            }
                        }
                        reason = failures.join("; ");
                    }
                    Err(failure) => reason = failure,
                }
                batches.push(json!({"group":group_index,"attempt":attempt,"item_ids":expected,"accepted_ids":accepted_ids,
                    "request_sha256":digest(&request),"response_sha256":digest(&response)}));
            }
        }
        if (index..end).any(|item| responses[item].is_none()) {
            return Err(recoverable(format!(
                "Boundary translation failed validation: {reason}"
            )));
        }
        index = end;
    }
    let responses = responses
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| recoverable("boundary group has unresolved items"))?;
    plan.resolve(&responses).map_err(recoverable)
}

fn attach_page_numbers(result: &mut ResultSet, pages: &[Option<i64>]) {
    fn visit(value: &mut Value, pages: &[Option<i64>]) {
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, pages);
                }
            }
            Value::Object(values) => {
                if let Some(page) = values
                    .get("page")
                    .and_then(Value::as_u64)
                    .and_then(|page| pages.get(page as usize))
                    .copied()
                    .flatten()
                {
                    values.insert("original_page".into(), json!(page));
                }
                for value in values.values_mut() {
                    visit(value, pages);
                }
            }
            _ => {}
        }
    }
    for trace in result.decisions.iter_mut().chain(&mut result.unresolved) {
        visit(trace, pages);
    }
}
