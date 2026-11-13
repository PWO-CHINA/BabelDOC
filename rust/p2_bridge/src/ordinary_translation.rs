//! Ordinary paragraph translation over the native M document.
use crate::{
    callbacks::{self, CancelFlag, ProgressError, PythonProgress, PythonService, Recorder},
    font_mapper::FontMapperHandle,
    translation_service::TranslatorPoolPair,
};
use babeldoc_ir_core::{self as ir, Document, Permission, ReadOnlyView, TranslationView, TreeView};
use babeldoc_p2_stages::translation::ordinary_translation::PlaceholderRegex;
use babeldoc_p2_stages::{ProgressReporter, translation::ordinary_translation as stage};
use pyo3::{
    exceptions::{PyRuntimeError, PyTypeError},
    prelude::*,
    types::{PyDict, PyTuple},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

fn collect_children<T>(
    children: impl Iterator<Item = Result<T, babeldoc_ir_core::AccessError>>,
) -> PyResult<Vec<T>> {
    children.map(|child| child.map_err(access)).collect()
}

pub(crate) struct RegexService<'py> {
    pub(crate) service: Bound<'py, PyAny>,
}

pub(crate) fn title_hint_text(unicode: Option<String>) -> String {
    unicode.unwrap_or_else(|| "None".into())
}

fn initialize_first_title(
    first: &mut Option<(String, String)>,
    recent: &mut Option<(String, String)>,
    snapshot: babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot,
) -> Option<babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot> {
    if first.is_some() {
        return None;
    }
    let title = (
        snapshot.debug_id.clone().unwrap_or_default(),
        title_hint_text(snapshot.unicode.clone()),
    );
    *first = Some(title.clone());
    *recent = Some(title);
    Some(snapshot)
}

impl stage::PlaceholderRegex for RegexService<'_> {
    type Error = PyErr;

    fn compile(&mut self, pattern: &str, ignore_case: bool) -> PyResult<u64> {
        let flags = if ignore_case { 2 } else { 0 };
        self.service
            .call_method1("regex_compile", (pattern, flags))?
            .extract()
    }

    fn matches(&mut self, pattern: u64, text: &str) -> PyResult<bool> {
        Ok(!self
            .service
            .call_method1("regex_match", (pattern, text))?
            .is_none())
    }

    fn find_all(&mut self, pattern: u64, text: &str) -> PyResult<Vec<stage::RegexMatch>> {
        let values = self
            .service
            .call_method1("regex_finditer", (pattern, text))?;
        values
            .try_iter()?
            .map(|item| {
                let item = item?;
                let spans = item
                    .try_iter()?
                    .map(|span| {
                        let span = span?.cast_into::<PyTuple>()?;
                        let start = span.get_item(0)?.extract::<i64>()?;
                        let end = span.get_item(1)?.extract::<i64>()?;
                        Ok(if start < 0 || end < 0 {
                            None
                        } else {
                            Some((start as usize, end as usize))
                        })
                    })
                    .collect::<PyResult<Vec<_>>>()?;
                let Some((start, end)) = spans.first().and_then(|span| *span) else {
                    return Err(PyTypeError::new_err(
                        "Python regex returned no group zero span",
                    ));
                };
                let full = text.chars().skip(start).take(end - start).collect();
                Ok(stage::RegexMatch {
                    full,
                    start,
                    end,
                    groups: spans.into_iter().skip(1).collect(),
                })
            })
            .collect()
    }
}

fn placeholder_result(
    service: &Bound<'_, PyAny>,
    method: &str,
    id: &str,
) -> PyResult<(String, String)> {
    let value = service.call_method1(method, (id,))?;
    if let Ok(tuple) = value.cast::<PyTuple>() {
        Ok((tuple.get_item(0)?.extract()?, tuple.get_item(1)?.extract()?))
    } else {
        let token = value.extract::<String>()?;
        let pattern: String = service
            .py()
            .import("re")?
            .call_method1("escape", (&token,))?
            .extract()?;
        Ok((token, pattern))
    }
}

pub(crate) fn legacy_pattern(service: &Bound<'_, PyAny>, method: &str) -> PyResult<String> {
    let value = service.call_method1(method, (r"\d+",))?;
    if let Ok(tuple) = value.cast::<PyTuple>() {
        tuple.get_item(1)?.extract()
    } else {
        value
            .extract::<String>()?
            .chars()
            .nth(1)
            .map(|ch| ch.to_string())
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("string index out of range"))
    }
}

pub(crate) fn cross_page_patterns(service: &Bound<'_, PyAny>, regex: &mut RegexService<'_>) -> PyResult<Vec<u64>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for method in ["get_formular_placeholder", "get_rich_text_left_placeholder", "get_rich_text_right_placeholder"] {
        let value = service.call_method1(method, ("CROSSPAGEID",))?;
        let pattern = if let Ok(tuple) = value.cast::<PyTuple>() {
            let pattern: String = tuple.get_item(1)?.extract()?;
            pattern.replace("CROSSPAGEID", r"\d+")
        } else {
            let token: String = value.extract()?;
            if !token.contains("CROSSPAGEID") { return Err(PyTypeError::new_err("placeholder does not contain its identifier")); }
            let escaped: String = service.py().import("re")?.call_method1("escape", (token,))?.extract()?;
            escaped.replace("CROSSPAGEID", r"\d+")
        };
        let unique = if seen.insert(pattern.clone()) { pattern.as_str() } else { "(?!)" };
        ids.push(regex.compile(unique, true)?);
    }
    Ok(ids)
}

pub(crate) fn character_text(
    values: Vec<ir::Cursor<babeldoc_ir_core::types::PdfCharacter>>,
    view: &TreeView<'_, impl Permission>,
) -> PyResult<String> {
    let parts = character_fragments(values.into_iter().map(Ok), view)?;
    Ok(compose_input_text(&parts))
}

fn raw_character_text(
    characters: Vec<ir::Cursor<babeldoc_ir_core::types::PdfCharacter>>,
    view: &TreeView<'_, impl Permission>,
) -> PyResult<String> {
    characters
        .into_iter()
        .map(|character| {
            Ok(character
                .char_unicode(view)
                .map_err(access)?
                .clone()
                .unwrap_or_default())
        })
        .collect::<PyResult<Vec<_>>>()
        .map(|parts| parts.concat())
}

fn paragraph_unicode(
    paragraph: &ir::Cursor<babeldoc_ir_core::types::PdfParagraph>,
    view: &TreeView<'_, impl Permission>,
) -> PyResult<String> {
    let mut parts = Vec::new();
    for composition in paragraph
        .pdf_paragraph_composition_iter(view)
        .map_err(access)?
    {
        let composition = composition.map_err(access)?;
        if let Some(line) = composition.pdf_line(view).map_err(access)? {
            parts.extend(character_fragments(
                line.pdf_character_iter(view).map_err(access)?,
                view,
            )?);
        } else if let Some(same) = composition
            .pdf_same_style_characters(view)
            .map_err(access)?
        {
            parts.extend(character_fragments(
                same.pdf_character_iter(view).map_err(access)?,
                view,
            )?);
        } else if let Some(same) = composition
            .pdf_same_style_unicode_characters(view)
            .map_err(access)?
        {
            if let Some(unicode) = same.unicode(view).map_err(access)? {
                parts.push(InputTextPart::Text(unicode.clone()));
            }
        } else if let Some(formula) = composition.pdf_formula(view).map_err(access)? {
            parts.extend(character_fragments(
                formula.pdf_character_iter(view).map_err(access)?,
                view,
            )?);
        } else if let Some(character) = composition.pdf_character(view).map_err(access)? {
            parts.extend(character_fragments(std::iter::once(Ok(character)), view)?);
        }
    }
    Ok(compose_input_text(&parts))
}

fn instruction_equivalent(
    left: &Option<ir::Instructions>,
    right: &Option<ir::Instructions>,
    view: &babeldoc_ir_core::TreeView<'_, ReadOnlyView>,
) -> PyResult<bool> {
    Ok(view.render_instructions(left).map_err(access)?
        == view.render_instructions(right).map_err(access)?)
}

fn style_matches(
    left: Option<ir::Cursor<babeldoc_ir_core::types::PdfStyle>>,
    right: Option<ir::Cursor<babeldoc_ir_core::types::PdfStyle>>,
    view: &babeldoc_ir_core::TreeView<'_, ReadOnlyView>,
    except_size: bool,
    except_font: bool,
    mapper: &crate::font_mapper::FontMapperHandle,
    font_styles: &HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>,
) -> PyResult<bool> {
    if left.is_none() || right.is_none() {
        return Ok(left.is_none() && right.is_none());
    }
    let left = left.expect("checked Some");
    let right = right.expect("checked Some");
    let left_id = left.font_id(view).map_err(access)?;
    let right_id = right.font_id(view).map_err(access)?;
    let left_size = left.font_size(view).map_err(access)?.unwrap_or_default();
    let right_size = right.font_size(view).map_err(access)?.unwrap_or_default();
    let left_graphic = left.graphic_state(view).map_err(access)?;
    let right_graphic = right.graphic_state(view).map_err(access)?;
    let same_graphic = match (left_graphic, right_graphic) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            let left = left
                .passthrough_per_char_instruction(view)
                .map_err(access)?;
            let right = right
                .passthrough_per_char_instruction(view)
                .map_err(access)?;
            instruction_equivalent(left, right, view)?
        }
        _ => false,
    };
    let size_match = if except_size {
        let ratio = (left_size / right_size).abs();
        0.7 < ratio && ratio < 1.3
    } else {
        (left_size - right_size).abs() < 0.02
    };
    let font_match = if except_font {
        let source = |id: Option<String>| {
            id.and_then(|id| font_styles.get(&id).copied())
                .unwrap_or_default()
        };
        let left_mapped = mapper.mapped_font_id(
            source(left_id.clone()).bold,
            source(left_id.clone()).italic,
            source(left_id.clone()).monospaced,
            source(left_id.clone()).serif,
            '1',
        );
        let right_mapped = mapper.mapped_font_id(
            source(right_id.clone()).bold,
            source(right_id.clone()).italic,
            source(right_id.clone()).monospaced,
            source(right_id.clone()).serif,
            '1',
        );
        left_mapped.is_some() && left_mapped == right_mapped
    } else {
        left_id == right_id
    };
    Ok(font_match && size_match && same_graphic)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FontStyleLookupError {
    MissingId,
    MissingKey(String),
}

fn require_font_style(
    font_styles: &HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>,
    font_id: Option<String>,
) -> Result<babeldoc_p2_stages::font_mapper::SourceFontStyle, FontStyleLookupError> {
    let Some(font_id) = font_id else {
        return Err(FontStyleLookupError::MissingId);
    };
    font_styles
        .get(&font_id)
        .copied()
        .ok_or(FontStyleLookupError::MissingKey(font_id))
}

impl FontStyleLookupError {
    pub(crate) fn key_error_message(&self) -> String {
        match self {
            Self::MissingId => "None".into(),
            Self::MissingKey(font_id) => python_string_repr(font_id),
        }
    }
}

pub(crate) fn build_input(
    paragraph: &ir::Cursor<babeldoc_ir_core::types::PdfParagraph>,
    view: &babeldoc_ir_core::TreeView<'_, ReadOnlyView>,
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &crate::font_mapper::FontMapperHandle,
    font_styles: &HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>,
    unicode_override: Option<&str>,
    deferred_missing_font_style: &mut Option<FontStyleLookupError>,
) -> PyResult<Option<stage::TranslateInput>> {
    build_input_impl(
        paragraph, view, service, regex, patterns, allow_rich, mapper, font_styles,
        unicode_override, deferred_missing_font_style, None, &mut None,
    )
}

