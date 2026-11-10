//! Native LLM-only batch planner and translator bridge.
use crate::{
    callbacks::{self, CancelFlag, ProgressError, PythonService, Recorder},
    font_mapper::FontMapperHandle,
    ordinary_translation::{self as ordinary, RegexService},
    translation_service::TranslatorPoolPair,
};
use babeldoc_ir_core::Document;
use babeldoc_p2_stages::{
    ProgressReporter,
    runtime::shared_context::SharedContextRoot,
    translation::{
        cross_page_context,
        llm_only as stage,
        ordinary_translation::{self as ordinary_stage, PlaceholderRegex},
        token_count::count_tokens,
    },
};
use pyo3::{
    exceptions::PyTypeError,
    prelude::*,
    types::{PyDict, PyTuple},
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
struct Work {
    batch: stage::BatchPlan,
    jobs: Vec<ordinary::Job>,
    prompt: String,
    title_context: Option<(String, String)>,
    local_title_context: Option<(String, String)>,
    inputs: Vec<String>,
    prepare_error: Option<String>,
    admitted: Arc<AtomicBool>,
    tracker_group: usize,
    skipped_count: usize,
    cross_page_context: bool,
    context_sentence: Option<String>,
    context_aliases: Option<cross_page_context::StyleAliases>,
    whole_sentence: Option<babeldoc_p2_stages::translation::cross_page_protocol::WholeSentenceMove>,
    numeric_continuation: bool,
}

#[derive(Clone)]
struct Output {
    job: ordinary::Job,
    input: ordinary_stage::TranslateInput,
    text: String,
    fallback: bool,
    fallback_prompt: Option<String>,
    tracker_kind: stage::GroupKind,
    tracker_group: usize,
    tracker_index: usize,
    context_group: Option<usize>,
}

struct FallbackRequest {
    input: ordinary_stage::TranslateInput,
    text: String,
    formula_hint: Option<String>,
    paragraph_token_count: usize,
}

enum ScheduledWork {
    Checkpoint,
    Batch(Work),
}

fn py_error(error: impl std::fmt::Display) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(error.to_string())
}

fn fallback_request(
    job: &ordinary::Job,
    include_formula_hint: bool,
    batch_failed: bool,
) -> Option<FallbackRequest> {
    let input = job.fallback_input.clone()?;
    let paragraph_token_text = if batch_failed {
        &job.tracker_unicode
    } else {
        &job.text
    };
    Some(FallbackRequest {
        text: input.text.clone(),
        formula_hint: include_formula_hint
            .then(|| ordinary::formula_hint(&input))
            .flatten(),
        // A batch-level exception restores paragraph.unicode before counting;
        // an item-level response fallback counts it before the restore.
        paragraph_token_count: count_tokens(paragraph_token_text),
        input,
    })
}

