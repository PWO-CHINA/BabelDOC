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
}

pub(crate) struct BaselineReplay {
    source: Document,
    jobs: Vec<ordinary::Job>,
    patterns: ordinary_stage::PlaceholderPatterns,
    outputs: Vec<Output>,
    hallucination_ids: Vec<u64>,
}

pub(crate) struct EnhancementCandidate {
    pub document: Option<Document>,
    pub report: serde_json::Value,
}

pub(crate) fn finish_enhancement_report(report: &mut serde_json::Value, applied: bool, reason: &str) {
    let groups = report["prepared_groups"].as_array().cloned().unwrap_or_default();
    for decision in report["decisions"].as_array_mut().into_iter().flatten() {
        if decision["result"] == "prepared" {
            decision["result"] = serde_json::json!(if applied { "applied" } else { "fallback" });
            if !applied { decision["reason"] = serde_json::json!(reason); }
        }
    }
    if !applied {
        for group in &groups {
            report["unresolved"].as_array_mut().expect("report unresolved array").push(serde_json::json!({
                "kind":"enhancement", "group":group["group"], "locations":group["locations"], "reason":reason
            }));
        }
        if !groups.is_empty() { report["status"] = serde_json::json!("needs_review"); }
    }
    report["applied_groups"] = serde_json::json!(if applied { groups.len() } else { 0 });
    report["candidate_state"] = serde_json::json!(if applied { "applied" } else { "fallback" });
}

pub(crate) fn write_enhancement_report(config: &Bound<'_, PyAny>, report: &serde_json::Value) {
    let result = (|| -> PyResult<()> {
        let path = config.call_method1("get_working_file_path", ("cross_boundary_report.json",))?
            .str()?.to_str()?.to_owned();
        let contents = serde_json::to_string(report).map_err(py_error)?;
        std::fs::write(path, contents).map_err(py_error)?;
        Ok(())
    })();
    if let Err(error) = result { log::warn!("Could not write cross-boundary report: {error}"); }
}

fn nearby_locations(document: &Document, locations: &[(usize, usize)]) -> PyResult<Vec<(usize, usize)>> {
    let artifact = document.artifact().map_err(py_error)?;
    let Some(pages) = artifact.get("document").and_then(|value| value.get("page")).and_then(|value| value.as_array()) else {
        return Ok(locations.to_vec());
    };
    let bounds = |paragraph: &serde_json::Value| -> Option<(f64, f64, f64, f64)> {
        let rect = paragraph.get("box")?;
        Some((rect.get("x")?.as_f64()?, rect.get("y")?.as_f64()?, rect.get("x2")?.as_f64()?, rect.get("y2")?.as_f64()?))
    };
    let mut nearby = locations.to_vec();
    for &(page, paragraph) in locations {
        let Some(paragraphs) = pages.get(page).and_then(|value| value.get("pdf_paragraph")).and_then(|value| value.as_array()) else { continue };
        let Some((left, bottom, right, top)) = paragraphs.get(paragraph).and_then(&bounds) else { continue };
        for (index, other) in paragraphs.iter().enumerate() {
            if nearby.contains(&(page, index)) { continue; }
            if let Some((other_left, other_bottom, other_right, other_top)) = bounds(other) {
                if left - 2.0 < other_right && other_left < right + 2.0
                    && bottom - 2.0 < other_top && other_bottom < top + 2.0 {
                    nearby.push((page, index));
                }
            }
        }
    }
    Ok(nearby)
}