fn build_input_impl(
    paragraph: &ir::Cursor<babeldoc_ir_core::types::PdfParagraph>,
    view: &babeldoc_ir_core::TreeView<'_, ReadOnlyView>,
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &crate::font_mapper::FontMapperHandle,
    font_styles: &HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>,
    unicode_override: Option<&str>,
    deferred_missing_font_style: &mut Option<FontStyleLookupError>,
    boundary_patterns: Option<&[u64]>,
    boundary_failure: &mut Option<String>,
) -> PyResult<Option<stage::TranslateInput>> {
    let boundary = boundary_patterns.is_some();
    let composition_len = paragraph
        .pdf_paragraph_composition_len(view)
        .map_err(access)?;
    if composition_len == 0 {
        *boundary_failure = Some("paragraph has no source compositions".into());
        return Ok(None);
    }
    let unicode = unicode_override
        .map_or_else(
            || {
                paragraph
                    .unicode(view)
                    .map(|value| value.clone())
                    .map_err(access)
            },
            |value| Ok(Some(value.to_owned())),
        )?
        .unwrap_or_default();
    if let Some(boundary_patterns) = boundary_patterns {
        if !boundary_source_unambiguous("", &unicode, boundary_patterns, regex)? {
            *boundary_failure = Some("paragraph contains a literal placeholder or reserved marker".into());
            return Ok(None);
        }
    }
    if !boundary && babeldoc_p2_stages::translation::paragraph_helpers::is_pure_numeric_text(&unicode) {
        return Ok(None);
    }
    let mut formula_only = composition_len != 0;
    for composition in paragraph
        .pdf_paragraph_composition_iter(view)
        .map_err(access)?
    {
        *boundary_failure = Some("paragraph contains only whitespace".into());
        let composition = composition.map_err(access)?;
        formula_only &= composition.pdf_formula(view).map_err(access)?.is_some();
    }
    if (!boundary && formula_only)
        || (!unicode.is_empty()
            && babeldoc_p2_stages::translation::python_semantics::python_isspace(&unicode))
    {
        return Ok(None);
    }
    let originals =
        stage::original_placeholder_tokens(regex, patterns, &unicode).map_err(access)?;
    if !boundary && composition_len == 1 {
        let composition = paragraph
            .pdf_paragraph_composition_at(view, 0)
            .map_err(access)?;
        let plain = composition.pdf_line(view).map_err(access)?.is_some()
            || composition
                .pdf_same_style_characters(view)
                .map_err(access)?
                .is_some()
            || composition.pdf_character(view).map_err(access)?.is_some();
        if plain {
            return Ok(Some(stage::TranslateInput {
                text: unicode,
                placeholders: vec![],
                original_tokens: originals,
            }));
        }
        if composition.pdf_formula(view).map_err(access)?.is_some()
            || composition
                .pdf_same_style_unicode_characters(view)
                .map_err(access)?
                .is_some()
        {
            return Ok(None);
        }
        return Ok(None);
    }

    let mut parts = Vec::new();
    let mut placeholders = Vec::new();
    let mut next_id = 1i64;
    for (index, composition) in paragraph
        .pdf_paragraph_composition_iter(view)
        .map_err(access)?
        .enumerate()
    {
        let composition = composition.map_err(access)?;
        if let Some(line) = composition.pdf_line(view).map_err(access)? {
            parts.extend(input_character_fragments(
                line.pdf_character_iter(view).map_err(access)?,
                view,
                boundary,
            )?);
        } else if let Some(formula) = composition.pdf_formula(view).map_err(access)? {
            let formula_characters =
                collect_children(formula.pdf_character_iter(view).map_err(access)?)?;
            let cid_count = formula_characters
                .iter()
                .map(|character| character.char_unicode(view).map_err(access))
                .collect::<PyResult<Vec<_>>>()?
                .iter()
                .filter_map(|value| value.as_deref())
                .filter(|value| is_cid_placeholder(value))
                .count();
            let hint_source = cid_count as f64 <= formula_characters.len() as f64 * 0.8
                && (!boundary || (!formula_characters.is_empty() && formula_characters.iter().all(|character| {
                    character.char_unicode(view).ok().and_then(|value| value.as_deref())
                        .is_some_and(|text| !text.is_empty())
                })));
            let source_chars = if boundary {
                raw_character_text(formula_characters.clone(), view)?
            } else {
                character_text(formula_characters.clone(), view)?
            };
            let placeholder = stage::formula_placeholder(
                regex,
                &unicode,
                next_id,
                index,
                |id| placeholder_result(service, "get_formular_placeholder", &id.to_string()),
                source_chars,
                hint_source,
            )
            .map_err(access)?;
            next_id = placeholder.id + 1;
            if boundary {
                let characters = input_character_fragments(formula_characters.into_iter().map(Ok), view, true)?;
                let formula_box = if let Some(rect) = formula.r#box(view).map_err(access)? {
                    match (rect.x(view).map_err(access)?, rect.y(view).map_err(access)?,
                        rect.x2(view).map_err(access)?, rect.y2(view).map_err(access)?) {
                        (Some(x), Some(y), Some(x2), Some(y2))
                            if [x, y, x2, y2].iter().all(|value| value.is_finite()) && x2 > x && y2 > y =>
                                Some((x, y, x2, y2)),
                        _ => None,
                    }
                } else {
                    None
                };
                let nonspace = |part: &&InputTextPart| matches!(part,
                    InputTextPart::Character { text, .. } if text.chars().any(|ch| !python_whitespace(ch)));
                let first = characters.iter().find(nonspace).and_then(boundary_character_coords).or(formula_box);
                let last = characters.iter().rev().find(nonspace).and_then(boundary_character_coords).or(formula_box);
                parts.push(InputTextPart::BoundaryFormula {
                    token: placeholder.token.clone(),
                    source: placeholder.source_chars.clone(),
                    first,
                    last,
                });
            } else {
                parts.push(InputTextPart::Text(placeholder.token.clone()));
            }
            placeholders.push(stage::InputPlaceholder::Formula(placeholder));
        } else if let Some(character) = composition.pdf_character(view).map_err(access)? {
            parts.extend(input_character_fragments(std::iter::once(Ok(character)), view, boundary)?);
        } else if let Some(group) = composition
            .pdf_same_style_characters(view)
            .map_err(access)?
        {
            let group_characters =
                collect_children(group.pdf_character_iter(view).map_err(access)?)?;
            let source_text = raw_character_text(group_characters.clone(), view)?;
            let composition_style = group.pdf_style(view).map_err(access)?;
            let paragraph_style = paragraph.pdf_style(view).map_err(access)?;
            if allow_rich {
                for style in [composition_style.as_ref(), paragraph_style.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    let font_id = style.font_id(view).map_err(access)?;
                    if let Err(error) = require_font_style(font_styles, font_id.clone()) {
                        *boundary_failure = Some(format!("missing source font style: {}", error.key_error_message()));
                        deferred_missing_font_style.get_or_insert(error);
                        return Ok(None);
                    }
                }
            }
            if boundary && allow_rich && composition_style.is_none() != paragraph_style.is_none() {
                *boundary_failure = Some("source and paragraph style presence differs".into());
                return Ok(None);
            }
            let equivalent = style_matches(
                composition_style.clone(),
                paragraph_style.clone(),
                view,
                false,
                false,
                mapper,
                font_styles,
            )? || style_matches(
                composition_style.clone(),
                paragraph_style.clone(),
                view,
                true,
                false,
                mapper,
                font_styles,
            )? || (style_matches(
                composition_style,
                paragraph_style,
                view,
                false,
                true,
                mapper,
                font_styles,
            )?);
            if allow_rich && !equivalent {
                let (left, left_pattern) = placeholder_result(
                    service,
                    "get_rich_text_left_placeholder",
                    &next_id.to_string(),
                )?;
                let (right, right_pattern) = placeholder_result(
                    service,
                    "get_rich_text_right_placeholder",
                    &next_id.to_string(),
                )?;
                let placeholder = stage::rich_placeholder(
                    regex,
                    &unicode,
                    next_id,
                    index,
                    |id| {
                        let (left, left_pattern) = placeholder_result(
                            service,
                            "get_rich_text_left_placeholder",
                            &id.to_string(),
                        )?;
                        let (right, right_pattern) = placeholder_result(
                            service,
                            "get_rich_text_right_placeholder",
                            &id.to_string(),
                        )?;
                        Ok((left, right, left_pattern, right_pattern))
                    },
                    source_text,
                )
                .map_err(access)?;
                next_id = placeholder.id + 2;
                parts.push(InputTextPart::BoundaryOpen(placeholder.left.clone()));
                parts.extend(input_character_fragments(
                    group_characters.into_iter().map(Ok),
                    view,
                    boundary,
                )?);
                parts.push(InputTextPart::BoundaryClose(placeholder.right.clone()));
                placeholders.push(stage::InputPlaceholder::Rich(placeholder));
                let _ = (left, left_pattern, right, right_pattern);
            } else {
                parts.extend(input_character_fragments(
                    group_characters.into_iter().map(Ok),
                    view,
                    boundary,
                )?);
            }
        } else if boundary {
            if let Some(group) = composition.pdf_same_style_unicode_characters(view).map_err(access)? {
                if allow_rich {
                    let group_style = group.pdf_style(view).map_err(access)?;
                    let paragraph_style = paragraph.pdf_style(view).map_err(access)?;
                    for style in [group_style.as_ref(), paragraph_style.as_ref()].into_iter().flatten() {
                        if let Err(error) = require_font_style(font_styles, style.font_id(view).map_err(access)?.clone()) {
                            *boundary_failure = Some(format!("missing source font style: {}", error.key_error_message()));
                            deferred_missing_font_style.get_or_insert(error);
                            return Ok(None);
                        }
                    }
                    let equivalent = style_matches(group_style.clone(), paragraph_style.clone(), view, false, false, mapper, font_styles)?
                        || style_matches(group_style.clone(), paragraph_style.clone(), view, true, false, mapper, font_styles)?
                        || style_matches(group_style, paragraph_style, view, false, true, mapper, font_styles)?;
                    if !equivalent {
                        *boundary_failure = Some("unicode-only source has a different style".into());
                        return Ok(None);
                    }
                }
                let Some(text) = group.unicode(view).map_err(access)?.clone() else {
                    *boundary_failure = Some("unicode-only source has no text".into());
                    return Ok(None)
                };
                parts.push(InputTextPart::Text(text));
            } else {
                *boundary_failure = Some("unsupported or empty source composition".into());
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
        if !boundary && placeholders.len() > 40 && allow_rich {
            return build_input(
                paragraph,
                view,
                service,
                regex,
                patterns,
                false,
                mapper,
                font_styles,
                unicode_override,
                deferred_missing_font_style,
            );
        }
    }
    let text = if let Some(boundary_patterns) = boundary_patterns {
        let raw = boundary_raw_text(&parts);
        if !boundary_source_unambiguous(&raw, &unicode, boundary_patterns, regex)? {
            *boundary_failure = Some("source contains a literal placeholder or reserved marker".into());
            return Ok(None);
        }
        match compose_boundary_input_text(&parts, &unicode) {
            Ok(text) => text,
            Err(reason) => { *boundary_failure = Some(reason.into()); return Ok(None); }
        }
    } else {
        compose_input_text(&parts)
    };
    let translated = stage::prepare_translated_text(&[text], placeholders, originals);
    Ok(Some(translated))
}

const CONTENT_FILTER_HINT: &str =
    "翻译服务检测到内容可能包含不安全或敏感内容，请您避免翻译敏感内容，感谢您的配合。";

fn is_cid_placeholder(value: &str) -> bool {
    let Some(digits) = value
        .strip_prefix("(cid:")
        .and_then(|s| s.strip_suffix(')'))
    else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|ch| ch.to_digit(10).is_some())
}

fn python_string_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::with_capacity(value.len() + 2);
    result.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch == quote => {
                result.push('\\');
                result.push(ch);
            }
            ch if ch.is_control() || ('\u{e000}'..='\u{f8ff}').contains(&ch) => {
                result.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => result.push(ch),
        }
    }
    result.push(quote);
    result
}