fn request_args(
    py: Python<'_>,
    text: &str,
    count: usize,
    json_mode: bool,
) -> PyResult<(Py<PyTuple>, Py<PyDict>)> {
    let args = PyTuple::new(py, [text])?.unbind();
    let params = PyDict::new(py);
    params.set_item("paragraph_token_count", count)?;
    if json_mode {
        params.set_item("request_json_mode", true)?;
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("rate_limit_params", params)?;
    Ok((args, kwargs.unbind()))
}

fn pages_and_jobs(
    document: &mut Document,
    root: &SharedContextRoot,
    translator: &Bound<'_, PyAny>,
    patterns: &ordinary_stage::PlaceholderPatterns,
    allow_rich: bool,
    min_text_length: usize,
    mapper: &FontMapperHandle,
) -> PyResult<(
    Vec<Vec<stage::ParagraphFacts>>,
    Vec<ordinary::Job>,
    Vec<usize>,
)> {
    let mut regex = RegexService {
        service: translator.clone(),
    };
    let (jobs, page_lengths, _) = ordinary::collect(
        document,
        root,
        translator,
        &mut regex,
        patterns,
        allow_rich,
        min_text_length,
        true,
        mapper,
    )?;
    let mut pages = page_lengths
        .iter()
        .map(|length| vec![None; *length])
        .collect::<Vec<Vec<Option<stage::ParagraphFacts>>>>();
    for job in &jobs {
        pages[job.page][job.paragraph] = Some(stage::ParagraphFacts {
            debug_id: job.debug_id.clone(),
            unicode: job.unicode_present.then(|| job.text.clone()),
            layout_label: job.layout_label.clone(),
            cid: job.cid,
            pure_numeric: job.pure_numeric,
            placeholder_only: job.placeholder_only,
            box_y2: job.box_y2,
            title: job.title_paragraph,
        });
    }
    let facts = pages
        .into_iter()
        .map(|page| {
            page.into_iter()
                .map(|value| value.unwrap_or_default())
                .collect()
        })
        .collect();
    Ok((facts, jobs, page_lengths))
}

fn role_block(custom_prompt: Option<&str>, lang_out: &str) -> String {
    if let Some(prompt) = custom_prompt.filter(|value| !value.trim().is_empty()) {
        let mut value = prompt.trim().to_owned();
        if !value.contains("Follow all rules strictly.") {
            if !value.ends_with('\n') {
                value.push('\n');
            }
            value.push_str("Follow all rules strictly.");
        }
        value
    } else {
        format!(
            "You are a professional {lang_out} native translator who needs to fluently translate text into {lang_out}.\n\nFollow all rules strictly."
        )
    }
}

/// Run a complete LLM-only stage on the native document. The regular translator
/// stage remains separately callable for the fallback worker.
pub fn process(
    py: Python<'_>,
    document: &mut Document,
    config: &Bound<'_, PyAny>,
    shared_context: &Bound<'_, PyAny>,
    mapper: &FontMapperHandle,
    dispatcher_a: Py<PyAny>,
    dispatcher_b: Py<PyAny>,
    translator_service: Py<PyAny>,
    flag: CancelFlag,
    monitor: Py<PyAny>,
) -> PyResult<(usize, usize, usize)> {
    let _native_mapper = &mapper.mapper;
    let root = crate::shared_context::root_from_handle(shared_context)?;
    let initial_first = root.first_paragraph().map(|title| {
        (
            title.debug_id.unwrap_or_default(),
            ordinary::title_hint_text(title.unicode),
        )
    });
    let initial_recent = root.recent_title_paragraph().map(|title| {
        (
            title.debug_id.unwrap_or_default(),
            ordinary::title_hint_text(title.unicode),
        )
    });
    let translator = translator_service.bind(py).clone();
    translator.call_method0("probe_il_translator")?;
    let formula = ordinary::legacy_pattern(&translator, "get_formular_placeholder")?;
    let left = ordinary::legacy_pattern(&translator, "get_rich_text_left_placeholder")?;
    let right = ordinary::legacy_pattern(&translator, "get_rich_text_right_placeholder")?;
    let patterns = ordinary_stage::PlaceholderPatterns {
        formula,
        rich_left: left,
        rich_right: right,
    };
    let supports_llm = translator
        .call_method0("probe_il_translator")?
        .extract::<bool>()?;
    if !supports_llm {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "LLM translator not supported",
        ));
    }
    let disable_rich = config
        .getattr("disable_rich_text_translate")?
        .extract::<bool>()?;
    let min_length = config.getattr("min_text_length")?.extract::<usize>()?;
    let (facts, mut jobs, page_lengths) = pages_and_jobs(
        document,
        &root,
        &translator,
        &patterns,
        !disable_rich,
        min_length,
        mapper,
    )?;
    let first_title = jobs.iter().find(|job| job.title_paragraph).map(|job| {
        (
            job.debug_id.clone().unwrap_or_default(),
            ordinary::title_hint_text(job.unicode_present.then(|| job.text.clone())),
        )
    });
    let cross_title = initial_first.or_else(|| first_title.clone());
    let cross_recent = initial_recent.or(first_title);
    let context_enabled = config
        .getattr("experimental_cross_page_context")
        .ok()
        .and_then(|value| value.extract::<bool>().ok())
        .unwrap_or(false);
    let continuation_pairs = if context_enabled {
        crate::cross_page::find_pairs(document, config)?
    } else {
        Vec::new()
    };
    let mut plans = stage::plan_batches(&facts, min_length);
    let mut regex = RegexService {
        service: translator.clone(),
    };
    let context_patterns = if context_enabled {
        ordinary::cross_page_patterns(&translator, &mut regex).ok()
    } else {
        None
    };
    if context_enabled && context_patterns.is_none() {
        for &(left, right) in &continuation_pairs {
            let left_text = jobs.iter().find(|job| (job.page, job.paragraph) == left).map(|job| job.text.as_str());
            let right_text = jobs.iter().find(|job| (job.page, job.paragraph) == right).map(|job| job.text.as_str());
            if matches!((left_text, right_text), (Some(left), Some(right)) if babeldoc_p2_stages::translation::cross_page::numeric_continuation_needs_review(left, right)) {
                return Err(py_error("跨页数值续句无法校验占位符；请人工核对。"));
            }
        }
    }
    let original_cross_page_pairs = stage::plan_batches(&facts, min_length)
        .iter()
        .filter(|plan| plan.kind == stage::GroupKind::CrossPage)
        .filter_map(|plan| match plan.paragraphs.as_slice() {
            [left, right] => Some([(left.page, left.index), (right.page, right.index)]),
            _ => None,
        })
        .collect::<Vec<_>>();
    ordinary::rebuild_cross_page_inputs(
        document,
        &mut jobs,
        &original_cross_page_pairs,
        &translator,
        &mut regex,
        &patterns,
        !disable_rich,
        mapper,
    )?;
    let mut accepted_pairs = Vec::new();
    let mut context_checklists = HashMap::new();
    let mut context_sentences = HashMap::new();
    let mut context_aliases = HashMap::new();
    let mut whole_sentences = HashMap::new();
    let mut numeric_pairs = std::collections::HashSet::new();
    if context_patterns.is_some() {
        for &(left, right) in &continuation_pairs {
            let left_text = jobs.iter().find(|job| (job.page, job.paragraph) == left).map(|job| job.text.as_str());
            let right_text = jobs.iter().find(|job| (job.page, job.paragraph) == right).map(|job| job.text.as_str());
            if !matches!((left_text, right_text), (Some(left), Some(right)) if babeldoc_p2_stages::translation::cross_page::has_context_sentence_evidence(left, right)) {
                continue;
            }
            let numeric = babeldoc_p2_stages::translation::cross_page::numeric_continuation_needs_review(
                left_text.expect("eligible left text"), right_text.expect("eligible right text"),
            );
            if !crate::cross_page_llm::is_context_pair(&continuation_pairs, &[left, right], &jobs, min_length) {
                if numeric { return Err(py_error("跨页数值续句无法安排成对翻译；请人工核对。")); }
                continue;
            }
            let mut trial_jobs = jobs.clone();
            let ready = ordinary::rebuild_page_local_inputs(
                document, &mut trial_jobs, [left, right], &translator, &mut regex,
                &patterns, !disable_rich, mapper,
            ).unwrap_or(false);
            if !ready || !crate::cross_page_llm::is_context_pair(&continuation_pairs, &[left, right], &trial_jobs, min_length) {
                if numeric { return Err(py_error("跨页数值续句无法准备原页内容；请人工核对。")); }
                continue;
            }
            if [left, right].iter().any(|key| trial_jobs.iter().find(|job| (job.page, job.paragraph) == *key)
                .is_none_or(|job| job.fallback_missing_font_style.is_some()
                    || job.fallback_input.as_ref().is_none_or(|input| input.text.chars().count() < min_length))) {
                if numeric { return Err(py_error("跨页数值续句无法准备原页内容；请人工核对。")); }
                continue;
            }
            let pair_inputs = [left, right].map(|key| trial_jobs.iter().find(|job| (job.page, job.paragraph) == key)
                .and_then(|job| job.input.clone()).expect("eligible context input"));
            let Some(sentence) = babeldoc_p2_stages::translation::cross_page::extract_context_sentence(
                left_text.expect("eligible left text"), right_text.expect("eligible right text"),
            ) else {
                if numeric { return Err(py_error("跨页数值续句无法识别完整句子；请人工核对。")); }
                continue
            };
            let ids = context_patterns.as_ref().expect("context patterns");
            let whole_sentence = match crate::cross_page::protected_ranges(&mut regex, &pair_inputs[0], ids)? {
                Some(ranges) => {
                    let plan = babeldoc_p2_stages::translation::cross_page_protocol::plan_whole_sentence_reflow(
                        &pair_inputs[0], &pair_inputs[1], &ranges, ids, &mut regex,
                    )?;
                    match plan {
                        Some(plan) => Some(plan),
                        None => babeldoc_p2_stages::translation::cross_page_protocol::plan_plain_to_styled_sentence(
                            &pair_inputs[0], &pair_inputs[1], &ranges, &sentence, ids, &mut regex,
                        )?,
                    }
                },
                None => None,
            };
            if numeric && whole_sentence.is_none() {
                return Err(py_error("跨页续句包含跨页数值，当前无法可靠翻译整句；请关闭实验选项并人工核对该处。"));
            }
            if whole_sentence.is_none() { continue; }
            let request_inputs = whole_sentence.as_ref().map_or_else(|| pair_inputs.clone(), |plan| [plan.inputs.left.clone(), plan.inputs.right.clone()]);
            let aliases = cross_page_context::StyleAliases::prepare(
                &request_inputs, &sentence, ids, &mut regex,
            ).unwrap_or(None);
            let checklist_inputs = aliases.as_ref().map_or(&request_inputs, |value| &value.inputs);
            let Some(mut checklist) = cross_page_context::placeholder_checklist(
                checklist_inputs, context_patterns.as_ref().expect("context patterns"), &mut regex,
            ).unwrap_or(None) else {
                if numeric { return Err(py_error("跨页数值续句无法校验占位符；请人工核对。")); }
                continue
            };
            if aliases.is_some() {
                checklist.push_str("\nSome numbered placeholders mark style boundaries. Preserve each one exactly and in order, and translate the human-readable text between them.");
            }
            let extra_tokens = count_tokens(&format!("{checklist}\n\n"));
            let sentence_tokens = count_tokens(&cross_page_context::read_only_sentence_block(&sentence));
            let input_tokens = checklist_inputs.iter().map(|input| count_tokens(&input.text)).sum::<usize>();
            let mut trial_plans = plans.clone();
            crate::cross_page_llm::prioritize_context_pairs(
                &mut trial_plans, &[(left, right)], &trial_jobs, min_length,
            );
            let paired = trial_plans.iter_mut().find(|plan| {
                plan.kind == stage::GroupKind::CrossPage
                    && plan.paragraphs.as_slice() == [
                        stage::ParagraphId { page: left.0, index: left.1 },
                        stage::ParagraphId { page: right.0, index: right.1 },
                    ]
            });
            if let Some(plan) = paired {
                let Some(total) = cross_page_context::context_token_budget(plan.token_count.max(input_tokens), extra_tokens, sentence_tokens, 1_048_576) else {
                    if numeric { return Err(py_error("跨页数值续句超出翻译请求长度；请人工核对。")); }
                    continue
                };
                plan.token_count = total;
                jobs = trial_jobs;
                plans = trial_plans;
                accepted_pairs.push((left, right));
                context_checklists.insert((left, right), checklist);
                context_sentences.insert((left, right), sentence);
                if let Some(plan) = whole_sentence { whole_sentences.insert((left, right), plan); }
                if numeric { numeric_pairs.insert((left, right)); }
                if let Some(aliases) = aliases { context_aliases.insert((left, right), aliases); }
            } else if numeric {
                return Err(py_error("跨页数值续句无法安排成对翻译；请人工核对。"));
            }
        }
    }
    let workers = config
        .getattr("pool_max_workers")?
        .extract::<usize>()?
        .max(1);
    let lang = config.getattr("lang_out")?.extract::<String>()?;
    let custom = config
        .getattr("custom_system_prompt")
        .ok()
        .and_then(|v| v.extract::<String>().ok());
    let formula_hints = config
        .getattr("add_formula_placehold_hint")
        .and_then(|v| v.extract::<bool>())
        .unwrap_or(false);
    let auto_extract = config
        .getattr("auto_extract_glossary")
        .and_then(|v| v.extract::<bool>())
        .unwrap_or(false);
    let glossaries = root.get_glossaries_for_translation(auto_extract);
    let roles = role_block(custom.as_deref(), &lang);
    let mut by_id = HashMap::new();
    for job in jobs {
        by_id.insert((job.page, job.paragraph), job);
    }
    let mut works = Vec::new();
    let mut tracking = stage::DocumentTracking::new(&page_lengths);
    let mut kind_offsets = HashMap::<stage::GroupKind, usize>::new();
    for batch in plans {
        let work_jobs = batch
            .paragraphs
            .iter()
            .filter_map(|id| by_id.get(&(id.page, id.index)).cloned())
            .collect::<Vec<_>>();
        let mut inputs = Vec::new();
        let mut json_items = Vec::new();
        let mut skipped_count = 0;
        let mut prepare_error = None;
        for job in &work_jobs {
            if let Some(error) = &job.missing_font_style {
                prepare_error = Some(error.key_error_message());
                break;
            }
            let Some(input) = &job.input else {
                skipped_count += 1;
                continue;
            };
            if input.text.chars().count() < min_length {
                skipped_count += 1;
                continue;
            }
            let label = job.layout_label.clone();
            let hint = formula_hints
                .then(|| ordinary::formula_hint(input))
                .flatten();
            inputs.push(input.text.clone());
            json_items.push((input.text.clone(), label, hint));
        }
        let matching_text = json_items
            .iter()
            .map(|(input, _, _)| input.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let first = work_jobs.first();
        let last = work_jobs.last();
        let (title_value, local_title_value) = if batch.kind == stage::GroupKind::Page {
            (
                first.and_then(|job| job.title.as_ref()),
                last.and_then(|job| job.local_title.as_ref()),
            )
        } else {
            (cross_title.as_ref(), cross_recent.as_ref())
        };
        let title = title_value.map(|(id, text)| (id.as_str(), text.as_str()));
        let local_title = local_title_value.map(|(id, text)| (id.as_str(), text.as_str()));
        let keys = batch.paragraphs.iter().map(|id| (id.page, id.index)).collect::<Vec<_>>();
        let cross_page_context = context_enabled
            && context_patterns.is_some()
            && batch.kind == stage::GroupKind::CrossPage
            && prepare_error.is_none()
            && json_items.len() == 2
            && crate::cross_page_llm::is_context_pair(&accepted_pairs, &keys, &work_jobs, min_length);
        let aliases = if cross_page_context { context_aliases.get(&(keys[0], keys[1])).cloned() } else { None };
        let whole_sentence = if cross_page_context { whole_sentences.get(&(keys[0], keys[1])).cloned() } else { None };
        if let Some(plan) = &whole_sentence {
            for (item, input) in json_items.iter_mut().zip([&plan.inputs.left, &plan.inputs.right]) {
                item.0 = input.text.clone();
            }
        }
        if let Some(aliases) = &aliases {
            for (item, input) in json_items.iter_mut().zip(&aliases.inputs) {
                item.0 = input.text.clone();
            }
        }
        let serialized = stage::batch_json(&json_items, formula_hints);
        let prompt = if cross_page_context {
            let checklist = context_checklists.get(&(keys[0], keys[1])).expect("accepted context checklist");
            if whole_sentence.is_some() {
                cross_page_context::render_whole_sentence_batch_prompt_with_checklist(
                    &roles, &lang, &serialized, &matching_text, title, local_title, &glossaries, checklist,
                )
            } else {
                cross_page_context::render_context_batch_prompt_with_checklist(
                    &roles, &lang, &serialized, &matching_text, title, local_title, &glossaries, checklist,
                )
            }
        } else {
            stage::render_batch_prompt(&roles, &lang, &serialized, &matching_text, title, local_title, &glossaries)
        };
        let title_context = title_value.cloned();
        let local_title_context = local_title_value.cloned();
        let group = if batch.kind == stage::GroupKind::Page {
            batch.paragraphs.first().map_or(0, |id| id.page)
        } else {
            let offset = kind_offsets.entry(batch.kind).or_default();
            let result = *offset;
            *offset += 1;
            result
        };
        if batch.kind != stage::GroupKind::Page {
            tracking.new_group(batch.kind, batch.paragraphs.len());
        }
        let admitted = Arc::new(AtomicBool::new(false));
        works.push(Work {
            batch,
            jobs: work_jobs,
            prompt,
            title_context,
            local_title_context,
            inputs,
            prepare_error,
            admitted,
            tracker_group: group,
            skipped_count,
            cross_page_context,
            context_sentence: (cross_page_context && whole_sentence.is_none()).then(|| context_sentences.get(&(keys[0], keys[1])).cloned()).flatten(),
            context_aliases: aliases,
            numeric_continuation: cross_page_context && numeric_pairs.contains(&(keys[0], keys[1])),
            whole_sentence,
        });
        let _ = group;
    }
    let mut schedule = Vec::new();
    schedule.push(ScheduledWork::Checkpoint); // process_cross_page_paragraph entry
    schedule.extend(
        works
            .iter()
            .filter(|work| work.batch.kind == stage::GroupKind::CrossPage)
            .cloned()
            .map(ScheduledWork::Batch),
    );
    for page in 0..page_lengths.len() {
        schedule.push(ScheduledWork::Checkpoint); // process_cross_column_paragraph entry
        schedule.extend(
            works
                .iter()
                .filter(|work| {
                    work.batch.kind == stage::GroupKind::CrossColumn
                        && work
                            .batch
                            .paragraphs
                            .first()
                            .is_some_and(|id| id.page == page)
                })
                .cloned()
                .map(ScheduledWork::Batch),
        );
    }
    for page in 0..page_lengths.len() {
        schedule.push(ScheduledWork::Checkpoint); // process_page entry
        schedule.extend(
            works
                .iter()
                .filter(|work| {
                    work.batch.kind == stage::GroupKind::Page
                        && work
                            .batch
                            .paragraphs
                            .first()
                            .is_some_and(|id| id.page == page)
                })
                .cloned()
                .map(ScheduledWork::Batch),
        );
    }
    let planned = works
        .iter()
        .flat_map(|work| work.batch.paragraphs.iter().copied())
        .collect::<std::collections::HashSet<_>>();
    let pre_skipped = facts
        .iter()
        .enumerate()
        .flat_map(|(page, values)| {
            values
                .iter()
                .enumerate()
                .map(move |(index, value)| (page, index, value))
        })
        .filter(|(page, index, facts)| {
            facts.debug_id.is_some()
                && facts.unicode.is_some()
                && !planned.contains(&stage::ParagraphId {
                    page: *page,
                    index: *index,
                })
        })
        .count();
    let tracking_path = config
        .call_method1("get_working_file_path", ("translate_tracking.json",))?
        .str()?
        .to_str()?
        .to_owned();
    let write_tracking =
        config.getattr("debug")?.extract::<bool>()? || !config.getattr("working_dir")?.is_none();
    let recorder_a = Recorder::for_dispatcher(py, dispatcher_a.bind(py));
    let recorder_b = Recorder::for_dispatcher(py, dispatcher_b.bind(py));
    let a_service = PythonService {
        dispatcher: dispatcher_a,
        recorder: recorder_a,
    };
    let b_service = PythonService {
        dispatcher: dispatcher_b,
        recorder: recorder_b,
    };
    let pool_a = Arc::new(TranslatorPoolPair::new(
        a_service,
        stage::STAGE_NAME,
        workers,
        root.clone(),
    ));
    let pool_b = Arc::new(TranslatorPoolPair::new(
        b_service,
        stage::STAGE_NAME,
        workers,
        root,
    ));
    let translator_for_regex = translator_service.clone_ref(py);
    let counters = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let counters_out = counters.clone();
    let result = callbacks::run(py, workers, move |runtime, progress_run| {
        let progress = progress_run
            .start(
                monitor,
                stage::STAGE_NAME,
                stage::stage_total(&facts) as u64,
                Duration::from_millis(30),
            )
            .map_err(ProgressError::into_python)?;
        if pre_skipped > 0 {
            progress
                .report(pre_skipped as u64, None)
                .map_err(|error| error.into_python())?;
        }
        let mut result_tracking = tracking;
        let mut outputs = Vec::<Output>::new();
        let run = runtime.block_on(async {
            // A jobs are all submitted and joined before B is drained; fallback
            // requests are submitted to their independent equal-capacity pool.
            let mut a_tasks = tokio::task::JoinSet::new();
            let mut b_tasks = tokio::task::JoinSet::new();
            let mut by_batch = Vec::new();
            for scheduled in schedule {
                let work = match scheduled {
                    ScheduledWork::Checkpoint => {
                        flag.checkpoint()?;
                        continue;
                    }
                    ScheduledWork::Batch(work) => work,
                };
                if work.inputs.is_empty() {
                    if work.skipped_count > 0 {
                        progress
                            .report(work.skipped_count as u64, None)
                            .map_err(|e| e.into_python())?;
                    }
                    continue;
                }
                if let Some(error) = work.prepare_error.clone() {
                    work.admitted.store(true, Ordering::Release);
                    let prompt = work.prompt.clone();
                    let inputs = work.inputs.clone();
                    let id = by_batch.len();
                    a_tasks
                        .spawn(async move { Ok((id, prompt, inputs, Some(Err((error, None))))) });
                    by_batch.push(work);
                    continue;
                }
                let admitted = Arc::clone(&work.admitted);
                let req_flag = flag.clone();
                let (args, kwargs) = Python::attach(|py| {
                    let request_prompt = cross_page_context::add_read_only_sentence(&work.prompt, work.context_sentence.as_deref());
                    request_args(py, &request_prompt, work.batch.token_count, true)
                })?;
                let pool = Arc::clone(&pool_a);
                let prompt = work.prompt.clone();
                let batch = work.batch.clone();
                let inputs = work.inputs.clone();
                let id = by_batch.len();
                a_tasks.spawn(async move {
                    let response = pool
                        .request(
                            "llm_translate",
                            format!("llm-only:{}", batch.id),
                            args,
                            Some(kwargs),
                            1_048_576 - batch.token_count as i64,
                            move || {
                                if req_flag.is_cancelled() {
                                    return false;
                                }
                                admitted.store(true, Ordering::Release);
                                true
                            },
                        )
                        .await;
                    let value = match response {
                        Ok(Some(value)) => {
                            let raw = Python::attach(|py| {
                                value
                                    .bind(py)
                                    .str()
                                    .map(|value| value.to_string_lossy().into_owned())
                            });
                            match raw {
                                Err(error) => {
                                    let message = Python::attach(|py| {
                                        error
                                            .value(py)
                                            .str()
                                            .map(|message| message.to_string_lossy().into_owned())
                                    })
                                    .unwrap_or_else(|_| {
                                        "translator response conversion failed".into()
                                    });
                                    Err((message, None))
                                }
                                Ok(raw) => {
                                    match Python::attach(|py| -> PyResult<_> {
                                        value
                                            .bind(py)
                                            .call_method0("strip")?
                                            .extract::<String>()
                                            .map_err(Into::into)
                                    }) {
                                        Ok(value) => Ok((value, Some(raw))),
                                        Err(error) => {
                                            let message = Python::attach(|py| {
                                                error.value(py).str().map(|message| {
                                                    message.to_string_lossy().into_owned()
                                                })
                                            })
                                            .unwrap_or_else(|_| {
                                                "translator response strip failed".into()
                                            });
                                            Err((message, Some(raw)))
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => return Ok((id, prompt, inputs, None)),
                        Err(crate::callbacks::CallbackError::Python(error)) => {
                            let message = Python::attach(|py| {
                                error
                                    .value(py)
                                    .str()
                                    .map(|message| message.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|_| "translator request failed".into());
                            return Ok((id, prompt, inputs, Some(Err((message, None)))));
                        }
                        Err(crate::callbacks::CallbackError::Bridge(error)) => {
                            return Err(py_error(error));
                        }
                    };
                    Ok((id, prompt, inputs, Some(value)))
                });
                by_batch.push(work);
            }
            while let Some(joined) = a_tasks.join_next().await {
                let (batch_index, _prompt, inputs, response) =
                    joined.map_err(|e| py_error(e))??;
                let work = &by_batch[batch_index];
                if !work.admitted.load(Ordering::Acquire) {
                    continue;
                }
                if work.cross_page_context {
                    flag.checkpoint()?;
                }
                let request_succeeded = matches!(&response, Some(Ok(_)));
                let (mut response, mut tracker_output, mut request_error) = match response {
                    Some(Ok((value, tracker_output))) => (value, tracker_output, None),
                    Some(Err((error, tracker_output))) => {
                        (String::new(), tracker_output, Some(error))
                    }
                    None => (String::new(), None, None),
                };
                if work.cross_page_context && request_succeeded {
                    let structurally_valid = Python::attach(|py| -> PyResult<bool> {
                        let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                        let ids = ordinary::cross_page_patterns(translator_for_regex.bind(py), &mut regex)?;
                        let pair_inputs = [work.jobs[0].input.clone().expect("context input"), work.jobs[1].input.clone().expect("context input")];
                        if let Some(plan) = &work.whole_sentence {
                            Ok(crate::cross_page_llm::reflow_response(plan, &response, work.context_aliases.as_ref(), &pair_inputs, &ids, &mut regex)?.is_some())
                        } else if let Some(aliases) = &work.context_aliases {
                            Ok(aliases.decode(&response, &ids, &mut regex)?.is_some())
                        } else {
                            Ok(cross_page_context::validate_pair(&response, &pair_inputs, &ids, &mut regex)?.is_some())
                        }
                    })?;
                    let retry_count = work.batch.token_count.checked_add(count_tokens(cross_page_context::RETRY_INSTRUCTION))
                        .filter(|count| *count < 1_048_576);
                    if retry_count.is_some() && cross_page_context::should_retry(true, true, structurally_valid, false, flag.is_cancelled()) {
                        flag.checkpoint()?;
                        tracker_output = None;
                        let retry_prompt = format!("{}{}", cross_page_context::RETRY_INSTRUCTION, cross_page_context::add_read_only_sentence(&work.prompt, work.context_sentence.as_deref()));
                        let retry_count = retry_count.expect("checked retry budget");
                        let (args, kwargs) = Python::attach(|py| request_args(py, &retry_prompt, retry_count, true))?;
                        let retry_flag = flag.clone();
                        let retry = pool_a.request("llm_translate", format!("llm-only-retry:{}", work.batch.id), args, Some(kwargs), 1_048_576 - retry_count as i64, move || !retry_flag.is_cancelled()).await;
                        flag.checkpoint()?;
                        match retry {
                            Ok(Some(value)) => {
                                response = Python::attach(|py| value.bind(py).extract::<String>()).unwrap_or_default();
                                request_error = None;
                            }
                            Ok(None) | Err(crate::callbacks::CallbackError::Python(_)) => {
                                response.clear();
                                request_error = Some("Cross-page retry failed".into());
                            }
                            Err(crate::callbacks::CallbackError::Bridge(error)) => return Err(py_error(error)),
                        }
                    }
                    if !structurally_valid { tracker_output = None; }
                }
                if work.cross_page_context && request_error.is_some() {
                    request_error = Some("Cross-page request failed".into());
                    tracker_output = None;
                }
                if work.cross_page_context && request_error.is_none() {
                    if let Some(plan) = &work.whole_sentence {
                        response = Python::attach(|py| -> PyResult<Option<String>> {
                            let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                            let ids = ordinary::cross_page_patterns(translator_for_regex.bind(py), &mut regex)?;
                            let pair_inputs = [work.jobs[0].input.clone().expect("context input"), work.jobs[1].input.clone().expect("context input")];
                            crate::cross_page_llm::reflow_response(plan, &response, work.context_aliases.as_ref(), &pair_inputs, &ids, &mut regex)
                        })?.unwrap_or_default();
                    } else if let Some(aliases) = &work.context_aliases {
                        response = Python::attach(|py| -> PyResult<Option<String>> {
                            let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                            let ids = ordinary::cross_page_patterns(translator_for_regex.bind(py), &mut regex)?;
                            aliases.decode(&response, &ids, &mut regex)
                        })?.unwrap_or_default();
                    }
                }
                let mut parsed = stage::interpret_response(&response, &inputs, false);
                if work.cross_page_context {
                    let valid = if request_error.is_none()
                        && parsed.error.is_none()
                        && parsed.fallback_indices.is_empty()
                        && parsed.items.len() == 2
                    {
                        Python::attach(|py| -> PyResult<bool> {
                            let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                            let ids = ordinary::cross_page_patterns(translator_for_regex.bind(py), &mut regex)?;
                            let pair_inputs = [
                                work.jobs[0].input.clone().expect("context input"),
                                work.jobs[1].input.clone().expect("context input"),
                            ];
                            let Some(validated) = cross_page_context::validate_pair(&response, &pair_inputs, &ids, &mut regex)? else {
                                return Ok(false);
                            };
                            Ok(parsed.items.iter().all(|item| {
                                item.id < 2 && item.output == validated[item.id]
                            }))
                        })?
                    } else {
                        false
                    };
                    if !valid {
                        if work.numeric_continuation {
                            return Err(py_error("跨页数值续句未能完成整句翻译或拆分；请重试并人工核对。"));
                        }
                        tracker_output = None;
                        parsed.items.clear();
                        parsed.fallback_indices = (0..inputs.len()).collect();
                        parsed.fallback_errors.clear();
                        parsed.processed = 0;
                        parsed.ok_count = 0;
                        parsed.fallback_count = 0;
                        parsed.error = Some("Cross-page response validation failed".into());
                    }
                }
                let outer_error = request_error.is_some() || parsed.error.is_some();
                counters_out.0.fetch_add(parsed.ok_count, Ordering::Relaxed);
                counters_out.1.fetch_add(
                    parsed.fallback_count + if outer_error { inputs.len() } else { 0 },
                    Ordering::Relaxed,
                );
                counters_out.2.fetch_add(
                    parsed.processed + if outer_error { inputs.len() } else { 0 },
                    Ordering::Relaxed,
                );
                let group_index = work.tracker_group;
                let mut item_outputs = Vec::new();
                for item in parsed.items {
                    item_outputs.push((item.id, item.output));
                }
                let llm_error = request_error.or(parsed.error);
                let fallback_errors = parsed.fallback_errors;
                let fallback_indices = if llm_error.is_some() {
                    (0..inputs.len()).collect()
                } else {
                    parsed.fallback_indices
                };
                let mut input_position = 0usize;
                for (tracker_index, id) in work.batch.paragraphs.iter().enumerate() {
                    let Some(job) = work
                        .jobs
                        .iter()
                        .find(|job| job.page == id.page && job.paragraph == id.index)
                        .cloned()
                    else {
                        continue;
                    };
                    if job.missing_font_style.is_some() {
                        break;
                    }
                    let Some(input) = job.input.clone() else {
                        progress.report(1, None).map_err(|e| e.into_python())?;
                        continue;
                    };
                    if input.text.chars().count() < min_length {
                        let slot = match work.batch.kind {
                            stage::GroupKind::Page => {
                                &mut result_tracking.page[id.page].paragraph[id.index]
                            }
                            stage::GroupKind::CrossPage => {
                                &mut result_tracking.cross_page[work.tracker_group].paragraph
                                    [tracker_index]
                            }
                            stage::GroupKind::CrossColumn => {
                                &mut result_tracking.cross_column[work.tracker_group].paragraph
                                    [tracker_index]
                            }
                        };
                        slot.pdf_unicode = Some(job.text.clone());
                        slot.input = Some(input.text.clone());
                        slot.original_placeholders = input.original_tokens.clone();
                        slot.placeholders = ordinary::tracking_placeholders(&input);
                        progress.report(1, None).map_err(|e| e.into_python())?;
                        continue;
                    }
                    let pos = input_position;
                    input_position += 1;
                    let (tracker_kind, tracker_group) = if work.batch.kind == stage::GroupKind::Page
                    {
                        (work.batch.kind, id.page)
                    } else {
                        (work.batch.kind, group_index)
                    };
                    let slot = match tracker_kind {
                        stage::GroupKind::Page => {
                            &mut result_tracking.page[tracker_group].paragraph[id.index]
                        }
                        stage::GroupKind::CrossPage => {
                            &mut result_tracking.cross_page[tracker_group].paragraph[tracker_index]
                        }
                        stage::GroupKind::CrossColumn => {
                            &mut result_tracking.cross_column[tracker_group].paragraph
                                [tracker_index]
                        }
                    };
                    if slot.input.is_none() {
                        slot.pdf_unicode = Some(job.text.clone());
                        slot.input = Some(input.text.clone());
                        slot.original_placeholders = input.original_tokens.clone();
                        slot.placeholders = ordinary::tracking_placeholders(&input);
                        slot.multi_paragraph_id = Some(work.batch.id);
                        slot.multi_paragraph_index = Some(pos);
                        let mut llm_tracker = ordinary_stage::LlmTracking {
                            input: if work.prepare_error.is_some() {
                                String::new()
                            } else {
                                work.prompt.clone()
                            },
                            output: tracker_output.clone().unwrap_or_default(),
                            ..Default::default()
                        };
                        if let Some(error) = &llm_error {
                            llm_tracker.has_error = true;
                            llm_tracker.error_message =
                                format!("Error {error} during translation. try fallback");
                            llm_tracker.fallback_to_translate = true;
                        } else if let Some((_, error, full_match)) =
                            fallback_errors.iter().find(|(index, _, _)| *index == pos)
                        {
                            llm_tracker.has_error = true;
                            llm_tracker.error_message = error.clone();
                            llm_tracker.placeholder_full_match = *full_match;
                            llm_tracker.fallback_to_translate = true;
                        } else if fallback_indices.contains(&pos) {
                            llm_tracker.fallback_to_translate = true;
                        }
                        slot.llm_translate_trackers.push(llm_tracker);
                    }
                    let matching_outputs = item_outputs
                        .iter()
                        .filter(|(index, _)| *index == pos)
                        .map(|(_, text)| text.clone())
                        .collect::<Vec<_>>();
                    for text in matching_outputs {
                        outputs.push(Output {
                            job: job.clone(),
                            input: input.clone(),
                            text,
                            fallback: false,
                            fallback_prompt: None,
                            tracker_kind,
                            tracker_group,
                            tracker_index,
                            context_group: work.cross_page_context.then_some(group_index),
                        });
                        progress.report(1, None).map_err(|e| e.into_python())?;
                    }
                    let fallback_times = fallback_indices
                        .iter()
                        .filter(|index| **index == pos)
                        .count();
                    if fallback_times > 0 {
                        // Python's LLM-only tracker keeps paragraph.unicode for the
                        // direct request, then its ordinary fallback path rebuilds
                        // unicode from the paragraph compositions before tracking.
                        slot.pdf_unicode = Some(job.tracker_unicode.clone());
                        if let Some(fallback_input) = &job.fallback_input {
                            slot.input = Some(fallback_input.text.clone());
                            slot.original_placeholders = fallback_input.original_tokens.clone();
                            slot.placeholders = ordinary::tracking_placeholders(fallback_input);
                        }
                    }
                    for _ in 0..fallback_times {
                        if work.cross_page_context {
                            flag.checkpoint()?;
                        }
                        let Some(request) =
                            fallback_request(&job, formula_hints, llm_error.is_some())
                        else {
                            if work.cross_page_context {
                                return Err(py_error("Cross-page fallback input unavailable"));
                            }
                            progress.report(1, None).map_err(|e| e.into_python())?;
                            continue;
                        };
                        let text = request.text.clone();
                        if text.chars().count() < min_length {
                            if work.cross_page_context {
                                return Err(py_error("Cross-page fallback input too short"));
                            }
                            progress.report(1, None).map_err(|e| e.into_python())?;
                            continue;
                        }
                        let (fallback_title, fallback_local) = (
                            work.title_context.as_ref(),
                            work.local_title_context.as_ref(),
                        );
                        let context = ordinary_stage::context_block(
                            fallback_title.map(|(id, t)| (id.as_str(), t.as_str())),
                            fallback_local.map(|(id, t)| (id.as_str(), t.as_str())),
                            request.formula_hint.as_deref(),
                        );
                        let prompt = ordinary_stage::render_prompt(
                            &roles,
                            &lang,
                            &ordinary_stage::glossary_block(&text, &glossaries),
                            &context,
                            &text,
                        );
                        let fallback_token_count = request.paragraph_token_count;
                        let (args, kwargs) = Python::attach(|py| {
                            request_args(py, &prompt, fallback_token_count, false)
                        })?;
                        let pool = Arc::clone(&pool_b);
                        let flag = flag.clone();
                        let p = progress.clone();
                        let job_id = format!("fallback:{}:{}", job.page, job.paragraph);
                        let job_for_task = job.clone();
                        let input_for_task = request.input;
                        let fallback_prompt = prompt.clone();
                        let indices = (tracker_kind, tracker_group, tracker_index);
                        let context_group = work.cross_page_context.then_some(group_index);
                        b_tasks.spawn(async move {
                            let response = pool
                                .request(
                                    "llm_translate",
                                    job_id,
                                    args,
                                    Some(kwargs),
                                    1_048_576 - fallback_token_count as i64,
                                    move || !flag.is_cancelled(),
                                )
                                .await;
                            match response {
                                Ok(Some(value)) => {
                                    let text = Python::attach(|py| {
                                        value.bind(py).extract::<String>().map_err(|_| {
                                            PyTypeError::new_err(
                                                "fallback llm_translate response must be a string",
                                            )
                                        })
                                    })?;
                                    p.report(1, None).map_err(|e| e.into_python())?;
                                    Ok(Some(Output {
                                        job: job_for_task,
                                        input: input_for_task,
                                        text,
                                        fallback: true,
                                        fallback_prompt: Some(fallback_prompt),
                                        tracker_kind: indices.0,
                                        tracker_group: indices.1,
                                        tracker_index: indices.2,
                                        context_group,
                                    }))
                                }
                                Ok(None) if context_group.is_some() => Err(py_error("Cross-page fallback cancelled or not admitted")),
                                Err(crate::callbacks::CallbackError::Python(error)) if context_group.is_some() => Err(error),
                                Ok(None) => Ok(None),
                                Err(crate::callbacks::CallbackError::Python(_)) => Ok(None),
                                Err(crate::callbacks::CallbackError::Bridge(error)) => {
                                    Err(py_error(error))
                                }
                            }
                        });
                    }
                }
            }
            while let Some(joined) = b_tasks.join_next().await {
                if let Some(output) = joined.map_err(|e| py_error(e))?? {
                    outputs.push(output);
                }
            }
            Ok::<_, PyErr>(outputs)
        });
        let outputs = match run {
            Ok(value) => value,
            Err(error) => {
                let _ = progress.end(None);
                return Err(error);
            }
        };
        let hallucination_ids = Python::attach(|py| -> PyResult<Vec<u64>> {
            let mut regex = RegexService {
                service: translator_for_regex.bind(py).clone(),
            };
            [
                patterns.formula.as_str(),
                patterns.rich_left.as_str(),
                patterns.rich_right.as_str(),
            ]
            .into_iter()
            .map(|pattern| regex.compile(pattern, true))
            .collect()
        })?;
        let context_ids = if context_enabled {
            Python::attach(|py| -> PyResult<Option<Vec<u64>>> {
                let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                Ok(ordinary::cross_page_patterns(translator_for_regex.bind(py), &mut regex).ok())
            })?
        } else {
            None
        };
        if context_enabled {
            flag.checkpoint()?;
        }
        let mut context_groups: std::collections::BTreeMap<usize, Vec<Output>> = std::collections::BTreeMap::new();
        let mut ordinary_outputs = Vec::new();
        for output in outputs {
            if let Some(group) = output.context_group {
                context_groups.entry(group).or_default().push(output);
            } else {
                ordinary_outputs.push(output);
            }
        }
        let transactional_context = !context_groups.is_empty();
        let mut transaction = crate::cross_page_transaction::StageTransaction::new(document, transactional_context);
        let mut applied_outputs = Vec::new();
        for output in ordinary_outputs {
            let (text, full_match, removed) = Python::attach(|py| {
                ordinary::apply_llm_output(
                    transaction.document(),
                    output.job.page,
                    output.job.paragraph,
                    &output.input,
                    &output.text,
                    translator_for_regex.bind(py),
                    &hallucination_ids,
                )
            })?;
            applied_outputs.push((output, text, full_match, removed));
        }
        for (_, mut pair) in context_groups {
            pair.sort_by_key(|output| output.tracker_index);
            if pair.len() != 2 || pair[0].tracker_index != 0 || pair[1].tracker_index != 1
                || pair[0].fallback != pair[1].fallback {
                return Err(py_error("Cross-page response incomplete"));
            }
            flag.checkpoint()?;
            let ids = context_ids.as_deref().ok_or_else(|| py_error("Cross-page placeholder patterns unavailable"))?;
            let validated = Python::attach(|py| -> PyResult<bool> {
                let response = serde_json::json!([
                    {"id": 0, "output": ordinary_stage::normalize_response_punctuation(&pair[0].text)},
                    {"id": 1, "output": ordinary_stage::normalize_response_punctuation(&pair[1].text)},
                ]).to_string();
                let inputs = [pair[0].input.clone(), pair[1].input.clone()];
                let mut regex = RegexService { service: translator_for_regex.bind(py).clone() };
                Ok(cross_page_context::validate_pair(&response, &inputs, ids, &mut regex)?.is_some())
            })?;
            if !validated {
                return Err(py_error("Cross-page fallback validation failed"));
            }
            let mut applied_pair = Vec::new();
            for output in pair {
                flag.checkpoint()?;
                let (text, full_match, removed) = Python::attach(|py| {
                    ordinary::apply_llm_output(
                        transaction.document(),
                        output.job.page,
                        output.job.paragraph,
                        &output.input,
                        &output.text,
                        translator_for_regex.bind(py),
                        ids,
                    )
                })?;
                if !full_match || !removed.is_empty() {
                    return Err(py_error("Cross-page response could not be applied safely"));
                }
                applied_pair.push((output, text, full_match, removed));
            }
            flag.checkpoint()?;
            applied_outputs.extend(applied_pair);
        }
        for (output, text, full_match, removed) in applied_outputs {
            let slot = match output.tracker_kind {
                stage::GroupKind::Page => {
                    &mut result_tracking.page[output.tracker_group].paragraph[output.job.paragraph]
                }
                stage::GroupKind::CrossPage => {
                    &mut result_tracking.cross_page[output.tracker_group].paragraph
                        [output.tracker_index]
                }
                stage::GroupKind::CrossColumn => {
                    &mut result_tracking.cross_column[output.tracker_group].paragraph
                        [output.tracker_index]
                }
            };
            slot.output = Some(text);
            slot.removed_hallucinated_placeholders = removed;
            if output.fallback {
                slot.llm_translate_trackers
                    .push(ordinary_stage::LlmTracking {
                        input: output.fallback_prompt.unwrap_or_default(),
                        output: output.text.clone(),
                        placeholder_full_match: full_match || output.text == output.input.text,
                        ..Default::default()
                    });
            } else if let Some(last) = slot.llm_translate_trackers.last_mut() {
                last.placeholder_full_match = full_match || output.text == output.input.text;
            }
        }
        if transactional_context {
            let tracking = if write_tracking {
                Some(crate::cross_page_transaction::PreparedTracking::prepare(
                    std::path::Path::new(&tracking_path), &result_tracking.to_json(),
                ).map_err(py_error)?)
            } else {
                None
            };
            progress.end(None).map_err(ProgressError::into_python)?;
            flag.checkpoint()?;
            transaction.commit_with_tracking(tracking).map_err(py_error)?;
        } else {
            progress.end(None).map_err(ProgressError::into_python)?;
            if write_tracking {
                if let Some(parent) = std::path::Path::new(&tracking_path).parent() {
                    std::fs::create_dir_all(parent).map_err(py_error)?;
                }
                std::fs::write(&tracking_path, result_tracking.to_json()).map_err(py_error)?;
            }
            transaction.commit();
        }
        Ok::<_, PyErr>(())
    });
    result?;
    Ok((
        counters.0.load(Ordering::Relaxed),
        counters.1.load(Ordering::Relaxed),
        counters.2.load(Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod llm_only_translation_tests;