fn safely_preserved_source(document: &Document, location: (usize, usize)) -> PyResult<bool> {
    let view = document.read();
    let paragraph = view.document().page_at(&view, location.0).map_err(py_error)?
        .pdf_paragraph_at(&view, location.1).map_err(py_error)?;
    let mut text = String::new();
    let mut found = false;
    for composition in paragraph.pdf_paragraph_composition_iter(&view).map_err(py_error)? {
        let composition = composition.map_err(py_error)?;
        found = true;
        if composition.pdf_formula(&view).map_err(py_error)?.is_some() {
            continue;
        }
        if let Some(group) = composition.pdf_same_style_unicode_characters(&view).map_err(py_error)? {
            let Some(value) = group.unicode(&view).map_err(py_error)? else { return Ok(false) };
            text.push_str(value);
            continue;
        }
        let characters = if let Some(line) = composition.pdf_line(&view).map_err(py_error)? {
            line.pdf_character_iter(&view).map_err(py_error)?.collect::<Result<Vec<_>, _>>().map_err(py_error)?
        } else if let Some(group) = composition.pdf_same_style_characters(&view).map_err(py_error)? {
            group.pdf_character_iter(&view).map_err(py_error)?.collect::<Result<Vec<_>, _>>().map_err(py_error)?
        } else if let Some(character) = composition.pdf_character(&view).map_err(py_error)? {
            vec![character]
        } else {
            return Ok(false);
        };
        for character in characters {
            let Some(value) = character.char_unicode(&view).map_err(py_error)? else { return Ok(false) };
            text.push_str(value);
        }
    }
    Ok(found && (text.trim().is_empty()
        || babeldoc_p2_stages::translation::paragraph_helpers::is_pure_numeric_text(&text)))
}