pub(crate) fn formula_hint(input: &stage::TranslateInput) -> Option<String> {
    let entries = input
        .placeholders
        .iter()
        .filter_map(|placeholder| match placeholder {
            stage::InputPlaceholder::Formula(formula) if formula.hint_source => Some(format!(
                "{}: {}",
                python_string_repr(&formula.token),
                python_string_repr(&formula.source_chars)
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!entries.is_empty()).then(|| format!("{{{}}}", entries.join(", ")))
}

pub(crate) fn tracking_placeholders(
    input: &stage::TranslateInput,
) -> Vec<stage::PlaceholderRecord> {
    input
        .placeholders
        .iter()
        .map(|placeholder| match placeholder {
            stage::InputPlaceholder::Formula(formula) => stage::PlaceholderRecord::Formula {
                id: formula.id,
                text: formula.token.clone(),
                pattern: formula.pattern.clone(),
                chars: Some(formula.source_chars.clone()),
            },
            stage::InputPlaceholder::Rich(rich) => stage::PlaceholderRecord::RichText {
                id: rich.id,
                left: rich.left.clone(),
                right: rich.right.clone(),
                left_pattern: rich.left_pattern.clone(),
                right_pattern: rich.right_pattern.clone(),
                chars: Some(rich.source_text.clone()),
            },
        })
        .collect()
}

pub(crate) fn access(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

pub(crate) fn apply_llm_output(
    document: &mut Document,
    page_index: usize,
    paragraph_index: usize,
    input: &stage::TranslateInput,
    output: &str,
    regex_service: &Bound<'_, PyAny>,
    hallucination_ids: &[u64],
) -> PyResult<(String, bool, Vec<(String, usize)>)> {
    let text = stage::normalize_response_punctuation(output);
    let (parsed, full_match, removed) = Python::attach(|_py| -> PyResult<_> {
        let mut regex = RegexService {
            service: regex_service.clone(),
        };
        let mut removed = Vec::new();
        let (parsed, full) =
            stage::parse_output(&mut regex, input, &text, hallucination_ids, &mut removed)?;
        Ok((parsed, full, removed))
    })?;
    let mut view = document.view::<TranslationView>();
    let paragraph = view
        .document()
        .page_at(&view, page_index)
        .map_err(access)?
        .pdf_paragraph_at(&view, paragraph_index)
        .map_err(access)?;
    // Python 0.6.4 compares the output string with the TranslateInput object;
    // that branch is unreachable and the paragraph is always rebuilt.
    paragraph
        .set_unicode(&mut view, Some(text.clone()))
        .map_err(access)?;
    let mut compositions = Vec::new();
    for part in parsed {
        let composition = match part {
            stage::ParsedPart::Text(value) => {
                let style = paragraph
                    .pdf_style(&view)
                    .map_err(access)?
                    .map(|value| value.share(&mut view))
                    .transpose()
                    .map_err(access)?;
                let same = ir::PdfSameStyleUnicodeCharactersInit {
                    pdf_style: style,
                    unicode: Some(value),
                    debug_info: None,
                }
                .into_value();
                ir::PdfParagraphCompositionInit {
                    pdf_same_style_unicode_characters: Some(same),
                    ..Default::default()
                }
                .into_value()
            }
            stage::ParsedPart::Formula { composition_index } => {
                let old = paragraph
                    .pdf_paragraph_composition_at(&view, composition_index)
                    .map_err(access)?;
                let formula = old.pdf_formula(&view).map_err(access)?.ok_or_else(|| {
                    PyRuntimeError::new_err("formula placeholder source disappeared")
                })?;
                ir::PdfParagraphCompositionInit {
                    pdf_formula: Some(formula.share(&mut view).map_err(access)?),
                    ..Default::default()
                }
                .into_value()
            }
            stage::ParsedPart::Rich {
                composition_index,
                text: value,
                matches_source,
            } => {
                let old = paragraph
                    .pdf_paragraph_composition_at(&view, composition_index)
                    .map_err(access)?;
                let rich = old
                    .pdf_same_style_characters(&view)
                    .map_err(access)?
                    .ok_or_else(|| {
                        PyRuntimeError::new_err("rich placeholder source disappeared")
                    })?;
                if !matches_source {
                    let style = rich
                        .pdf_style(&view)
                        .map_err(access)?
                        .map(|value| value.share(&mut view))
                        .transpose()
                        .map_err(access)?;
                    let same = ir::PdfSameStyleUnicodeCharactersInit {
                        pdf_style: style,
                        unicode: Some(value),
                        debug_info: None,
                    }
                    .into_value();
                    ir::PdfParagraphCompositionInit {
                        pdf_same_style_unicode_characters: Some(same),
                        ..Default::default()
                    }
                    .into_value()
                } else {
                    ir::PdfParagraphCompositionInit {
                        pdf_same_style_characters: Some(rich.share(&mut view).map_err(access)?),
                        ..Default::default()
                    }
                    .into_value()
                }
            }
        };
        compositions.push(composition);
    }
    paragraph
        .set_pdf_paragraph_composition(&mut view, compositions)
        .map_err(access)?;
    Ok((text, full_match, removed))
}

fn add_page_font_styles(
    styles: &mut HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>,
    fonts: impl Iterator<
        Item = Result<ir::Cursor<babeldoc_ir_core::types::PdfFont>, babeldoc_ir_core::AccessError>,
    >,
    view: &TreeView<'_, impl Permission>,
) -> PyResult<()> {
    for font in fonts {
        let font = font.map_err(access)?;
        let Some(font_id) = font.font_id(view).map_err(access)? else {
            continue;
        };
        styles.insert(
            font_id.clone(),
            babeldoc_p2_stages::font_mapper::SourceFontStyle {
                bold: font.bold(view).map_err(access)?.as_ref().is_some_and(|v| {
                    matches!(
                        v,
                        babeldoc_il_types::RuntimeBool::Bool(true)
                            | babeldoc_il_types::RuntimeBool::Int(1)
                    )
                }),
                italic: font
                    .italic(view)
                    .map_err(access)?
                    .as_ref()
                    .is_some_and(|v| {
                        matches!(
                            v,
                            babeldoc_il_types::RuntimeBool::Bool(true)
                                | babeldoc_il_types::RuntimeBool::Int(1)
                        )
                    }),
                monospaced: font
                    .monospace(view)
                    .map_err(access)?
                    .as_ref()
                    .is_some_and(|v| {
                        matches!(
                            v,
                            babeldoc_il_types::RuntimeBool::Bool(true)
                                | babeldoc_il_types::RuntimeBool::Int(1)
                        )
                    }),
                serif: font.serif(view).map_err(access)?.as_ref().is_some_and(|v| {
                    matches!(
                        v,
                        babeldoc_il_types::RuntimeBool::Bool(true)
                            | babeldoc_il_types::RuntimeBool::Int(1)
                    )
                }),
            },
        );
    }
    Ok(())
}

type FontStyles = HashMap<String, babeldoc_p2_stages::font_mapper::SourceFontStyle>;

#[derive(Clone, Default)]
struct PageFontMaps {
    fonts: FontStyles,
    xobjects: HashMap<Option<i64>, FontStyles>,
}

impl PageFontMaps {
    fn for_xobject(&self, xobj_id: Option<i64>) -> &FontStyles {
        self.xobjects.get(&xobj_id).unwrap_or(&self.fonts)
    }
}

#[cfg(test)]
mod page_local_font_tests {
    use super::*;

    struct LiteralRegex(Vec<String>);
    impl stage::PlaceholderRegex for LiteralRegex {
        type Error = std::convert::Infallible;
        fn compile(&mut self, pattern: &str, _: bool) -> Result<u64, Self::Error> {
            self.0.push(pattern.into());
            Ok((self.0.len() - 1) as u64)
        }
        fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> {
            Ok(!self.find_all(pattern, text)?.is_empty())
        }
        fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<stage::RegexMatch>, Self::Error> {
            Ok(text.match_indices(&self.0[pattern as usize]).map(|(start, value)| stage::RegexMatch {
                full: value.into(), start: text[..start].chars().count(),
                end: text[..start + value.len()].chars().count(), groups: vec![],
            }).collect())
        }
    }

    #[test]
    fn literal_tokens_are_rejected_and_clean_input_is_validated() {
        let mut regex = LiteralRegex(vec!["{v1}".into(), "{v1}".into(), "{v2}".into()]);
        let mut input = stage::TranslateInput { text: "字{v1} {v1}".into(), placeholders: vec![], original_tokens: vec![] };
        assert!(!validate_local_input(&mut input, "字{v1}", &[0, 1, 2], &mut regex).unwrap());
        assert!(!validate_local_input(&mut input, "字{v1} {v1}", &[0, 1, 2], &mut regex).unwrap());
        input.text = "普通文字".into();
        input.original_tokens = vec![("stale".into(), 1)];
        assert!(validate_local_input(&mut input, "普通文字", &[0, 1, 2], &mut regex).unwrap());
        assert!(input.original_tokens.is_empty());
        let mut overlap = LiteralRegex(vec!["{v1}".into(), "v1".into(), "{v2}".into()]);
        assert!(!validate_local_input(&mut input, "字{v1}", &[0, 1, 2], &mut overlap).unwrap());
        assert!(!validate_local_input(&mut input, "字{v1}", &[0, 1], &mut overlap).unwrap());
    }

    fn character(text: &str, x: f64, y: f64, width: f64, height: f64) -> InputTextPart {
        InputTextPart::Character {
            text: text.into(),
            coords: Some((x, y, x + width, y + height)),
            font_id: None,
            font_size: None,
        }
    }

    fn unpositioned(text: &str) -> InputTextPart {
        InputTextPart::Character { text: text.into(), coords: None, font_id: None, font_size: None }
    }

    #[test]
    fn boundary_geometry_passes_style_and_formula_markers() {
        let styled = vec![
            character("T", 0.0, 0.0, 1.0, 10.0),
            character("h", 1.0, 0.0, 1.0, 10.0),
            character("e", 2.0, 0.0, 1.0, 10.0),
            InputTextPart::BoundaryOpen("<b1>".into()),
            character("m", 5.0, 0.0, 1.0, 10.0),
            character("o", 6.0, 0.0, 1.0, 10.0),
            InputTextPart::BoundaryClose("</b1>".into()),
            character("x", 9.0, 0.0, 1.0, 10.0),
        ];
        assert_eq!(compose_boundary_input_text(&styled, "Themo x"), Ok("The <b1>mo</b1> x".into()));
        let formula = vec![
            character("A", 0.0, 0.0, 1.0, 10.0),
            InputTextPart::BoundaryFormula {
                token: "{v1}".into(), source: "x".into(),
                first: Some((3.0, 0.0, 4.0, 10.0)), last: Some((3.0, 0.0, 4.0, 10.0)),
            },
            character("B", 6.0, 0.0, 1.0, 10.0),
        ];
        assert_eq!(compose_boundary_input_text(&formula, "AxB"), Ok("A {v1} B".into()));
    }

    #[test]
    fn boundary_preserves_unicode_whitespace_and_rejects_mismatch() {
        let one = vec![unpositioned("We\u{a0}find")];
        let many = vec![unpositioned("We"), InputTextPart::BoundaryOpen("<b1>".into()),
            unpositioned("find"), InputTextPart::BoundaryClose("</b1>".into())];
        assert_eq!(compose_boundary_input_text(&one, "We\u{a0}find"), Ok("We\u{a0}find".into()));
        assert_eq!(compose_boundary_input_text(&many, "We\u{a0}find"), Ok("We\u{a0}<b1>find</b1>".into()));
        assert_eq!(compose_boundary_input_text(&one, "We find"), Ok("We find".into()));
        assert!(compose_boundary_input_text(&many, "We found").is_err());
        assert_eq!(compose_boundary_input_text(&[unpositioned("10⁻³")], "10⁻³"), Ok("10⁻³".into()));
    }

    #[test]
    fn boundary_maps_ligatures_and_legacy_scripts_to_their_sources() {
        let parts = vec![unpositioned("A  e"), InputTextPart::BoundaryOpen("<b1>".into()),
            unpositioned("ﬀ"), InputTextPart::BoundaryClose("</b1>".into()), unpositioned("ect 10⁻³")];
        assert_eq!(compose_boundary_input_text(&parts, "A effect 10−3"), Ok("A e<b1>ff</b1>ect 10⁻³".into()));
        assert!(compose_boundary_input_text(&[unpositioned("ﬁ")], "f i").is_err());
        assert!(compose_boundary_input_text(&[unpositioned("x²")], "x3").is_err());
        assert!(compose_boundary_input_text(&[unpositioned("x2")], "x²").is_err());
        let formula = InputTextPart::BoundaryFormula { token: "{v1}".into(), source: "10⁻³".into(), first: None, last: None };
        assert_eq!(compose_boundary_input_text(&[formula], "10−3"), Ok("{v1}".into()));
    }

    #[test]
    fn boundary_rebuilds_six_stored_pre_ir_sources() {
        let root = std::env::var_os("BABELDOC_SOURCE_FIXTURES").map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.crossboundary-rootcause-20261112/selection"));
        let samples: &[(&str, &[(usize, usize)])] = &[
            ("06-Asp-pre", &[(0, 6), (0, 7)]),
            ("07-science-first-pre", &[(0, 8), (1, 2), (1, 8), (1, 9)]),
        ];
        if !root.join(samples[0].0).exists() { return; }
        for (sample, locations) in samples {
            let path = root.join(sample).join("pretranslation-artifact.json");
            let document: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            for &(page, paragraph) in *locations {
                let paragraph = &document["document"]["page"][page]["pdf_paragraph"][paragraph];
                let mut parts = Vec::new();
                for (index, composition) in paragraph["pdf_paragraph_composition"].as_array().unwrap().iter().enumerate() {
                    let source_character = |value: &serde_json::Value| {
                        let bounds = &value["box"];
                        let coords = bounds["x"].as_f64().zip(bounds["y"].as_f64())
                            .zip(bounds["x2"].as_f64().zip(bounds["y2"].as_f64()))
                            .map(|((x, y), (x2, y2))| (x, y, x2, y2));
                        InputTextPart::Character {
                            text: value["char_unicode"].as_str().unwrap_or_default().into(), coords,
                            font_id: value["pdf_style"]["font_id"].as_str().map(str::to_owned),
                            font_size: value["pdf_style"]["font_size"].as_f64(),
                        }
                    };
                    if let Some(character) = composition.get("pdf_character").filter(|value| !value.is_null()) {
                        parts.push(source_character(character));
                    } else if let Some(formula) = composition.get("pdf_formula").filter(|value| !value.is_null()) {
                        let characters: Vec<_> = formula["pdf_character"].as_array().unwrap().iter().map(source_character).collect();
                        let source = boundary_raw_text(&characters);
                        let first = characters.iter().find_map(boundary_character_coords);
                        let last = characters.iter().rev().find_map(boundary_character_coords);
                        parts.push(InputTextPart::BoundaryFormula { token: format!("{{v{index}}}"), source, first, last });
                    } else if let Some(group) = composition.get("pdf_same_style_characters").filter(|value| !value.is_null()) {
                        parts.push(InputTextPart::BoundaryOpen(format!("<b{index}>")));
                        parts.extend(group["pdf_character"].as_array().unwrap().iter().map(source_character));
                        parts.push(InputTextPart::BoundaryClose(format!("</b{index}>")));
                    } else if let Some(line) = composition.get("pdf_line").filter(|value| !value.is_null()) {
                        parts.extend(line["pdf_character"].as_array().unwrap().iter().map(source_character));
                    } else if let Some(group) = composition.get("pdf_same_style_unicode_characters").filter(|value| !value.is_null()) {
                        parts.push(InputTextPart::Text(group["unicode"].as_str().unwrap().into()));
                    } else {
                        panic!("unsupported source composition in {sample}");
                    }
                }
                let text = compose_boundary_input_text(&parts, paragraph["unicode"].as_str().unwrap());
                assert!(text.is_ok(), "{sample} {page}: {text:?}");
                if *sample == "06-Asp-pre" && paragraph["unicode"].as_str().unwrap().contains("toolsand") {
                    assert!(text.unwrap().contains("tools and"));
                }
            }
        }
    }

    #[test]
    fn boundary_rejects_unrepresented_geometric_script() {
        let shifted = vec![character("x", 0.0, 0.0, 5.0, 10.0), character("2", 5.0, 6.0, 3.0, 6.0)];
        assert!(compose_boundary_input_text(&shifted, "x2").is_err());
        assert_eq!(compose_boundary_input_text(&[unpositioned("x2")], "x2"), Ok("x2".into()));
        let encoded = vec![character("x", 0.0, 0.0, 5.0, 10.0), character("²", 5.0, 6.0, 3.0, 6.0)];
        assert_eq!(compose_boundary_input_text(&encoded, "x²"), Ok("x²".into()));
    }

    #[test]
    fn boundary_allows_numeric_and_formula_but_rejects_literal_tokens() {
        assert_eq!(compose_boundary_input_text(&[unpositioned("42")], "42"), Ok("42".into()));
        let formula = InputTextPart::BoundaryFormula {
            token: "{v1}".into(), source: "10⁻³".into(), first: None, last: None,
        };
        assert_eq!(compose_boundary_input_text(&[formula], "10⁻³"), Ok("{v1}".into()));
        let spaced_formula = InputTextPart::BoundaryFormula {
            token: "{v1}".into(), source: "x + y".into(), first: None, last: None,
        };
        assert_eq!(compose_boundary_input_text(&[spaced_formula], "x + y"), Ok("{v1}".into()));
        let mut regex = LiteralRegex(vec!["{v1}".into(), "<b1>".into(), "</b1>".into()]);
        assert!(!boundary_source_unambiguous("prefix {v1}", "prefix {v1}", &[0, 1, 2], &mut regex).unwrap());
        assert!(!boundary_source_unambiguous("prefix <b1>", "prefix <b1>", &[0, 1, 2], &mut regex).unwrap());
        assert!(!boundary_source_unambiguous("[[BABEL_F_0_0_0]]", "", &[0, 1, 2], &mut regex).unwrap());
        assert!(boundary_source_unambiguous("42", "42", &[0, 1, 2], &mut regex).unwrap());
    }

    #[test]
    fn boundary_rebuilds_ir_inputs_and_preserves_atomic_failure() {
        Python::initialize();
        Python::attach(|py| {
            let module = pyo3::types::PyModule::from_code(
                py,
                pyo3::ffi::c_str!(r#"
import re
class Service:
    def __init__(self):
        self.patterns = []
    def regex_compile(self, pattern, flags):
        self.patterns.append(re.compile(pattern, flags))
        return len(self.patterns) - 1
    def regex_match(self, index, text):
        return self.patterns[index].match(text)
    def regex_finditer(self, index, text):
        return [[match.span(index) for index in range(self.patterns[index].groups + 1)]
                for match in self.patterns[index].finditer(text)]
    def get_formular_placeholder(self, identifier):
        return '{v' + str(identifier) + '}'
    def get_rich_text_left_placeholder(self, identifier):
        return '<b' + str(identifier) + '>'
    def get_rich_text_right_placeholder(self, identifier):
        return '</b' + str(identifier) + '>'
service = Service()
"#),
                pyo3::ffi::c_str!("boundary_fixture.py"),
                pyo3::ffi::c_str!("boundary_fixture"),
            ).unwrap();
            let service = module.getattr("service").unwrap();
            let mut regex = RegexService { service: service.clone() };
            let patterns = stage::PlaceholderPatterns {
                formula: r"\{v\d+\}".into(), rich_left: r"<b\d+>".into(), rich_right: r"</b\d+>".into(),
            };
            let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
            let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
            let mapper = FontMapperHandle::from_shared(
                babeldoc_p2_stages::font_mapper::font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap(),
            );
            let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
                page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
                ..Default::default()
            }));
            let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
            let make_line = |text: &str| ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit {
                    pdf_character: text.chars().map(|ch| ir::PdfCharacterInit {
                        char_unicode: Some(ch.to_string()), ..Default::default()
                    }.into_value()).collect(), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value();
            let make_formula = || ir::PdfParagraphCompositionInit {
                pdf_formula: Some(ir::PdfFormulaInit {
                    pdf_character: vec![ir::PdfCharacterInit { char_unicode: Some("x".into()), ..Default::default() }.into_value()],
                    ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value();
            let make_rich = || ir::PdfParagraphCompositionInit {
                pdf_same_style_characters: Some(ir::PdfSameStyleCharactersInit {
                    pdf_style: Some(ir::PdfStyleInit {
                        font_id: Some("test".into()), font_size: Some(10.0), ..Default::default()
                    }.into_value()),
                    pdf_character: vec![ir::PdfCharacterInit { char_unicode: Some("x".into()), ..Default::default() }.into_value()],
                    ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value();
            let mut rich_paragraph = ir::PdfParagraphInit {
                unicode: Some("x".repeat(41)),
                pdf_style: Some(ir::PdfStyleInit {
                    font_id: Some("test".into()), font_size: Some(10.0),
                    graphic_state: Some(ir::GraphicStateInit {
                        passthrough_per_char_instruction: Some(ir::Instructions::Materialized("different".into())),
                    }.into_value()),
                }.into_value()), ..Default::default()
            };
            rich_paragraph.pdf_paragraph_composition = (0..41).map(|_| make_rich()).collect();
            {
                let mut view = document.view::<ir::ParagraphFinderView>();
                let page = view.document().page_at(&view, 0).unwrap();
                page.set_pdf_paragraph(&mut view, vec![
                    ir::PdfParagraphInit { unicode: Some("42".into()), pdf_paragraph_composition: vec![make_line("42")], ..Default::default() }.into_value(),
                    ir::PdfParagraphInit { unicode: Some("x".into()), pdf_paragraph_composition: vec![make_formula()], ..Default::default() }.into_value(),
                    ir::PdfParagraphInit { unicode: Some("We find".into()), pdf_paragraph_composition: vec![make_line("We"), make_line("find")], ..Default::default() }.into_value(),
                    ir::PdfParagraphInit { unicode: Some("a{v1}".into()), pdf_paragraph_composition: vec![make_line("a{v1}")], ..Default::default() }.into_value(),
                    rich_paragraph.into_value(),
                ]).unwrap();
            }
            let make_job = |paragraph: usize, text: &str| Job {
                page: 0, paragraph, debug_id: Some(format!("p{paragraph}")), text: text.into(), tracker_unicode: text.into(),
                vertical: false, unicode_present: true, layout_label: None, cid: false,
                pure_numeric: false, placeholder_only: false, box_y2: None, title_paragraph: false, token_count: 0,
                title: None, local_title: None, eligible: false, input: None, fallback_input: None,
                missing_font_style: None, fallback_missing_font_style: None,
            };
            let mut jobs = vec![make_job(0, "42"), make_job(1, "x"), make_job(2, "We find"), make_job(3, "a{v1}"), make_job(4, &"x".repeat(41))];
            let font_styles = HashMap::from([("test".into(), babeldoc_p2_stages::font_mapper::SourceFontStyle::default())]);
            {
                let view = document.view::<ReadOnlyView>();
                let page = view.document().page_at(&view, 0).unwrap();
                let paragraph = page.pdf_paragraph_at(&view, 4).unwrap();
                let ids = cross_page_patterns(&service, &mut regex).unwrap();
                let mut missing = None;
                let input = build_input_impl(&paragraph, &view, &service, &mut regex, &patterns, true, &mapper, &font_styles, None, &mut missing, Some(&ids), &mut None).unwrap().unwrap();
                assert_eq!(input.placeholders.len(), 41);
                assert!(input.placeholders.iter().all(|placeholder| matches!(placeholder, stage::InputPlaceholder::Rich(_))));
                for index in 0..2 {
                    let paragraph = page.pdf_paragraph_at(&view, index).unwrap();
                    assert!(build_input(&paragraph, &view, &service, &mut regex, &patterns, true, &mapper, &font_styles, None, &mut None).unwrap().is_none());
                }
            }
            for index in 0..3 {
                assert!(rebuild_boundary_inputs(&mut document, &mut jobs, &[(0, index)], &service, &mut regex, &patterns, true, &mapper).unwrap());
            }
            assert_eq!(jobs[0].input.as_ref().unwrap().text, "42");
            let formula = jobs[1].input.as_ref().unwrap();
            assert_eq!(formula.text, "{v1}");
            assert!(matches!(&formula.placeholders[..], [stage::InputPlaceholder::Formula(value)] if value.composition_index == 0 && value.source_chars == "x"));
            let ids = cross_page_patterns(&service, &mut regex).unwrap();
            let source = babeldoc_p2_stages::translation::cross_boundary::Source::new(
                babeldoc_p2_stages::translation::cross_boundary::Location { page: 0, paragraph: 1 },
                formula, &ids, &mut regex,
            ).unwrap().unwrap();
            assert_eq!(source.formulas()[0].1, "x");
            assert_eq!(jobs[2].input.as_ref().unwrap().text, "We find");
            let before = jobs[0].input.clone();
            assert!(!rebuild_boundary_inputs(&mut document, &mut jobs, &[(0, 0), (0, 3)], &service, &mut regex, &patterns, true, &mapper).unwrap());
            let failure = rebuild_boundary_inputs_diagnostic(&mut document, &mut jobs, &[(0, 0), (0, 3)], &service, &mut regex, &patterns, true, &mapper).unwrap().unwrap_err();
            assert_eq!((failure.page, failure.paragraph), (0, 3));
            assert!(failure.reason.contains("literal placeholder"));
            assert_eq!(jobs[0].input, before);
            assert!(jobs[3].input.is_none());
        });
    }

    #[test]
    fn page_and_xobject_font_resources_remain_separate() {
        let normal = babeldoc_p2_stages::font_mapper::SourceFontStyle::default();
        let mut bold = normal;
        bold.bold = true;
        let mut first = PageFontMaps::default();
        first.fonts.insert("F1".into(), normal);
        first.xobjects.insert(Some(7), HashMap::from([("F1".into(), bold)]));
        let mut second = PageFontMaps::default();
        second.fonts.insert("F1".into(), bold);
        second.xobjects.insert(Some(7), HashMap::from([("F1".into(), normal)]));
        assert!(!first.for_xobject(None)["F1"].bold);
        assert!(first.for_xobject(Some(7))["F1"].bold);
        assert!(second.for_xobject(None)["F1"].bold);
        assert!(!second.for_xobject(Some(7))["F1"].bold);
    }
}

fn merge_cross_page_font_maps(curr: &PageFontMaps, next: &PageFontMaps) -> PageFontMaps {
    let mut merged = curr.clone();
    merged.fonts.extend(next.fonts.clone());
    merged.xobjects.extend(next.xobjects.clone());
    merged
}

fn page_font_maps_for(
    view: &babeldoc_ir_core::TreeView<'_, ReadOnlyView>,
    page_index: usize,
    needed_xobjects: &HashSet<Option<i64>>,
) -> PyResult<PageFontMaps> {
    let page = view.document().page_at(view, page_index).map_err(access)?;
    let mut fonts = HashMap::new();
    add_page_font_styles(&mut fonts, page.pdf_font_iter(view).map_err(access)?, view)?;
    let mut xobjects = HashMap::new();
    for xobject in page.pdf_xobject_iter(view).map_err(access)? {
        let xobject = xobject.map_err(access)?;
        let id = xobject.xobj_id(view).map_err(access)?;
        if !needed_xobjects.contains(&id) {
            continue;
        }
        let mut xobject_fonts = fonts.clone();
        add_page_font_styles(
            &mut xobject_fonts,
            xobject.pdf_font_iter(view).map_err(access)?,
            view,
        )?;
        xobjects.insert(id, xobject_fonts);
    }
    Ok(PageFontMaps { fonts, xobjects })
}

fn validate_local_input<R: stage::PlaceholderRegex>(
    input: &mut stage::TranslateInput,
    source: &str,
    patterns: &[u64],
    regex: &mut R,
) -> Result<bool, R::Error> {
    if patterns.len() != 3 || input.text.contains("[[BABEL_") { return Ok(false); }
    for &pattern in patterns {
        if !regex.find_all(pattern, source)?.is_empty() { return Ok(false); }
    }
    input.original_tokens.clear();
    babeldoc_p2_stages::translation::cross_page_protocol::validate_response(regex, input, &input.text, patterns)
}

pub(crate) fn rebuild_page_local_inputs(
    document: &mut Document,
    jobs: &mut [Job],
    pair: [(usize, usize); 2],
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &FontMapperHandle,
) -> PyResult<bool> {
    rebuild_boundary_inputs(document, jobs, &pair, service, regex, patterns, allow_rich, mapper)
}

pub(crate) fn rebuild_boundary_inputs(
    document: &mut Document,
    jobs: &mut [Job],
    keys: &[(usize, usize)],
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &FontMapperHandle,
) -> PyResult<bool> {
    Ok(rebuild_boundary_inputs_diagnostic(document, jobs, keys, service, regex, patterns, allow_rich, mapper)?.is_ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundaryInputFailure {
    pub(crate) page: usize,
    pub(crate) paragraph: usize,
    pub(crate) reason: String,
}

pub(crate) fn rebuild_boundary_inputs_diagnostic(
    document: &mut Document,
    jobs: &mut [Job],
    keys: &[(usize, usize)],
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &FontMapperHandle,
) -> PyResult<Result<(), BoundaryInputFailure>> {
    if keys.iter().copied().collect::<HashSet<_>>().len() != keys.len() {
        return Err(PyRuntimeError::new_err("duplicate paragraph in boundary selection"));
    }
    let mut indices = Vec::with_capacity(keys.len());
    for &key in keys {
        let mut found = jobs.iter().enumerate().filter(|(_, job)| (job.page, job.paragraph) == key);
        let Some((index, _)) = found.next() else {
            return Err(PyRuntimeError::new_err("cross-page paragraph job missing"));
        };
        if found.next().is_some() {
            return Err(PyRuntimeError::new_err("duplicate cross-page paragraph job"));
        }
        indices.push(index);
    }
    let generic_patterns = cross_page_patterns(service, regex)?;
    let view = document.view::<ReadOnlyView>();
    let mut rebuilt: Vec<Job> = indices.iter().map(|&index| jobs[index].clone()).collect();
    for (&(page_index, paragraph_index), job) in keys.iter().zip(&mut rebuilt) {
        let failure = |reason: String| BoundaryInputFailure { page: page_index, paragraph: paragraph_index, reason };
        let page = view.document().page_at(&view, page_index).map_err(access)?;
        let paragraph = page.pdf_paragraph_at(&view, paragraph_index).map_err(access)?;
        let vertical = paragraph.vertical(&view).map_err(access)?.unwrap_or(false);
        let xobj_id = paragraph.xobj_id(&view).map_err(access)?;
        let maps = page_font_maps_for(&view, page_index, &HashSet::from([xobj_id]))?;
        let font_styles = maps.for_xobject(xobj_id);
        let unicode = paragraph.unicode(&view).map_err(access)?.clone();
        let mut build_failure = None;
        rebuild_job_inputs(job, vertical, || {
            let mut missing = None;
            let input = build_input_impl(&paragraph, &view, service, regex, patterns, allow_rich, mapper, font_styles, unicode.as_deref(), &mut missing, Some(&generic_patterns), &mut build_failure)?;
            Ok((input.clone(), input, missing.clone(), missing))
        })?;
        let Some(input) = job.input.as_mut() else {
            let reason = if vertical || job.vertical { "vertical source paragraph".into() }
                else { build_failure.unwrap_or_else(|| "source input could not be built".into()) };
            return Ok(Err(failure(reason)));
        };
        if !validate_local_input(input, &job.text, &generic_patterns, regex)? {
            return Ok(Err(failure("source placeholder validation failed".into())));
        }
        let Some(fallback) = job.fallback_input.as_mut() else { return Ok(Err(failure("fallback source input is missing".into()))) };
        if !validate_local_input(fallback, &job.text, &generic_patterns, regex)? {
            return Ok(Err(failure("fallback source placeholder validation failed".into())));
        }
    }
    for (index, job) in indices.into_iter().zip(rebuilt) {
        jobs[index] = job;
    }
    Ok(Ok(()))
}

/// Rebuild only inputs selected for cross-page LLM batches with the same
/// current-then-next page font maps that Python 0.6.4 passes to the batch.
pub(crate) fn rebuild_cross_page_inputs(
    document: &mut Document,
    jobs: &mut [Job],
    pairs: &[[(usize, usize); 2]],
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    mapper: &FontMapperHandle,
) -> PyResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }

    let view = document.view::<ReadOnlyView>();
    let job_indices = jobs
        .iter()
        .enumerate()
        .map(|(index, job)| ((job.page, job.paragraph), index))
        .collect::<HashMap<_, _>>();

    for pair in pairs {
        let mut needed_xobjects = HashSet::new();
        for (page_index, paragraph_index) in pair {
            let page = view
                .document()
                .page_at(&view, *page_index)
                .map_err(access)?;
            let paragraph = page
                .pdf_paragraph_at(&view, *paragraph_index)
                .map_err(access)?;
            needed_xobjects.insert(paragraph.xobj_id(&view).map_err(access)?);
        }
        let current = page_font_maps_for(&view, pair[0].0, &needed_xobjects)?;
        let next = page_font_maps_for(&view, pair[1].0, &needed_xobjects)?;
        let merged = merge_cross_page_font_maps(&current, &next);

        for (page_index, paragraph_index) in pair {
            let page = view
                .document()
                .page_at(&view, *page_index)
                .map_err(access)?;
            let paragraph = page
                .pdf_paragraph_at(&view, *paragraph_index)
                .map_err(access)?;
            let is_vertical = paragraph.vertical(&view).map_err(access)?.unwrap_or(false);
            let Some(index) = job_indices.get(&(*page_index, *paragraph_index)) else {
                continue;
            };
            let xobj_id = paragraph.xobj_id(&view).map_err(access)?;
            let font_styles = merged.for_xobject(xobj_id);
            let unicode = paragraph.unicode(&view).map_err(access)?.clone();
            let tracker_unicode = jobs[*index].tracker_unicode.clone();
            rebuild_job_inputs(&mut jobs[*index], is_vertical, || {
                let mut deferred_missing_font_style = None;
                let input = build_input(
                    &paragraph,
                    &view,
                    service,
                    regex,
                    patterns,
                    allow_rich,
                    mapper,
                    font_styles,
                    unicode.as_deref(),
                    &mut deferred_missing_font_style,
                )?;
                let missing_font_style = deferred_missing_font_style;
                let mut deferred_fallback_font_style = None;
                let fallback_input = build_input(
                    &paragraph,
                    &view,
                    service,
                    regex,
                    patterns,
                    allow_rich,
                    mapper,
                    font_styles,
                    Some(&tracker_unicode),
                    &mut deferred_fallback_font_style,
                )?;
                Ok((
                    input,
                    fallback_input,
                    missing_font_style,
                    deferred_fallback_font_style,
                ))
            })?;
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Job {
    pub(crate) page: usize,
    pub(crate) paragraph: usize,
    pub(crate) debug_id: Option<String>,
    pub(crate) text: String,
    pub(crate) tracker_unicode: String,
    pub(crate) vertical: bool,
    pub(crate) unicode_present: bool,
    pub(crate) layout_label: Option<String>,
    pub(crate) cid: bool,
    pub(crate) pure_numeric: bool,
    pub(crate) placeholder_only: bool,
    pub(crate) box_y2: Option<f64>,
    pub(crate) title_paragraph: bool,
    pub(crate) token_count: usize,
    pub(crate) title: Option<(String, String)>,
    pub(crate) local_title: Option<(String, String)>,
    pub(crate) eligible: bool,
    pub(crate) input: Option<stage::TranslateInput>,
    pub(crate) fallback_input: Option<stage::TranslateInput>,
    pub(crate) missing_font_style: Option<FontStyleLookupError>,
    pub(crate) fallback_missing_font_style: Option<FontStyleLookupError>,
}

fn rebuild_job_inputs(
    job: &mut Job,
    vertical: bool,
    build: impl FnOnce() -> PyResult<(
        Option<stage::TranslateInput>,
        Option<stage::TranslateInput>,
        Option<FontStyleLookupError>,
        Option<FontStyleLookupError>,
    )>,
) -> PyResult<()> {
    if vertical || job.vertical {
        job.eligible = false;
        job.input = None;
        job.fallback_input = None;
        job.missing_font_style = None;
        job.fallback_missing_font_style = None;
        return Ok(());
    }
    let (input, fallback_input, missing_font_style, fallback_missing_font_style) = build()?;
    job.eligible = input.is_some();
    job.input = input;
    job.fallback_input = fallback_input;
    job.missing_font_style = missing_font_style;
    job.fallback_missing_font_style = fallback_missing_font_style;
    Ok(())
}

#[derive(Clone)]
enum InputTextPart {
    Character {
        text: String,
        coords: Option<(f64, f64, f64, f64)>,
        font_id: Option<String>,
        font_size: Option<f64>,
    },
    Text(String),
    BoundaryOpen(String),
    BoundaryClose(String),
    BoundaryFormula {
        token: String,
        source: String,
        first: Option<(f64, f64, f64, f64)>,
        last: Option<(f64, f64, f64, f64)>,
    },
}

fn input_character_fragments(
    characters: impl IntoIterator<
        Item = Result<ir::Cursor<babeldoc_ir_core::types::PdfCharacter>, babeldoc_ir_core::AccessError>,
    >,
    view: &TreeView<'_, impl Permission>,
    boundary: bool,
) -> PyResult<Vec<InputTextPart>> {
    if !boundary {
        return character_fragments(characters, view);
    }
    characters.into_iter().map(|character| {
        let character = character.map_err(access)?;
        let text = character.char_unicode(view).map_err(access)?.clone().unwrap_or_default();
        let coords = if let Some(rect) = character.r#box(view).map_err(access)? {
            match (rect.x(view).map_err(access)?, rect.y(view).map_err(access)?,
                rect.x2(view).map_err(access)?, rect.y2(view).map_err(access)?) {
                (Some(x), Some(y), Some(x2), Some(y2))
                    if [x, y, x2, y2].iter().all(|value| value.is_finite()) && x2 > x && y2 > y =>
                        Some((x, y, x2, y2)),
                _ => None,
            }
        } else {
            None
        };
        let (font_id, font_size) = match character.pdf_style(view).map_err(access)? {
            Some(style) => (style.font_id(view).map_err(access)?.clone(), style.font_size(view).map_err(access)?),
            None => (None, None),
        };
        Ok(InputTextPart::Character { text, coords, font_id, font_size })
    }).collect()
}

fn boundary_character_coords(part: &InputTextPart) -> Option<(f64, f64, f64, f64)> {
    match part {
        InputTextPart::Character { text, coords, .. } if text.chars().any(|ch| !python_whitespace(ch)) => *coords,
        _ => None,
    }
}

fn character_fragments(
    characters: impl IntoIterator<
        Item = Result<
            ir::Cursor<babeldoc_ir_core::types::PdfCharacter>,
            babeldoc_ir_core::AccessError,
        >,
    >,
    view: &TreeView<'_, impl Permission>,
) -> PyResult<Vec<InputTextPart>> {
    characters
        .into_iter()
        .map(|character| {
            let character = character.map_err(access)?;
            let text = character
                .char_unicode(view)
                .map_err(access)?
                .clone()
                .unwrap_or_default();
            let coords = character
                .r#box(view)
                .map_err(access)?
                .map(|box_| {
                    Ok::<_, PyErr>((
                        box_.x(view).map_err(access)?.unwrap_or_default(),
                        box_.y(view).map_err(access)?.unwrap_or_default(),
                        box_.x2(view).map_err(access)?.unwrap_or_default(),
                        box_.y2(view).map_err(access)?.unwrap_or_default(),
                    ))
                })
                .transpose()?;
            let (font_id, font_size) = match character.pdf_style(view).map_err(access)? {
                Some(style) => (style.font_id(view).map_err(access)?.clone(), style.font_size(view).map_err(access)?),
                None => (None, None),
            };
            Ok(InputTextPart::Character { text, coords, font_id, font_size })
        })
        .collect()
}

fn python_whitespace(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

fn boundary_raw_text(parts: &[InputTextPart]) -> String {
    let mut raw = String::new();
    for part in parts {
        match part {
            InputTextPart::Character { text, .. } | InputTextPart::Text(text) => raw.push_str(text),
            InputTextPart::BoundaryFormula { source, .. } => raw.push_str(source),
            InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_) => {}
        }
    }
    raw
}

fn boundary_source_unambiguous<R: stage::PlaceholderRegex>(
    raw: &str,
    unicode: &str,
    patterns: &[u64],
    regex: &mut R,
) -> Result<bool, R::Error> {
    if patterns.len() != 3 || raw.contains("[[BABEL_") || unicode.contains("[[BABEL_") {
        return Ok(false);
    }
    for &pattern in patterns {
        if !regex.find_all(pattern, raw)?.is_empty() || !regex.find_all(pattern, unicode)?.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn boundary_whitespace_slots(text: &str) -> (String, Vec<String>) {
    let mut visible = String::new();
    let mut slots = vec![String::new()];
    for ch in text.chars() {
        if python_whitespace(ch) {
            slots.last_mut().expect("initial slot").push(ch);
        } else {
            let ligature = babeldoc_p2_stages::shared::layout_helper::expand_print_ligature(ch);
            if ligature.is_empty() {
                visible.push(ch);
                slots.push(String::new());
            } else {
                for expanded in ligature.chars() {
                    visible.push(expanded);
                    slots.push(String::new());
                }
            }
        }
    }
    (visible, slots)
}

#[derive(Clone)]
struct BoundaryAtom {
    text: String,
    start: usize,
    end: usize,
    first: Option<(f64, f64, f64, f64)>,
    last: Option<(f64, f64, f64, f64)>,
    part_index: usize,
    character: Option<char>,
    formula: bool,
}

enum BoundaryEvent<'a> {
    Atom(usize),
    Marker { text: &'a str, open: bool },
}

fn boundary_unicode_script(ch: char) -> bool {
    matches!(ch, '\u{00b2}' | '\u{00b3}' | '\u{00b9}' | '\u{2070}'..='\u{209f}'
        | '\u{1d2c}'..='\u{1d6a}' | '\u{1d9b}'..='\u{1dbf}')
}

fn boundary_unrepresented_script(left: &BoundaryAtom, right: &BoundaryAtom) -> bool {
    if left.formula || right.formula
        || left.character.is_some_and(boundary_unicode_script)
        || right.character.is_some_and(boundary_unicode_script) {
        return false;
    }
    if left.part_index == right.part_index {
        return false;
    }
    let (Some((left_x, left_y, left_x2, left_y2)), Some((right_x, right_y, right_x2, right_y2))) =
        (left.last, right.first) else { return false; };
    let left_height = left_y2 - left_y;
    let right_height = right_y2 - right_y;
    if (left_y2 < right_y || right_y2 < left_y) && right_x2 >= left_x
        && right_x - left_x2 <= left_height.max(right_height) * 0.75 {
        return true;
    }
    let height = left_height.max(right_height);
    let gap = right_x - left_x2;
    let overlap = left_y2.min(right_y2) - left_y.max(right_y);
    let center_shift = ((left_y + left_y2) - (right_y + right_y2)).abs() / 2.0;
    right_x2 >= left_x && gap <= height * 0.75 && overlap > 0.0
        && center_shift > height * 0.22
        && (left_height.min(right_height) < height * 0.85 || center_shift > height * 0.35)
}

fn compose_boundary_input_text(parts: &[InputTextPart], unicode: &str) -> Result<String, &'static str> {
    if parts.iter().any(|part| matches!(part, InputTextPart::Character { text, .. } if text.is_empty())) {
        return Err("empty source character");
    }
    let raw = boundary_raw_text(parts);
    let (raw_visible, mut spaces) = boundary_whitespace_slots(&raw);
    let original_spaces = spaces.clone();
    if !unicode.is_empty() {
        let (unicode_visible, unicode_spaces) = boundary_whitespace_slots(unicode);
        let raw_characters: Vec<_> = raw_visible.chars().collect();
        let unicode_characters: Vec<_> = unicode_visible.chars().collect();
        let matches = raw_characters.len() == unicode_characters.len()
            && raw_characters.iter().zip(&unicode_characters).all(|(&raw, &unicode)| {
                raw == unicode || boundary_unicode_script(raw)
                    && babeldoc_p2_stages::shared::layout_helper::normalize_unicode_nfkc(&raw.to_string()) == unicode.to_string()
            });
        if !matches {
            return Err("source characters disagree with paragraph unicode");
        }
        for (raw_space, unicode_space) in spaces.iter_mut().zip(unicode_spaces) {
            if !unicode_space.is_empty() {
                *raw_space = unicode_space;
            }
        }
    }

    let mut raw_position = 0;
    for part in parts {
        match part {
            InputTextPart::Character { text, .. } | InputTextPart::Text(text) => {
                raw_position += boundary_whitespace_slots(text).0.chars().count();
            }
            InputTextPart::BoundaryFormula { source, .. } => {
                let length = boundary_whitespace_slots(source).0.chars().count();
                let range = raw_position + 1..raw_position + length;
                if length > 1 && spaces[range.clone()].iter()
                    .zip(&original_spaces[range]).any(|(merged, original)| merged.is_empty() != original.is_empty()) {
                    return Err("paragraph whitespace changes formula source");
                }
                raw_position += length;
            }
            InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_) => {}
        }
    }

    let mut atoms = Vec::new();
    let mut events = Vec::new();
    let mut position = 0;
    for (part_index, part) in parts.iter().enumerate() {
        match part {
            InputTextPart::BoundaryOpen(text) => events.push(BoundaryEvent::Marker { text, open: true }),
            InputTextPart::BoundaryClose(text) => events.push(BoundaryEvent::Marker { text, open: false }),
            InputTextPart::BoundaryFormula { token, source, first, last } => {
                let end = position + boundary_whitespace_slots(source).0.chars().count();
                if (end == position && !raw_visible.is_empty()) || source.chars().all(python_whitespace) && !source.is_empty() {
                    return Err("formula has no visible source characters");
                }
                events.push(BoundaryEvent::Atom(atoms.len()));
                atoms.push(BoundaryAtom {
                    text: token.clone(), start: position, end, first: *first, last: *last,
                    part_index, character: None, formula: true,
                });
                position = end;
            }
            InputTextPart::Character { text, .. } | InputTextPart::Text(text) => {
                let coords = match part {
                    InputTextPart::Character { coords, .. } => *coords,
                    _ => None,
                };
                for ch in text.chars().filter(|ch| !python_whitespace(*ch)) {
                    let ligature = babeldoc_p2_stages::shared::layout_helper::expand_print_ligature(ch);
                    let expanded = if ligature.is_empty() { ch.to_string() } else { ligature.to_owned() };
                    let end = position + expanded.chars().count();
                    if spaces[position + 1..end].iter().any(|space| !space.is_empty()) {
                        return Err("paragraph whitespace splits a source ligature");
                    }
                    events.push(BoundaryEvent::Atom(atoms.len()));
                    atoms.push(BoundaryAtom {
                        text: expanded, start: position, end,
                        first: coords, last: coords, part_index, character: Some(ch), formula: false,
                    });
                    position = end;
                }
            }
        }
    }
    if atoms.is_empty() || position + 1 != spaces.len() {
        return Err("source text has no representable characters");
    }

    for pair in atoms.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        if !spaces[right.start].is_empty() {
            continue;
        }
        if boundary_unrepresented_script(left, right) {
            return Err("source contains an unrepresented geometric script");
        }
    }
    use babeldoc_p2_stages::shared::layout_helper::{inferred_word_spaces, SpacingUnit};
    let spacing_units: Vec<_> = parts.iter().filter_map(|part| match part {
        InputTextPart::Character { text, coords, font_id, font_size } => Some(SpacingUnit {
            text, bounds: *coords, font_id: font_id.as_deref(), font_size: *font_size,
        }),
        InputTextPart::Text(text) => Some(SpacingUnit { text, bounds: None, font_id: None, font_size: None }),
        InputTextPart::BoundaryFormula { source, first, last, .. } => Some(SpacingUnit {
            text: source, bounds: first.zip(*last).map(|(first, last)| (first.0, first.1.min(last.1), last.2, first.3.max(last.3))),
            font_id: None, font_size: None,
        }),
        InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_) => None,
    }).collect();
    let spacing = inferred_word_spaces(&spacing_units);
    let mut part_spacing = vec![false; parts.len()];
    let mut spacing_index = 0;
    for (index, part) in parts.iter().enumerate() {
        if !matches!(part, InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_)) {
            part_spacing[index] = spacing[spacing_index];
            spacing_index += 1;
        }
    }
    let mut output = String::new();
    let mut pending = Vec::new();
    let mut previous: Option<&BoundaryAtom> = None;
    let mut emitted_slots = vec![false; spaces.len()];
    for event in events {
        match event {
            BoundaryEvent::Marker { text, open } => pending.push((text, open)),
            BoundaryEvent::Atom(index) => {
                let atom = &atoms[index];
                let mut gap = if emitted_slots[atom.start] { String::new() } else { spaces[atom.start].clone() };
                if gap.is_empty() && !emitted_slots[atom.start] {
                    if previous.is_some_and(|left| left.part_index != atom.part_index) && part_spacing[atom.part_index] {
                        gap.push(' ');
                    }
                }
                let marker_count = pending.len();
                let insertion = pending.iter().position(|(_, open)| *open).unwrap_or(marker_count);
                for (index, (marker, _)) in pending.drain(..).enumerate() {
                    if index == insertion { output.push_str(&gap); }
                    output.push_str(marker);
                }
                if insertion == marker_count {
                    output.push_str(&gap);
                }
                emitted_slots[atom.start] = true;
                output.push_str(&atom.text);
                previous = Some(atom);
            }
        }
    }
    for (marker, _) in pending {
        output.push_str(marker);
    }
    if !emitted_slots[spaces.len() - 1] {
        output.push_str(spaces.last().expect("initial slot"));
    }
    Ok(output)
}

fn compose_input_text(parts: &[InputTextPart]) -> String {
    use babeldoc_p2_stages::shared::layout_helper::{inferred_word_spaces, normalize_source_text, SpacingUnit};
    let units: Vec<_> = parts.iter().filter_map(|part| match part {
        InputTextPart::Character { text, coords, font_id, font_size } => Some(SpacingUnit {
            text, bounds: *coords, font_id: font_id.as_deref(), font_size: *font_size,
        }),
        InputTextPart::Text(text) => Some(SpacingUnit { text, bounds: None, font_id: None, font_size: None }),
        InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_) => None,
        InputTextPart::BoundaryFormula { .. } => unreachable!("boundary-only input part"),
    }).collect();
    let spaces = inferred_word_spaces(&units);
    let mut output = String::new();
    let mut pending_markers = String::new();
    let mut index = 0;
    for part in parts {
        if let InputTextPart::BoundaryOpen(marker) | InputTextPart::BoundaryClose(marker) = part {
            pending_markers.push_str(marker);
            continue;
        }
        if spaces[index] { output.push(' '); }
        output.push_str(&pending_markers);
        pending_markers.clear();
        index += 1;
        match part {
            InputTextPart::Text(text) => output.push_str(text),
            InputTextPart::Character { text, .. } => output.push_str(text),
            InputTextPart::BoundaryOpen(_) | InputTextPart::BoundaryClose(_)
            | InputTextPart::BoundaryFormula { .. } => unreachable!("boundary-only input part"),
        }
    }
    output.push_str(&pending_markers);
    let collapsed = normalize_source_text(&output);
    babeldoc_p2_stages::translation::python_semantics::python_strip(&collapsed).to_owned()
}

pub(crate) fn collect(
    document: &mut Document,
    root: &babeldoc_p2_stages::runtime::shared_context::SharedContextRoot,
    service: &Bound<'_, PyAny>,
    regex: &mut RegexService<'_>,
    patterns: &stage::PlaceholderPatterns,
    allow_rich: bool,
    min_text_length: usize,
    llm_only_recent_title_admission: bool,
    mapper: &crate::font_mapper::FontMapperHandle,
) -> PyResult<(Vec<Job>, Vec<usize>, usize)> {
    let view = document.view::<ReadOnlyView>();
    let mut jobs = Vec::new();
    let mut count = 0;
    let mut page_lengths = Vec::new();
    let mut translated_ids = HashSet::new();
    let mut first = root.first_paragraph().map(|title| {
        (
            title.debug_id.unwrap_or_default(),
            title_hint_text(title.unicode),
        )
    });
    let mut recent = root.recent_title_paragraph().map(|title| {
        (
            title.debug_id.unwrap_or_default(),
            title_hint_text(title.unicode),
        )
    });
    if first.is_none() {
        'find_first: for page in view.document().page_iter(&view).map_err(access)? {
            let page = page.map_err(access)?;
            for paragraph in page.pdf_paragraph_iter(&view).map_err(access)? {
                let paragraph = paragraph.map_err(access)?;
                if paragraph.layout_label(&view).map_err(access)?.as_deref() == Some("title") {
                    let snapshot =
                        babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot {
                            debug_id: paragraph.debug_id(&view).map_err(access)?.clone(),
                            unicode: paragraph.unicode(&view).map_err(access)?.clone(),
                            layout_label: Some("title".into()),
                        };
                    let title = (
                        snapshot.debug_id.clone().unwrap_or_default(),
                        title_hint_text(snapshot.unicode.clone()),
                    );
                    // The initial title belongs to the document root, not just
                    // this part's job builder. Later parts must retain it.
                    root.set_first_paragraph(Some(snapshot.clone()));
                    root.set_recent_title_paragraph(Some(snapshot));
                    first = Some(title.clone());
                    recent = Some(title);
                    break 'find_first;
                }
            }
        }
    }
    for (page_index, page) in view
        .document()
        .page_iter(&view)
        .map_err(access)?
        .enumerate()
    {
        let page = page.map_err(access)?;
        let paragraph_len = page.pdf_paragraph_len(&view).map_err(access)?;
        page_lengths.push(paragraph_len);
        let mut font_styles = HashMap::new();
        add_page_font_styles(
            &mut font_styles,
            page.pdf_font_iter(&view).map_err(access)?,
            &view,
        )?;
        for (paragraph_index, paragraph) in
            page.pdf_paragraph_iter(&view).map_err(access)?.enumerate()
        {
            let paragraph = paragraph.map_err(access)?;
            count += 1;
            let unicode = paragraph.unicode(&view).map_err(access)?;
            let tracker_unicode = if unicode.is_some() {
                paragraph_unicode(&paragraph, &view)?
            } else {
                String::new()
            };
            let vertical = paragraph.vertical(&view).map_err(access)?.unwrap_or(false);
            let label = paragraph.layout_label(&view).map_err(access)?;
            let debug_id = paragraph.debug_id(&view).map_err(access)?.clone();
            let paragraph_box = paragraph.r#box(&view).map_err(access)?;
            let box_y2 = paragraph_box
                .map(|value| value.y2(&view).map_err(access))
                .transpose()?
                .flatten();
            let mut cid_count = 0usize;
            let mut cid_total = 0usize;
            let mut placeholder_only = unicode.as_deref().is_some_and(|text| !text.is_empty());
            let mut count_character = |character: &ir::Cursor<
                babeldoc_ir_core::types::PdfCharacter,
            >| {
                cid_total += 1;
                let text = character
                    .char_unicode(&view)
                    .ok()
                    .and_then(|value| value.as_deref());
                if text.as_deref().is_some_and(|value| {
                    value
                        .strip_prefix("(cid:")
                        .and_then(|value| value.strip_suffix(')'))
                        .is_some_and(|digits| {
                            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
                        })
                }) {
                    cid_count += 1;
                }
            };
            for composition in paragraph
                .pdf_paragraph_composition_iter(&view)
                .map_err(access)?
            {
                let composition = composition.map_err(access)?;
                if let Some(line) = composition.pdf_line(&view).map_err(access)? {
                    for character in line.pdf_character_iter(&view).map_err(access)? {
                        let character = character.map_err(access)?;
                        placeholder_only &= character
                            .char_unicode(&view)
                            .ok()
                            .and_then(|value| value.as_deref())
                            .as_deref()
                            .is_some_and(
                                babeldoc_p2_stages::translation::python_semantics::python_isspace,
                            );
                        count_character(&character);
                    }
                } else if let Some(group) = composition
                    .pdf_same_style_characters(&view)
                    .map_err(access)?
                {
                    for character in group.pdf_character_iter(&view).map_err(access)? {
                        let character = character.map_err(access)?;
                        placeholder_only &= character
                            .char_unicode(&view)
                            .ok()
                            .and_then(|value| value.as_deref())
                            .as_deref()
                            .is_some_and(
                                babeldoc_p2_stages::translation::python_semantics::python_isspace,
                            );
                        count_character(&character);
                    }
                } else if let Some(group) = composition
                    .pdf_same_style_unicode_characters(&view)
                    .map_err(access)?
                {
                    placeholder_only &= group
                        .unicode(&view)
                        .map_err(access)?
                        .as_deref()
                        .is_some_and(
                            babeldoc_p2_stages::translation::python_semantics::python_isspace,
                        );
                } else if let Some(formula) = composition.pdf_formula(&view).map_err(access)? {
                    for character in formula.pdf_character_iter(&view).map_err(access)? {
                        count_character(&character.map_err(access)?);
                    }
                } else if let Some(character) = composition.pdf_character(&view).map_err(access)? {
                    placeholder_only &= character
                        .char_unicode(&view)
                        .map_err(access)?
                        .as_deref()
                        .is_some_and(
                            babeldoc_p2_stages::translation::python_semantics::python_isspace,
                        );
                    count_character(&character);
                } else {
                    placeholder_only = false;
                }
            }
            let cid = cid_count * 5 > cid_total * 4;
            let title_paragraph = label.as_deref() == Some("title");
            let pure_numeric = unicode.as_deref().is_some_and(
                babeldoc_p2_stages::translation::paragraph_helpers::is_pure_numeric_text,
            );
            let mut deferred_missing_font_style = None;
            if title_paragraph && !llm_only_recent_title_admission {
                let title_id = debug_id.clone().unwrap_or_default();
                recent = Some((title_id.clone(), title_hint_text(unicode.clone())));
                root.set_recent_title_paragraph(Some(
                    babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot {
                        debug_id: Some(title_id),
                        unicode: unicode.clone(),
                        layout_label: label.clone(),
                    },
                ));
            }
            if first.is_none() && title_paragraph {
                let snapshot = babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot {
                    debug_id: debug_id.clone(),
                    unicode: unicode.clone(),
                    layout_label: label.clone(),
                };
                if let Some(first_snapshot) =
                    initialize_first_title(&mut first, &mut recent, snapshot)
                {
                    root.set_first_paragraph(Some(first_snapshot.clone()));
                    root.set_recent_title_paragraph(Some(first_snapshot));
                }
            }
            if let Some(text) = unicode.clone() {
                let (input, fallback_input, fallback_missing_font_style) = if vertical {
                    (None, None, None)
                } else {
                    let xobj_id = paragraph.xobj_id(&view).map_err(access)?;
                    let mut paragraph_font_styles = font_styles.clone();
                    for xobj in page.pdf_xobject_iter(&view).map_err(access)? {
                        let xobj = xobj.map_err(access)?;
                        if xobj.xobj_id(&view).map_err(access)? == xobj_id {
                            add_page_font_styles(
                                &mut paragraph_font_styles,
                                xobj.pdf_font_iter(&view).map_err(access)?,
                                &view,
                            )?;
                            break;
                        }
                    }
                    let input = build_input(
                        &paragraph,
                        &view,
                        service,
                        regex,
                        patterns,
                        allow_rich,
                        mapper,
                        &paragraph_font_styles,
                        Some(&text),
                        &mut deferred_missing_font_style,
                    )?;
                    let mut deferred_fallback_font_style = None;
                    let fallback_input = build_input(
                        &paragraph,
                        &view,
                        service,
                        regex,
                        patterns,
                        allow_rich,
                        mapper,
                        &paragraph_font_styles,
                        Some(&tracker_unicode),
                        &mut deferred_fallback_font_style,
                    )?;
                    (input, fallback_input, deferred_fallback_font_style)
                };
                let eligible = input.is_some();
                let passes_llm_filters = debug_id.is_some()
                    && text.chars().count() >= min_text_length
                    && !cid
                    && !pure_numeric
                    && !placeholder_only;
                let first_occurrence =
                    passes_llm_filters && translated_ids.insert(paragraph.identity());
                let admitted = first_occurrence;
                if title_paragraph && llm_only_recent_title_admission && admitted {
                    recent = Some((debug_id.clone().unwrap_or_default(), text.clone()));
                    root.set_recent_title_paragraph(Some(
                        babeldoc_p2_stages::runtime::shared_context::TitleContextSnapshot {
                            debug_id: debug_id.clone(),
                            unicode: Some(text.clone()),
                            layout_label: label.clone(),
                        },
                    ));
                }
                jobs.push(Job {
                    page: page_index,
                    paragraph: paragraph_index,
                    debug_id: paragraph.debug_id(&view).map_err(access)?.clone(),
                    token_count: babeldoc_p2_stages::translation::token_count::count_tokens(&text),
                    unicode_present: true,
                    layout_label: label.clone(),
                    cid,
                    pure_numeric,
                    placeholder_only,
                    box_y2,
                    title_paragraph,
                    text,
                    tracker_unicode,
                    vertical,
                    title: first.clone(),
                    local_title: recent.clone(),
                    eligible,
                    input,
                    fallback_input,
                    missing_font_style: deferred_missing_font_style,
                    fallback_missing_font_style,
                });
            } else {
                jobs.push(Job {
                    page: page_index,
                    paragraph: paragraph_index,
                    debug_id: paragraph.debug_id(&view).map_err(access)?.clone(),
                    token_count: 0,
                    unicode_present: false,
                    layout_label: label.clone(),
                    cid,
                    pure_numeric: false,
                    placeholder_only,
                    box_y2,
                    title_paragraph,
                    text: String::new(),
                    tracker_unicode: String::new(),
                    vertical,
                    title: first.clone(),
                    local_title: recent.clone(),
                    eligible: false,
                    input: None,
                    fallback_input: None,
                    missing_font_style: None,
                    fallback_missing_font_style: None,
                });
            }
        }
    }
    Ok((jobs, page_lengths, count))
}

pub fn process(
    py: Python<'_>,
    document: &mut Document,
    config: &Bound<'_, PyAny>,
    shared_context: &Bound<'_, PyAny>,
    mapper: &FontMapperHandle,
    dispatcher: Py<PyAny>,
    translator_service: Py<PyAny>,
    flag: CancelFlag,
    monitor: Py<PyAny>,
) -> PyResult<()> {
    let _native_font_mapper = &mapper.mapper;
    let root = crate::shared_context::root_from_handle(shared_context)?;
    let translator = translator_service.bind(py).clone();
    let supports_llm = translator
        .call_method0("probe_il_translator")?
        .extract::<bool>()?;
    let disable_rich = config
        .getattr("disable_rich_text_translate")?
        .extract::<bool>()?;
    let allow_rich = supports_llm && !disable_rich;
    let mut regex = RegexService {
        service: translator.clone(),
    };
    // BaseTranslator returns strings. Its [1] indexing is part of 0.6.4's
    // d10 behavior; OpenAI shaped returns expose the regex tuple member.
    let formula_pattern = legacy_pattern(&translator, "get_formular_placeholder")?;
    let left_pattern = legacy_pattern(&translator, "get_rich_text_left_placeholder")?;
    let right_pattern = legacy_pattern(&translator, "get_rich_text_right_placeholder")?;
    let patterns = stage::PlaceholderPatterns {
        formula: formula_pattern,
        rich_left: left_pattern,
        rich_right: right_pattern,
    };
    let hallucination_ids = [
        regex.compile(&patterns.formula, true)?,
        regex.compile(&patterns.rich_left, true)?,
        regex.compile(&patterns.rich_right, true)?,
    ];
    let min_length = config.getattr("min_text_length")?.extract::<usize>()?;
    let (mut jobs, page_lengths, total) = collect(
        document,
        &root,
        &translator,
        &mut regex,
        &patterns,
        allow_rich,
        min_length,
        false,
        mapper,
    )?;
    let candidates: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let cross_page_ids = if candidates.is_empty() {
        None
    } else {
        cross_page_patterns(&translator, &mut regex).ok()
    };
    let mut paired = Vec::new();
    if let Some(ids) = &cross_page_ids {
        let mut candidates_jobs = jobs.clone();
        let mut safe = true;
        for job in &mut candidates_jobs {
            if !candidates.iter().any(|(left, right)| *left == (job.page, job.paragraph) || *right == (job.page, job.paragraph)) { continue; }
            let Some(input) = &mut job.input else { continue };
            let mut counts = std::collections::BTreeMap::new();
            for id in ids {
                match regex.find_all(*id, &job.text) {
                    Ok(matches) => for found in matches { *counts.entry(found.full).or_insert(0) += 1; },
                    Err(_) => { safe = false; break; }
                }
            }
            input.original_tokens = counts.into_iter().collect();
        }
        if safe {
            let prepared = crate::cross_page::prepare_pairs(&mut candidates_jobs, &candidates, &mut regex, ids, min_length);
            for (left, right) in prepared {
                let left_index = jobs.iter().position(|job| (job.page, job.paragraph) == (left.page, left.paragraph)).unwrap();
                let right_index = jobs.iter().position(|job| (job.page, job.paragraph) == (right.page, right.paragraph)).unwrap();
                paired.push((jobs[left_index].clone(), jobs[right_index].clone(), left, right));
                jobs[left_index] = candidates_jobs[left_index].clone();
                jobs[right_index] = candidates_jobs[right_index].clone();
            }
        }
    }
    let workers = config
        .getattr("pool_max_workers")?
        .extract::<usize>()?
        .max(1);
    let lang_out = config.getattr("lang_out")?.extract::<String>()?;
    let custom_prompt = config
        .getattr("custom_system_prompt")
        .ok()
        .and_then(|v| v.extract::<String>().ok());
    let add_formula_hint = config
        .getattr("add_formula_placehold_hint")
        .and_then(|value| value.extract::<bool>())
        .unwrap_or(false);
    let auto_extract = config
        .getattr("auto_extract_glossary")?
        .extract::<bool>()
        .unwrap_or(false);
    let tracking_path = config
        .call_method1("get_working_file_path", ("translate_tracking.json",))?
        .str()?
        .to_str()?
        .to_owned();
    let write_tracking =
        config.getattr("debug")?.extract::<bool>()? || !config.getattr("working_dir")?.is_none();
    let glossaries = root.get_glossaries_for_translation(auto_extract);
    let roles = stage::role_block(custom_prompt.as_deref(), &lang_out);
    let regex_service_for_parse = translator_service.clone_ref(py);
    let recorder = Recorder::for_dispatcher(py, dispatcher.bind(py));
    let service = PythonService {
        dispatcher,
        recorder,
    };
    let pool = Arc::new(TranslatorPoolPair::new(
        service,
        stage::STAGE_NAME,
        workers,
        root,
    ));
    let result = callbacks::run(py, workers, move |runtime, progress_run| {
        let progress = progress_run
            .start(
                monitor,
                stage::STAGE_NAME,
                total as u64,
                Duration::from_millis(30),
            )
            .map_err(ProgressError::into_python)?;
        let result = runtime.block_on(async {
            let mut tasks = tokio::task::JoinSet::new();
            let mut tracking = stage::TranslationTracking {
                page: page_lengths
                    .iter()
                    .map(|count| stage::PageTracking {
                        paragraph: vec![stage::ParagraphTracking::default(); *count],
                    })
                    .collect(),
            };
            let mut page_error = None;
            let mut current_page = None;
            let mut submitted = 0usize;
            let mut translated = Vec::new();
            let mut admitted_tracking = Vec::new();
            let pair_keys: HashSet<_> = paired.iter().flat_map(|(left, right, _, _)| [(left.page, left.paragraph), (right.page, right.paragraph)]).collect();
            for job in jobs {
                if current_page != Some(job.page) {
                    current_page = Some(job.page);
                    if let Err(error) = flag.checkpoint() {
                        page_error = Some(error);
                        break;
                    }
                }
                let page = job.page;
                let report_progress = !pair_keys.contains(&(job.page, job.paragraph));
                let slot = &mut tracking.page[page].paragraph[job.paragraph];
                if job.missing_font_style.is_some() {
                    progress.report(1, None).map_err(|e| e.into_python())?;
                    continue;
                }
                if !job.eligible {
                    progress.report(1, None).map_err(|e| e.into_python())?;
                    continue;
                }
                let input = job
                    .input
                    .as_ref()
                    .expect("eligible job has a translate input");
                if input.text.chars().count() < min_length {
                    slot.pdf_unicode = Some(job.text.clone());
                    slot.input = Some(input.text.clone());
                    slot.original_placeholders = input.original_tokens.clone();
                    slot.placeholders = tracking_placeholders(input);
                    progress.report(1, None).map_err(|e| e.into_python())?;
                    continue;
                }
                let admitted = Arc::new(AtomicBool::new(false));
                admitted_tracking.push((
                    job.page,
                    job.paragraph,
                    job.text.clone(),
                    input.text.clone(),
                    input.original_tokens.clone(),
                    tracking_placeholders(input),
                    Arc::clone(&admitted),
                ));
                slot.llm_translate_trackers
                    .push(stage::LlmTracking::default());
                if supports_llm {
                    let context = stage::context_block(
                        job.title
                            .as_ref()
                            .map(|(id, text)| (id.as_str(), text.as_str())),
                        job.local_title
                            .as_ref()
                            .map(|(id, text)| (id.as_str(), text.as_str())),
                        add_formula_hint
                            .then(|| formula_hint(input))
                            .flatten()
                            .as_deref(),
                    );
                    let prompt = stage::render_prompt(
                        &roles,
                        &lang_out,
                        &stage::glossary_block(&input.text, &glossaries),
                        &context,
                        &input.text,
                    );
                    if let Some(track) = slot.llm_translate_trackers.last_mut() {
                        track.input = prompt.clone();
                    }
                    let admitted = spawn_request(
                        &mut tasks,
                        Arc::clone(&pool),
                        job,
                        prompt,
                        true,
                        flag.clone(),
                        progress.clone(),
                        admitted,
                        report_progress,
                    );
                    submitted += 1;
                    if submitted <= workers {
                        let _ = admitted.await;
                    }
                    tokio::task::yield_now().await;
                } else {
                    let admitted = spawn_request(
                        &mut tasks,
                        Arc::clone(&pool),
                        job.clone(),
                        input.text.clone(),
                        false,
                        flag.clone(),
                        progress.clone(),
                        admitted,
                        report_progress,
                    );
                    submitted += 1;
                    if submitted <= workers {
                        let _ = admitted.await;
                    }
                    tokio::task::yield_now().await;
                }
            }
            let has_pairs = !paired.is_empty();
            while let Some(joined) = tasks.join_next().await {
                if has_pairs && page_error.is_none() {
                    page_error = flag.checkpoint().err();
                }
                match joined {
                    Ok(Ok(Some((job, response, content_filter)))) => {
                        translated.push((job, response, content_filter))
                    }
                    Ok(Ok(None)) => continue,
                    Ok(Err(error)) if !has_pairs || page_error.is_none() => return Err(error),
                    Err(error) if !has_pairs || page_error.is_none() => return Err(PyRuntimeError::new_err(error.to_string())),
                    _ => continue,
                }
            }
            if has_pairs {
                if let Some(error) = page_error.take() {
                    return Err(error);
                }
                flag.checkpoint()?;
            }
            let mut failed_pairs = HashSet::new();
            let mut paired_results = HashSet::new();
            let mut fallback_inputs = HashMap::new();
            let mut fallback_jobs = Vec::new();
            for (left, right, safe_left, safe_right) in paired {
                let left_key = (left.page, left.paragraph);
                let right_key = (right.page, right.paragraph);
                let redistributed = Python::attach(|py| -> PyResult<Option<(String, String)>> {
                    let mut regex = RegexService {
                        service: regex_service_for_parse.bind(py).clone(),
                    };
                    let mut outputs = Vec::new();
                    let mut inputs = Vec::new();
                    for key in [left_key, right_key] {
                        let mut matches = translated.iter().filter(|(job, _, _)| {
                            (job.page, job.paragraph) == key
                        });
                        let Some((job, Ok(output), false)) = matches.next() else {
                            return Ok(None);
                        };
                        if matches.next().is_some() {
                            return Ok(None);
                        }
                        let Some(input) = job.input.as_ref() else {
                            return Ok(None);
                        };
                        inputs.push(input);
                        outputs.push(stage::normalize_response_punctuation(output));
                    }
                    babeldoc_p2_stages::translation::cross_page_protocol::redistribute_response(
                        inputs[0], &outputs[0], inputs[1], &outputs[1],
                        safe_left.input.as_ref().expect("prepared left input"),
                        safe_right.input.as_ref().expect("prepared right input"),
                        cross_page_ids.as_ref().expect("paired inputs have patterns"), &mut regex,
                    )
                }).unwrap_or(None);
                if let Some((left_output, right_output)) = redistributed {
                    translated.retain(|(job, _, _)| ![left_key, right_key].contains(&(job.page, job.paragraph)));
                    for job in [&safe_left, &safe_right] {
                        if let Some(entry) = admitted_tracking.iter_mut().find(|entry| (entry.0, entry.1) == (job.page, job.paragraph)) {
                            let input = job.input.as_ref().expect("prepared original input");
                            entry.3 = input.text.clone();
                            entry.4 = input.original_tokens.clone();
                            entry.5 = tracking_placeholders(input);
                        }
                    }
                    translated.push((safe_left, Ok(left_output), false));
                    translated.push((safe_right, Ok(right_output), false));
                    paired_results.extend([left_key, right_key]);
                    progress.report(2, None).map_err(|e| e.into_python())?;
                } else {
                    failed_pairs.extend([left_key, right_key]);
                    fallback_inputs.insert(left_key, safe_left.input.clone());
                    fallback_inputs.insert(right_key, safe_right.input.clone());
                    fallback_jobs.extend([safe_left, safe_right]);
                }
            }
            translated.retain(|(job, _, _)| {
                !failed_pairs.contains(&(job.page, job.paragraph))
            });
            for job in fallback_jobs {
                let Some(input) = job.input.as_ref() else {
                    continue;
                };
                let text = if supports_llm {
                    let context = stage::context_block(
                        job.title.as_ref().map(|(id, text)| (id.as_str(), text.as_str())),
                        job.local_title.as_ref().map(|(id, text)| (id.as_str(), text.as_str())),
                        add_formula_hint.then(|| formula_hint(input)).flatten().as_deref(),
                    );
                    stage::render_prompt(&roles, &lang_out, &stage::glossary_block(&input.text, &glossaries), &context, &input.text)
                } else { input.text.clone() };
                let admitted = Arc::new(AtomicBool::new(false));
                admitted_tracking.retain(|(page, paragraph, ..)| (*page, *paragraph) != (job.page, job.paragraph));
                admitted_tracking.push((job.page, job.paragraph, job.text.clone(), input.text.clone(), input.original_tokens.clone(), tracking_placeholders(input), Arc::clone(&admitted)));
                let slot = &mut tracking.page[job.page].paragraph[job.paragraph];
                slot.llm_translate_trackers.clear();
                let mut track = stage::LlmTracking::default();
                if supports_llm { track.input = text.clone(); }
                slot.llm_translate_trackers.push(track);
                spawn_request(
                    &mut tasks,
                    Arc::clone(&pool),
                    job,
                    text,
                    supports_llm,
                    flag.clone(),
                    progress.clone(),
                    admitted,
                    false,
                );
            }
            while let Some(joined) = tasks.join_next().await {
                flag.checkpoint()?;
                match joined {
                    Ok(Ok(Some((job, response, content_filter)))) => {
                        if failed_pairs.contains(&(job.page, job.paragraph)) && (response.is_err() || content_filter) {
                            return Err(PyRuntimeError::new_err("Cross-page fallback translation failed"));
                        }
                        if let Some(Some(input)) = fallback_inputs.get(&(job.page, job.paragraph)) {
                            let valid = Python::attach(|py| -> PyResult<bool> {
                                let mut regex = RegexService { service: regex_service_for_parse.bind(py).clone() };
                                let text = stage::normalize_response_punctuation(response.as_ref().expect("response checked"));
                                babeldoc_p2_stages::translation::cross_page_protocol::validate_response(
                                    &mut regex, input, &text, cross_page_ids.as_ref().expect("paired inputs have patterns"),
                                )
                            }).unwrap_or(false);
                            if !valid { return Err(PyRuntimeError::new_err("Cross-page fallback response validation failed")); }
                        }
                        translated.push((job, response, content_filter));
                    }
                    Ok(Ok(None)) => continue,
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(PyRuntimeError::new_err(error.to_string())),
                }
            }
            if failed_pairs.iter().any(|key| !translated.iter().any(|(job, _, _)| (job.page, job.paragraph) == *key)) {
                flag.checkpoint()?;
                return Err(PyRuntimeError::new_err("Cross-page fallback response missing"));
            }
            if has_pairs {
                flag.checkpoint()?;
            }
            for _ in 0..failed_pairs.len() / 2 {
                progress.report(2, None).map_err(|e| e.into_python())?;
            }
            for (page, paragraph, pdf_unicode, input, originals, placeholders, admitted) in
                admitted_tracking
            {
                if admitted.load(Ordering::Acquire) {
                    let slot = &mut tracking.page[page].paragraph[paragraph];
                    slot.pdf_unicode = Some(pdf_unicode);
                    slot.input = Some(input);
                    slot.original_placeholders = originals;
                    slot.placeholders = placeholders;
                }
            }
            if let Some(error) = page_error {
                return Err(error);
            }
            // Apply completions after all worker jobs have finished. This keeps
            // the M tree serial while retaining independent paragraph requests.
            let mut view = document.view::<TranslationView>();
            for (job, response, content_filter) in translated {
                if content_filter {
                    let page = view.document().page_at(&view, job.page).map_err(access)?;
                    let paragraph = page
                        .pdf_paragraph_at(&view, job.paragraph)
                        .map_err(access)?;
                    let original_box = paragraph
                        .r#box(&view)
                        .map_err(|e| {
                            PyRuntimeError::new_err(format!("content filter paragraph box: {e}"))
                        })?
                        .ok_or_else(|| {
                            PyRuntimeError::new_err("content-filtered paragraph has no box")
                        })?;
                    let y2 = original_box
                        .y2(&view)
                        .map_err(|e| {
                            PyRuntimeError::new_err(format!("content filter box y2: {e}"))
                        })?
                        .ok_or_else(|| {
                            PyRuntimeError::new_err("content-filtered paragraph box has no y2")
                        })?;
                    let box_value = ir::BoxInit {
                        x: original_box.x(&view).map_err(|e| {
                            PyRuntimeError::new_err(format!("content filter box x: {e}"))
                        })?,
                        y: Some(y2),
                        x2: original_box.x2(&view).map_err(|e| {
                            PyRuntimeError::new_err(format!("content filter box x2: {e}"))
                        })?,
                        y2: Some(y2 + 1.1),
                    }
                    .into_value();
                    let hint_index = page.pdf_paragraph_len(&view).map_err(access)?;
                    let hint = ir::PdfParagraphInit {
                        r#box: Some(box_value),
                        pdf_style: Some(
                            ir::PdfStyleInit {
                                font_id: Some("base".into()),
                                font_size: Some(1.0),
                                graphic_state: Some(
                                    ir::GraphicStateInit {
                                        passthrough_per_char_instruction: Some(
                                            ir::Instructions::Materialized("0.80 g 0.80 G".into()),
                                        ),
                                    }
                                    .into_value(),
                                ),
                            }
                            .into_value(),
                        ),
                        xobj_id: Some(-1),
                        unicode: Some(CONTENT_FILTER_HINT.into()),
                        vertical: Some(false),
                        first_line_indent: Some(false),
                        ..Default::default()
                    }
                    .into_value();
                    page.push_pdf_paragraph(&mut view, hint).map_err(|e| {
                        PyRuntimeError::new_err(format!("content filter page append: {e}"))
                    })?;
                    let hint_paragraph =
                        page.pdf_paragraph_at(&view, hint_index).map_err(access)?;
                    let style = hint_paragraph
                        .pdf_style(&view)
                        .map_err(access)?
                        .ok_or_else(|| PyRuntimeError::new_err("content-filter style missing"))?
                        .share(&mut view)
                        .map_err(access)?;
                    let chars = ir::PdfSameStyleUnicodeCharactersInit {
                        unicode: Some(CONTENT_FILTER_HINT.into()),
                        pdf_style: Some(style),
                        debug_info: Some(true),
                    }
                    .into_value();
                    let composition = ir::PdfParagraphCompositionInit {
                        pdf_same_style_unicode_characters: Some(chars),
                        ..Default::default()
                    }
                    .into_value();
                    hint_paragraph
                        .set_pdf_paragraph_composition(&mut view, vec![composition])
                        .map_err(access)?;
                    continue;
                }
                if let Ok(text) = response {
                    let text = stage::normalize_response_punctuation(&text);
                    let Some(input) = job.input.as_ref() else {
                        continue;
                    };
                    let translation_unchanged = text == input.text;
                    let (parsed, placeholder_full_match, removed) =
                        Python::attach(|py| -> PyResult<_> {
                            let mut regex = RegexService {
                                service: regex_service_for_parse.bind(py).clone(),
                            };
                            let mut removed = Vec::new();
                            let (parsed, full) = stage::parse_output(
                                &mut regex,
                                input,
                                &text,
                                if paired_results.contains(&(job.page, job.paragraph)) || failed_pairs.contains(&(job.page, job.paragraph)) { cross_page_ids.as_ref().expect("paired inputs have patterns") } else { &hallucination_ids },
                                &mut removed,
                            )?;
                            Ok((parsed, full, removed))
                        })?;
                    let paragraph = view
                        .document()
                        .page_at(&view, job.page)
                        .map_err(access)?
                        .pdf_paragraph_at(&view, job.paragraph)
                        .map_err(access)?;
                    if text != input.text || paired_results.contains(&(job.page, job.paragraph)) {
                        paragraph
                            .set_unicode(&mut view, Some(text.clone()))
                            .map_err(access)?;
                        let mut new_compositions = Vec::new();
                        for part in parsed {
                            let composition = match part {
                                stage::ParsedPart::Text(value) => {
                                    let style = paragraph
                                        .pdf_style(&view)
                                        .map_err(access)?
                                        .map(|c| c.share(&mut view))
                                        .transpose()
                                        .map_err(access)?;
                                    let same = ir::PdfSameStyleUnicodeCharactersInit {
                                        pdf_style: style,
                                        unicode: Some(value),
                                        debug_info: None,
                                    }
                                    .into_value();
                                    ir::PdfParagraphCompositionInit {
                                        pdf_same_style_unicode_characters: Some(same),
                                        ..Default::default()
                                    }
                                    .into_value()
                                }
                                stage::ParsedPart::Formula { composition_index } => {
                                    let old = paragraph
                                        .pdf_paragraph_composition_at(&view, composition_index)
                                        .map_err(access)?;
                                    let formula = old
                                        .pdf_formula(&view)
                                        .map_err(access)?
                                        .ok_or_else(|| {
                                            PyRuntimeError::new_err(
                                                "formula placeholder source disappeared",
                                            )
                                        })?;
                                    let formula = formula.share(&mut view).map_err(access)?;
                                    ir::PdfParagraphCompositionInit {
                                        pdf_formula: Some(formula),
                                        ..Default::default()
                                    }
                                    .into_value()
                                }
                                stage::ParsedPart::Rich {
                                    composition_index,
                                    text: value,
                                    matches_source,
                                } => {
                                    let old = paragraph
                                        .pdf_paragraph_composition_at(&view, composition_index)
                                        .map_err(access)?;
                                    let rich = old
                                        .pdf_same_style_characters(&view)
                                        .map_err(access)?
                                        .ok_or_else(|| {
                                            PyRuntimeError::new_err(
                                                "rich placeholder source disappeared",
                                            )
                                        })?;
                                    if !matches_source {
                                        let style = rich
                                            .pdf_style(&view)
                                            .map_err(access)?
                                            .map(|c| c.share(&mut view))
                                            .transpose()
                                            .map_err(access)?;
                                        let same = ir::PdfSameStyleUnicodeCharactersInit {
                                            pdf_style: style,
                                            unicode: Some(value),
                                            debug_info: None,
                                        }
                                        .into_value();
                                        ir::PdfParagraphCompositionInit {
                                            pdf_same_style_unicode_characters: Some(same),
                                            ..Default::default()
                                        }
                                        .into_value()
                                    } else {
                                        let rich = rich.share(&mut view).map_err(access)?;
                                        ir::PdfParagraphCompositionInit {
                                            pdf_same_style_characters: Some(rich),
                                            ..Default::default()
                                        }
                                        .into_value()
                                    }
                                }
                            };
                            new_compositions.push(composition);
                        }
                        paragraph
                            .set_pdf_paragraph_composition(&mut view, new_compositions)
                            .map_err(access)?;
                    }
                    if let Some(slot) = tracking
                        .page
                        .get_mut(job.page)
                        .and_then(|p| p.paragraph.get_mut(job.paragraph))
                    {
                        slot.output = Some(text);
                        slot.removed_hallucinated_placeholders = removed;
                        if let Some(last) = slot.llm_translate_trackers.last_mut() {
                            if supports_llm {
                                last.output = slot.output.clone().unwrap_or_default();
                            }
                            last.placeholder_full_match =
                                placeholder_full_match || translation_unchanged;
                        }
                    }
                }
            }
            Ok(tracking)
        });
        match result {
            Ok(tracking) => {
                progress.end(None).map_err(ProgressError::into_python)?;
                if write_tracking {
                    if let Some(parent) = std::path::Path::new(&tracking_path).parent() {
                        std::fs::create_dir_all(parent).map_err(access)?;
                    }
                    std::fs::write(&tracking_path, tracking.to_json()).map_err(access)
                } else {
                    Ok(())
                }
            }
            Err(error) => {
                let _ = progress.end(None);
                Err(error)
            }
        }
    });
    result
}

#[cfg(test)]
mod ordinary_translation_tests;

fn spawn_request(
    tasks: &mut tokio::task::JoinSet<PyResult<Option<(Job, PyResult<String>, bool)>>>,
    pool: Arc<TranslatorPoolPair>,
    job: Job,
    input: String,
    llm: bool,
    flag: CancelFlag,
    progress: PythonProgress,
    recorded: Arc<AtomicBool>,
    report_progress: bool,
) -> oneshot::Receiver<()> {
    let (admitted_tx, admitted_rx) = oneshot::channel();
    tasks.spawn(async move {
        let method = if llm { "llm_translate" } else { "translate" };
        let (args, kwargs) = Python::attach(|py| -> PyResult<_> {
            let args = PyTuple::new(py, [input.clone()])?.unbind();
            let params = PyDict::new(py);
            params.set_item("paragraph_token_count", job.token_count)?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("rate_limit_params", params)?;
            Ok((args, kwargs.unbind()))
        })?;
        let priority = 1_048_576 - job.token_count as i64;
        let request_flag = flag.clone();
        let response = pool
            .request(
                method,
                format!("translate:{}:{}", job.page, job.paragraph),
                args,
                Some(kwargs),
                priority,
                move || {
                    let _ = admitted_tx.send(());
                    let allowed = !request_flag.is_cancelled();
                    if allowed {
                        recorded.store(true, Ordering::Release);
                    }
                    allowed
                },
            )
            .await;
        match response {
            Ok(Some(value)) => {
                if report_progress {
                    progress
                        .report(1, None)
                        .map_err(|error| error.into_python())?;
                }
                let result = Python::attach(|py| {
                    value
                        .bind(py)
                        .extract::<String>()
                        .map_err(|_| PyTypeError::new_err("translation response must be a string"))
                });
                Ok(Some((job, result, false)))
            }
            Ok(None) => Ok(None),
            Err(crate::callbacks::CallbackError::Python(error)) => {
                if report_progress {
                    progress
                        .report(1, None)
                        .map_err(|error| error.into_python())?;
                }
                let is_content_filter = Python::attach(|py| -> PyResult<bool> {
                    let class = py
                        .import("babeldoc.babeldoc_exception.BabelDOCException")?
                        .getattr("ContentFilterError")?;
                    Ok(error.is_instance(py, &class))
                })?;
                Ok(Some((job, Err(error), is_content_filter)))
            }
            Err(crate::callbacks::CallbackError::Bridge(message)) => {
                Err(crate::callbacks::BridgeError::new_err(message))
            }
        }
    });
    admitted_rx
}