pub(crate) fn prepare_enhancements(
    py: Python<'_>,
    replay: BaselineReplay,
    config: &Bound<'_, PyAny>,
    shared_context: &Bound<'_, PyAny>,
    mapper: &FontMapperHandle,
    dispatcher: Py<PyAny>,
    translator_service: Py<PyAny>,
    flag: &CancelFlag,
    typesetting_config: &serde_json::Value,
    graphics_evidence: Option<&crate::boundary_apply::boundary_graphics::Evidence>,
) -> PyResult<EnhancementCandidate> {
    use babeldoc_p2_stages::translation::cross_boundary::OutputPart;
    use std::collections::HashSet;

    let root = crate::shared_context::root_from_handle(shared_context)?;
    let translator = translator_service.bind(py);
    let mut source = replay.source;
    let allow_rich = !config.getattr("disable_rich_text_translate")?.extract::<bool>()?;
    let mut result = crate::boundary_translation::process(
        py, &mut source, &replay.jobs, config, &root, translator, &replay.patterns,
        mapper, dispatcher, flag, allow_rich,
    )?;
    let (lang, has_progress, styles) =
        crate::production_stages::typesetting_configuration(typesetting_config)?;
    let typesetter = babeldoc_p2_stages::stages::typesetting::Typesetting::new(
        &lang, mapper.mapper.clone(), has_progress, styles,
    );
    let mut accepted = HashSet::new();
    let mut accepted_outputs = Vec::new();
    let mut accepted_groups = 0;
    let mut prepared_groups = Vec::new();
    for group in result.groups {
        let group_index = group.id;
        flag.checkpoint()?;
        let mut locations = group.locations.iter().copied().collect::<Vec<_>>();
        locations.sort_unstable();
        let location_json = locations.iter().map(|(page, paragraph)|
            serde_json::json!({"page":page,"paragraph":paragraph})).collect::<Vec<_>>();
        let outputs = group.outputs.iter().map(|output| (output.location.page, output.location.paragraph))
            .collect::<HashSet<_>>();
        let resources_within_group = group.outputs.iter().flat_map(|output| &output.parts).all(|part| {
            let resource = match part {
                OutputPart::Text { style, .. } => style.as_ref(),
                OutputPart::Formula(resource) => Some(resource),
            };
            resource.is_none_or(|resource| group.locations.contains(&(resource.location.page, resource.location.paragraph)))
        });
        let mut targets_have_baseline = true;
        for &location in &locations {
            if !replay.outputs.iter().any(|output|
                (output.job.page, output.job.paragraph) == location)
                && !safely_preserved_source(&source, location)? {
                targets_have_baseline = false;
                break;
            }
        }
        let invalid_group = outputs != group.locations || outputs.len() != group.outputs.len()
            || group.locations.iter().any(|location| accepted.contains(location))
            || !resources_within_group || !targets_have_baseline;
        let reason = if invalid_group {
            Some("boundary group has incomplete or overlapping source ownership".to_owned())
        } else {
            let mut proposed = source.fork();
            let mut combined_outputs = accepted_outputs.clone();
            combined_outputs.extend(group.outputs.iter().cloned());
            match crate::boundary_apply::apply_with_evidence(
                &mut proposed, &combined_outputs, mapper, graphics_evidence,
            ) {
                Err(error) if error.is_instance_of::<crate::boundary_apply::BoundaryApplyRejected>(py) => Some(error.to_string()),
                Err(error) => return Err(error),
                Ok(()) => {
                    let mut trial = proposed.fork();
                    for output in &replay.outputs {
                        flag.checkpoint()?;
                        let location = (output.job.page, output.job.paragraph);
                        if accepted.contains(&location) || group.locations.contains(&location) {
                            continue;
                        }
                        ordinary::apply_llm_output(&mut trial, location.0, location.1,
                            &output.input, &output.text, translator, &replay.hallucination_ids)?;
                    }
                    let neighbors = nearby_locations(&trial, &locations)?;
                    let mut context = babeldoc_p2_stages::StageContext::new(flag, &babeldoc_p2_stages::NoopProgress);
                    let previews = match py.detach(|| typesetter.audit_fork(&trial, &neighbors, &mut context)) {
                        Ok(previews) => previews,
                        Err(error) => {
                            flag.checkpoint()?;
                            if error == babeldoc_p2_stages::StageError::Cancelled {
                                return Err(crate::stage_error::to_python(py, &error));
                            }
                            result.unresolved.push(serde_json::json!({"kind":"enhancement", "group":group_index,
                                "reason":format!("candidate layout preview failed: {error}"), "locations":location_json}));
                            for decision in &mut result.decisions {
                                if decision.get("group").and_then(serde_json::Value::as_u64) == Some(group_index as u64) {
                                    decision["result"] = serde_json::json!("fallback");
                                    decision["reason"] = serde_json::json!(format!("candidate layout preview failed: {error}"));
                                }
                            }
                            continue;
                        }
                    };
                    flag.checkpoint()?;
                    use babeldoc_p2_stages::stages::typesetting::PreviewRejection;
                    let rejections = previews.iter().take(locations.len()).flat_map(|preview| &preview.rejections)
                        .filter(|reason| !matches!(reason, PreviewRejection::ObstacleAndClipEvidenceUnavailable
                            | PreviewRejection::GlyphResourceUnavailable | PreviewRejection::FormulaIdentityUnavailable))
                        .map(|reason| format!("{reason:?}")).collect::<Vec<_>>();
                    let formula_missing = previews.iter().take(locations.len())
                        .any(|preview| !preview.formulas.is_empty() && preview.formula_identities_match != Some(true));
                    let empty_target = previews.iter().take(locations.len())
                        .any(|preview| preview.glyphs.is_empty() && preview.formulas.is_empty() && preview.units.is_empty());
                    if !rejections.is_empty() || formula_missing || empty_target {
                        Some(format!("layout evidence insufficient: {}", rejections.join(", ")))
                    } else {
                        let targets = neighbors.iter().zip(&previews).map(|((page, paragraph), preview)|
                            crate::cross_boundary_preview::TargetPreview {
                                target: crate::cross_boundary_preview::TargetPosition { page: *page, paragraph: *paragraph },
                                preview,
                            }).collect::<Vec<_>>();
                        let overlaps = crate::cross_boundary_preview::inspect_target_overlaps_with_cancellation(
                            &targets, || flag.is_cancelled(),
                        );
                        match overlaps {
                            Err(_) => { flag.checkpoint()?; return Err(py_error("layout inspection cancelled")); }
                            Ok(report) if report.overlaps.iter().any(|overlap| {
                                group.locations.contains(&(overlap.first.target.page, overlap.first.target.paragraph))
                                    || group.locations.contains(&(overlap.second.target.page, overlap.second.target.paragraph))
                            }) || !report.uncertainties.is_empty() =>
                                Some("boundary target overlap or incomplete layout evidence".to_owned()),
                            Ok(_) => {
                                accepted.extend(group.locations.iter().copied());
                                accepted_outputs = combined_outputs;
                                accepted_groups += 1;
                                prepared_groups.push(serde_json::json!({"group":group_index,"locations":location_json}));
                                None
                            }
                        }
                    }
                }
            }
        };
        if let Some(reason) = reason {
            for decision in &mut result.decisions {
                if decision.get("group").and_then(serde_json::Value::as_u64) == Some(group_index as u64) {
                    decision["result"] = serde_json::json!("fallback");
                    decision["reason"] = serde_json::json!(reason);
                }
            }
            result.unresolved.push(serde_json::json!({"kind":"enhancement", "group":group_index,
                "reason":reason, "locations":location_json}));
        } else {
            for decision in &mut result.decisions {
                if decision.get("group").and_then(serde_json::Value::as_u64) == Some(group_index as u64) {
                    decision["result"] = serde_json::json!("prepared");
                }
            }
        }
    }
    let document = if accepted_groups == 0 {
        None
    } else {
        crate::boundary_apply::apply_with_evidence(&mut source, &accepted_outputs, mapper, graphics_evidence)?;
        for output in &replay.outputs {
            flag.checkpoint()?;
            let location = (output.job.page, output.job.paragraph);
            if !accepted.contains(&location) {
                ordinary::apply_llm_output(&mut source, location.0, location.1,
                    &output.input, &output.text, translator, &replay.hallucination_ids)?;
            }
        }
        Some(source)
    };
    Ok(EnhancementCandidate { document, report: serde_json::json!({"version":2,
        "status":if result.unresolved.is_empty() {"complete"} else {"needs_review"},
        "stage":"translation", "baseline_completed":true, "applied_groups":0,
        "candidate_state":if accepted_groups == 0 {"none"} else {"prepared"},
        "prepared_groups":prepared_groups,
        "decisions":result.decisions, "unresolved":result.unresolved}) })
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

pub fn boundary_typesetting_matches(
    translation_lang: &str,
    source_styles: Option<&HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>>,
    config: &serde_json::Value,
) -> PyResult<bool> {
    let (production_lang, progress, styles) = crate::production_stages::typesetting_configuration(config)?;
    let styles_observed = config.get("font_source_styles").and_then(serde_json::Value::as_object).is_some();
    let progress_observed = config.get("progress_monitor_present").and_then(serde_json::Value::as_bool) == Some(true);
    let debug = config.get("debug_information_before_typesetting").and_then(serde_json::Value::as_bool);
    Ok(production_lang == translation_lang && styles_observed && source_styles == Some(&styles)
        && progress && progress_observed && debug == Some(false))
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

fn provider_failure(error: &PyErr) -> bool {
    Python::attach(|py| {
        for (module, name) in [
            ("openai", "APIError"),
            ("httpx", "HTTPError"),
            ("requests", "RequestException"),
            ("babeldoc.tools.executor.translator", "ExecutorTranslatorError"),
            ("babeldoc.babeldoc_exception.BabelDOCException", "ContentFilterError"),
        ] {
            if let Ok(class) = py.import(module).and_then(|module| module.getattr(name)) {
                if error.is_instance(py, &class) {
                    return true;
                }
            }
        }
        if error.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
            || error.is_instance_of::<pyo3::exceptions::PyConnectionError>(py)
        {
            return true;
        }
        error.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py)
            && matches!(error.value(py).str().ok().and_then(|value| value.to_str().ok().map(str::to_owned)).as_deref(),
                Some("Cross-boundary model response was incomplete; retry or review this boundary.")
                | Some("Cross-boundary executor response was incomplete"))
    })
}

fn fatal_callback_error(error: &PyErr) -> bool {
    Python::attach(|py| {
        !error.is_instance_of::<pyo3::exceptions::PyException>(py)
            || py.import("concurrent.futures")
                .and_then(|module| module.getattr("CancelledError"))
                .is_ok_and(|class| error.is_instance(py, &class))
    })
}

pub(crate) fn boundary_request(
    py: Python<'_>, pool: Arc<TranslatorPoolPair>, flag: CancelFlag, prompt: &str,
    job: &str,
) -> PyResult<String> {
    flag.checkpoint()?;
    let (args, kwargs) = request_args(py, prompt, count_tokens(prompt), true)?;
    kwargs.bind(py).get_item("rate_limit_params")?.expect("rate limit parameters")
        .set_item("cross_boundary_request", true)?;
    kwargs.bind(py).get_item("rate_limit_params")?.expect("rate limit parameters")
        .set_item("cancel_flag", Py::new(py, flag.clone())?)?;
    let job = job.to_owned();
    callbacks::run(py, 1, move |runtime, _| {
        let request_flag = flag.clone();
        let result = runtime.block_on(pool.request("llm_translate", job.clone(), args, Some(kwargs), 0,
            move || !request_flag.is_cancelled()));
        flag.checkpoint()?;
        let value = result.map_err(|failure| match failure {
            crate::callbacks::CallbackError::Python(error) if provider_failure(&error) => {
                crate::boundary_translation::recoverable(format!("Boundary {job}: provider request failed"))
            }
            crate::callbacks::CallbackError::Python(error) => error,
            crate::callbacks::CallbackError::Bridge(message) => crate::callbacks::BridgeError::new_err(format!("Boundary {job}: {message}")),
        })?
            .ok_or_else(|| crate::callbacks::BridgeError::new_err("Boundary request was not admitted"))?;
        Python::attach(|py| value.bind(py).extract::<String>())
            .map_err(|_| crate::boundary_translation::recoverable("Cross-boundary response is not a string"))
    })
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

pub(crate) fn process_with_graphics_evidence(
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
    graphics_evidence: Option<crate::boundary_apply::boundary_graphics::Evidence>,
) -> PyResult<(usize, usize, usize)> {
    let enabled = config.getattr("experimental_cross_page_context").ok()
        .and_then(|value| value.extract::<bool>().ok()).unwrap_or(false);
    let (counts, replay) = process_baseline(py, document, config, shared_context, mapper,
        dispatcher_a.clone_ref(py), dispatcher_b, translator_service.clone_ref(py),
        flag.clone(), monitor, enabled)?;
    if let Some(replay) = replay {
        let mut styles = serde_json::Map::new();
        if let Some(source_styles) = mapper.source_styles() {
            for (id, style) in source_styles {
                styles.insert(id.clone(), serde_json::json!({"bold":style.bold,"italic":style.italic,
                    "monospaced":style.monospaced,"serif":style.serif}));
            }
        }
        let typesetting = serde_json::json!({"lang_out":config.getattr("lang_out")?.extract::<String>()?,
            "progress_monitor_present":true, "font_source_styles":styles});
        let mut candidate = prepare_enhancements(py, replay, config, shared_context, mapper,
            dispatcher_a, translator_service, &flag, &typesetting, graphics_evidence.as_ref())?;
        if let Some(enhanced) = candidate.document.take() {
            if flag.commit_when_live(|| { *document = enhanced; Ok::<_, PyErr>(()) })?.is_none() {
                flag.checkpoint()?;
            }
            finish_enhancement_report(&mut candidate.report, true, "");
        }
        candidate.report["scope"] = serde_json::json!("translation_only");
        write_enhancement_report(config, &candidate.report);
    }
    Ok(counts)
}

pub(crate) fn process_baseline(
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
    keep_replay: bool,
) -> PyResult<((usize, usize, usize), Option<BaselineReplay>)> {
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
    let source = keep_replay.then(|| document.fork());
    let plans = stage::plan_batches(&facts, min_length);
    let mut regex = RegexService {
        service: translator.clone(),
    };
    {
        let original_cross_page_pairs = plans
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
    }
    let replay_jobs = keep_replay.then(|| jobs.clone());
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
        let serialized = stage::batch_json(&json_items, formula_hints);
        let prompt = stage::render_batch_prompt(&roles, &lang, &serialized, &matching_text, title, local_title, &glossaries);
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
    let expected_outputs = works.iter().flat_map(|work| &work.jobs)
        .filter(|job| job.input.as_ref().is_some_and(|input| input.text.chars().count() >= min_length))
        .map(|job| (job.page, job.paragraph))
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
    let execution_patterns = patterns.clone();
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
                    request_args(py, &work.prompt, work.batch.token_count, true)
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
                            if fatal_callback_error(&error) {
                                return Err(error);
                            }
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
                let (response, tracker_output, request_error) = match response {
                    Some(Ok((value, tracker_output))) => (value, tracker_output, None),
                    Some(Err((error, tracker_output))) => (String::new(), tracker_output, Some(error)),
                    None => (String::new(), None, None),
                };
                let parsed = stage::interpret_response(&response, &inputs, false);
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
                        let Some(request) =
                            fallback_request(&job, formula_hints, llm_error.is_some())
                        else {
                            progress.report(1, None).map_err(|e| e.into_python())?;
                            continue;
                        };
                        let text = request.text.clone();
                        if text.chars().count() < min_length {
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
                                    }))
                                }
                                Ok(None) => Err(py_error("ordinary fallback request was not admitted")),
                                Err(crate::callbacks::CallbackError::Python(error)) => Err(error),
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
        flag.checkpoint()?;
        let completed = outputs.iter().map(|output| (output.job.page, output.job.paragraph))
            .collect::<std::collections::HashSet<_>>();
        if !expected_outputs.is_subset(&completed) {
            return Err(py_error("ordinary translation did not produce every requested paragraph"));
        }
        let hallucination_ids = Python::attach(|py| -> PyResult<Vec<u64>> {
            let mut regex = RegexService {
                service: translator_for_regex.bind(py).clone(),
            };
            [
                execution_patterns.formula.as_str(),
                execution_patterns.rich_left.as_str(),
                execution_patterns.rich_right.as_str(),
            ]
            .into_iter()
            .map(|pattern| regex.compile(pattern, true))
            .collect()
        })?;
        let replay_outputs = keep_replay.then(|| outputs.clone());
        let mut transaction = crate::cross_page_transaction::StageTransaction::new(document, true);
        let mut applied_outputs = Vec::new();
        for output in outputs {
            flag.checkpoint()?;
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
        let tracking = if write_tracking {
            Some(crate::cross_page_transaction::PreparedTracking::prepare(
                std::path::Path::new(&tracking_path), &result_tracking.to_json(),
            ).map_err(py_error)?)
        } else {
            None
        };
        progress.end(None).map_err(ProgressError::into_python)?;
        if flag.commit_when_live(|| {
            transaction.commit_with_tracking(tracking).map_err(py_error)
        })?.is_none() {
            flag.checkpoint()?;
        }
        Ok::<_, PyErr>((replay_outputs, hallucination_ids))
    });
    let (outputs, hallucination_ids) = result?;
    let replay = match (source, replay_jobs, outputs) {
        (Some(source), Some(jobs), Some(outputs)) => Some(BaselineReplay {
            source, jobs, patterns, outputs, hallucination_ids,
        }),
        _ => None,
    };
    Ok(((
        counters.0.load(Ordering::Relaxed),
        counters.1.load(Ordering::Relaxed),
        counters.2.load(Ordering::Relaxed),
    ), replay))
}

#[cfg(test)]
mod llm_only_translation_tests;
