use babeldoc_ir_core::{self as ir, Document, ReadOnlyView, TreeView};
use babeldoc_p2_stages::translation::cross_page::{is_continuation, PageFacts, ParagraphFacts, Rect};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use crate::ordinary_translation::{Job, RegexService};
use babeldoc_p2_stages::translation::{cross_page_protocol, ordinary_translation::{InputPlaceholder, PlaceholderRegex, TranslateInput}, token_count::count_tokens};
use std::{collections::HashSet, ops::Range};

pub(crate) type PageKey = (usize, usize);
pub(crate) type OriginalPair = (Job, Job);

fn byte_offset(text: &str, offset: usize) -> Option<usize> {
    if offset == text.chars().count() { Some(text.len()) } else { text.char_indices().nth(offset).map(|(index, _)| index) }
}

pub(crate) fn protected_ranges(regex: &mut RegexService<'_>, input: &TranslateInput, patterns: &[u64]) -> PyResult<Option<Vec<Range<usize>>>> {
    if !cross_page_protocol::validate_response(regex, input, &input.text, patterns)? { return Ok(None); }
    let mut ranges = Vec::new();
    for pattern in patterns {
        for item in regex.find_all(*pattern, &input.text)? {
            let (Some(start), Some(end)) = (byte_offset(&input.text, item.start), byte_offset(&input.text, item.end)) else { return Ok(None) };
            if start >= end || input.text.get(start..end) != Some(item.full.as_str()) { return Ok(None); }
            ranges.push(start..end);
        }
    }
    for placeholder in &input.placeholders {
        if let InputPlaceholder::Rich(rich) = placeholder {
            let left_pattern = regex.compile(&rich.left_pattern, true)?;
            let right_pattern = regex.compile(&rich.right_pattern, true)?;
            let left = regex.find_all(left_pattern, &input.text)?;
            let right = regex.find_all(right_pattern, &input.text)?;
            if left.len() != 1 || left[0].full != rich.left { return Ok(None); }
            let Some(right) = right.iter().find(|item| item.start >= left[0].end && item.full == rich.right) else { return Ok(None); };
            let (Some(start), Some(end)) = (byte_offset(&input.text, left[0].start), byte_offset(&input.text, right.end)) else { return Ok(None) };
            ranges.push(start..end);
        }
    }
    Ok(Some(ranges))
}

pub(crate) fn prepare_pairs(jobs: &mut [Job], pairs: &[(PageKey, PageKey)], regex: &mut RegexService<'_>, patterns: &[u64], min_length: usize) -> Vec<OriginalPair> {
    let mut prepared = Vec::new();
    let mut used = HashSet::new();
    for &(left_key, right_key) in pairs {
        if left_key == right_key || used.contains(&left_key) || used.contains(&right_key) { continue; }
        let Some(left_index) = jobs.iter().position(|job| (job.page, job.paragraph) == left_key) else { continue };
        let Some(right_index) = jobs.iter().position(|job| (job.page, job.paragraph) == right_key) else { continue };
        let left = &jobs[left_index];
        let right = &jobs[right_index];
        if !left.eligible || !right.eligible || left.vertical || right.vertical || left.missing_font_style.is_some() || right.missing_font_style.is_some() { continue; }
        let (Some(left_input), Some(right_input)) = (&left.input, &right.input) else { continue };
        if left_input.text.chars().count() < min_length || right_input.text.chars().count() < min_length { continue; }
        let Ok(Some(ranges)) = protected_ranges(regex, right_input, patterns) else { continue };
        let result = cross_page_protocol::move_sentence_prefix(left_input, right_input, &ranges);
        let Some(result) = result else { continue };
        if result.left.text.chars().count() < min_length || result.right.text.chars().count() < min_length { continue; }
        prepared.push((left.clone(), right.clone()));
        jobs[left_index].token_count = count_tokens(&result.left.text);
        jobs[right_index].token_count = count_tokens(&result.right.text);
        jobs[left_index].input = Some(result.left);
        jobs[right_index].input = Some(result.right);
        used.insert(left_key);
        used.insert(right_key);
    }
    prepared
}

fn access(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn rect(value: &ir::Cursor<ir::types::Box>, view: &TreeView<'_, ReadOnlyView>) -> PyResult<Option<Rect>> {
    let (Some(left), Some(bottom), Some(right), Some(top)) = (
        value.x(view).map_err(access)?, value.y(view).map_err(access)?,
        value.x2(view).map_err(access)?, value.y2(view).map_err(access)?,
    ) else { return Ok(None) };
    if ![left, bottom, right, top].iter().all(|value| value.is_finite()) || left >= right || bottom >= top {
        return Ok(None);
    }
    Ok(Some(Rect { left, bottom, right, top }))
}

struct Paragraph {
    index: usize,
    bounds: Rect,
    text: String,
    simple: bool,
}

struct Page {
    index: usize,
    number: usize,
    selected: bool,
    bounds: Rect,
    paragraphs: Vec<Paragraph>,
    obstacles: Vec<Rect>,
}

fn is_running_header(label: Option<&str>, text: Option<&str>, vertical: bool, bounds: Rect, page: Rect) -> bool {
    let Some(text) = text.map(str::trim) else { return false };
    let height = page.top - page.bottom;
    let width = page.right - page.left;
    label == Some("title")
        && !vertical
        && bounds.bottom >= page.bottom + 0.90 * height
        && bounds.top <= page.top - 0.04 * height
        && bounds.top - bounds.bottom <= 0.025 * height
        && bounds.right - bounds.left <= 0.25 * width
        && (6..=60).contains(&text.chars().count())
        && text.split_whitespace().count() <= 5
        && matches!(text.to_ascii_lowercase().as_str(), "review article" | "research article" | "article" | "review")
}

fn facts(paragraph: &Paragraph, column: usize) -> ParagraphFacts<'_> {
    ParagraphFacts { text: &paragraph.text, bounds: paragraph.bounds, is_body: true, is_boundary_body: true, plain_single_style: paragraph.simple, column }
}

fn boundary(page: &Page, last: bool) -> Option<(usize, usize, usize)> {
    let width = page.bounds.right - page.bounds.left;
    let height = page.bounds.top - page.bounds.bottom;
    let mut body: Vec<_> = page.paragraphs.iter().filter(|paragraph| {
        let b = paragraph.bounds;
        b.left >= page.bounds.left && b.right <= page.bounds.right
            && b.bottom >= page.bounds.bottom + 0.05 * height
            && b.top <= page.bounds.top - 0.05 * height
            && b.right - b.left >= 0.25 * width
            && if last { b.bottom <= page.bounds.bottom + 0.30 * height }
               else { b.top >= page.bounds.bottom + 0.70 * height }
    }).collect();
    if body.is_empty() { return None; }
    body.sort_by(|left, right| left.bounds.left.total_cmp(&right.bounds.left));
    let min_left = body.first()?.bounds.left;
    let max_left = body.last()?.bounds.left;
    let columns = if max_left - min_left <= 0.08 * width { 1 } else { 2 };
    let split = (min_left + max_left) / 2.0;
    let mut left_column = Vec::new();
    let mut right_column = Vec::new();
    for paragraph in body {
        if columns == 1 || paragraph.bounds.left < split {
            left_column.push(paragraph);
        } else {
            right_column.push(paragraph);
        }
    }
    if columns == 2 {
        if left_column.is_empty() || right_column.is_empty() { return None; }
        let left_edge = left_column.iter().map(|p| p.bounds.left).fold(f64::INFINITY, f64::min);
        let right_edge = right_column.iter().map(|p| p.bounds.left).fold(f64::INFINITY, f64::min);
        if left_column.iter().any(|p| (p.bounds.left - left_edge).abs() > 0.08 * width)
            || right_column.iter().any(|p| (p.bounds.left - right_edge).abs() > 0.08 * width)
            || left_column.iter().any(|p| p.bounds.right > right_edge) { return None; }
    }
    for column in [&left_column, &right_column] {
        for (index, first) in column.iter().enumerate() {
            if column.iter().skip(index + 1).any(|second| first.bounds.bottom < second.bounds.top && second.bounds.bottom < first.bounds.top) { return None; }
        }
    }
    let column = if last && columns == 2 { &right_column } else { &left_column };
    let chosen = if last {
        column.iter().min_by(|a, b| a.bounds.bottom.total_cmp(&b.bounds.bottom))?
    } else {
        column.iter().max_by(|a, b| a.bounds.top.total_cmp(&b.bounds.top))?
    };
    let excluded_body = page.paragraphs.iter().filter_map(|paragraph| {
        let bounds = paragraph.bounds;
        (bounds.left >= page.bounds.left && bounds.right <= page.bounds.right
            && bounds.bottom >= page.bounds.bottom + 0.05 * height
            && bounds.top <= page.bounds.top - 0.05 * height
            && bounds.right - bounds.left < 0.25 * width).then_some(&paragraph.bounds)
    });
    if page.obstacles.iter().chain(excluded_body).any(|obstacle| {
        let overlaps_column = obstacle.left < chosen.bounds.right && obstacle.right > chosen.bounds.left;
        let in_boundary = if last {
            obstacle.bottom < chosen.bounds.bottom && obstacle.top > page.bounds.bottom + 0.05 * height
        } else {
            obstacle.top > chosen.bounds.top && obstacle.bottom < page.bounds.top - 0.05 * height
        };
        overlaps_column && in_boundary
    }) { return None; }
    Some((chosen.index, columns, if last { columns - 1 } else { 0 }))
}

fn boundary_column(page: &Page, last: bool, anchor: Rect) -> Option<usize> {
    let height = page.bounds.top - page.bounds.bottom;
    let width = page.bounds.right - page.bounds.left;
    let tolerance = 0.025 * width;
    let body = page.paragraphs.iter().filter(|paragraph| {
        let bounds = paragraph.bounds;
        bounds.left >= page.bounds.left && bounds.right <= page.bounds.right
            && bounds.bottom >= page.bounds.bottom + 0.05 * height
            && bounds.top <= page.bounds.top - 0.05 * height
            && (bounds.left - anchor.left).abs() <= tolerance
            && (bounds.right - anchor.right).abs() <= tolerance
    }).collect::<Vec<_>>();
    let chosen = if last {
        body.iter().min_by(|a, b| a.bounds.bottom.total_cmp(&b.bounds.bottom))?
    } else {
        body.iter().max_by(|a, b| a.bounds.top.total_cmp(&b.bounds.top))?
    };
    if last && chosen.bounds.bottom > page.bounds.bottom + 0.30 * height
        || !last && chosen.bounds.top < page.bounds.bottom + 0.70 * height {
        return None;
    }
    let obstacles = page.obstacles.iter().copied().chain(
        page.paragraphs.iter().filter(|paragraph| paragraph.index != chosen.index).map(|paragraph| paragraph.bounds)
    );
    if obstacles.into_iter().any(|obstacle| {
        let horizontal = obstacle.left < anchor.right && obstacle.right > anchor.left;
        let vertical = if last {
            obstacle.bottom < chosen.bounds.bottom && obstacle.top > page.bounds.bottom + 0.05 * height
        } else {
            obstacle.top > chosen.bounds.top && obstacle.bottom < page.bounds.top - 0.05 * height
        };
        horizontal && vertical
    }) { return None; }
    Some(chosen.index)
}

fn paired_boundaries(left: &Page, right: &Page) -> Option<(usize, usize)> {
    let left_width = left.bounds.right - left.bounds.left;
    let right_width = right.bounds.right - right.bounds.left;
    let left_height = left.bounds.top - left.bounds.bottom;
    let right_height = right.bounds.top - right.bounds.bottom;
    let mut candidates = Vec::new();
    for previous in &left.paragraphs {
        if previous.bounds.bottom > left.bounds.bottom + 0.30 * left_height { continue; }
        for following in &right.paragraphs {
            if following.bounds.top < right.bounds.bottom + 0.70 * right_height { continue; }
            let left_facts = PageFacts { number: left.number, selected: left.selected, bounds: left.bounds, reading_order_unambiguous: true, body_columns: 2 };
            let right_facts = PageFacts { number: right.number, selected: right.selected, bounds: right.bounds, reading_order_unambiguous: true, body_columns: 2 };
            if !is_continuation(left_facts, facts(previous, 1), right_facts, facts(following, 0)) { continue; }
            let match_column = |anchor: Rect, other: Rect| {
                ((anchor.left - left.bounds.left) / left_width - (other.left - right.bounds.left) / right_width).abs() <= 0.025
                    && ((anchor.right - left.bounds.left) / left_width - (other.right - right.bounds.left) / right_width).abs() <= 0.025
            };
            let left_anchor = left.paragraphs.iter().any(|paragraph| {
                paragraph.index != previous.index && paragraph.bounds.right <= previous.bounds.left
                    && match_column(paragraph.bounds, following.bounds)
            });
            let right_anchor = right.paragraphs.iter().any(|paragraph| {
                paragraph.index != following.index && paragraph.bounds.left >= following.bounds.right
                    && match_column(previous.bounds, paragraph.bounds)
            });
            if !left_anchor || !right_anchor { continue; }
            if boundary_column(left, true, previous.bounds) != Some(previous.index)
                || boundary_column(right, false, following.bounds) != Some(following.index) { continue; }
            candidates.push((previous.index, following.index));
        }
    }
    if candidates.len() == 1 { candidates.pop() } else { None }
}

pub(crate) fn find_pairs(document: &mut Document, config: &Bound<'_, PyAny>) -> PyResult<Vec<((usize, usize), (usize, usize))>> {
    let view = document.view::<ReadOnlyView>();
    let mut pages = Vec::new();
    for (index, page) in view.document().page_iter(&view).map_err(access)?.enumerate() {
        let page = page.map_err(access)?;
        let Some(number) = page.page_number(&view).map_err(access)?.and_then(|number| usize::try_from(number).ok()) else { continue };
        let Some(crop) = page.cropbox(&view).map_err(access)? else { continue };
        let Some(crop_box) = crop.r#box(&view).map_err(access)? else { continue };
        let Some(page_bounds) = rect(&crop_box, &view)? else { continue };
        let selected = config.call_method1("should_translate_page", (number + 1,))?.is_truthy()?;
        let mut paragraphs = Vec::new();
        let mut obstacles = Vec::new();
        for (paragraph_index, paragraph) in page.pdf_paragraph_iter(&view).map_err(access)?.enumerate() {
            let paragraph = paragraph.map_err(access)?;
            let Some(paragraph_box) = paragraph.r#box(&view).map_err(access)? else { continue };
            let Some(bounds) = rect(&paragraph_box, &view)? else { continue };
            let mut has_composition = false;
            let mut debug_only = true;
            for composition in paragraph.pdf_paragraph_composition_iter(&view).map_err(access)? {
                let composition = composition.map_err(access)?;
                has_composition = true;
                if !composition.pdf_same_style_unicode_characters(&view).map_err(access)?
                    .is_some_and(|characters| characters.debug_info(&view).ok().flatten() == Some(true)) {
                    debug_only = false;
                    break;
                }
            }
            if has_composition && debug_only { continue; }
            let label = paragraph.layout_label(&view).map_err(access)?;
            if !matches!(label.as_deref(), Some("text" | "plain text" | "paragraph_hybrid")) {
                let text = paragraph.unicode(&view).map_err(access)?;
                let vertical = paragraph.vertical(&view).map_err(access)?.unwrap_or(false);
                if is_running_header(label.as_deref(), text.as_deref(), vertical, bounds, page_bounds) { continue; }
                obstacles.push(bounds);
                continue;
            }
            let Some(text) = paragraph.unicode(&view).map_err(access)?.clone() else { obstacles.push(bounds); continue };
            let mut simple = !paragraph.vertical(&view).map_err(access)?.unwrap_or(false);
            for composition in paragraph.pdf_paragraph_composition_iter(&view).map_err(access)? {
                let composition = composition.map_err(access)?;
                if let Some(line) = composition.pdf_line(&view).map_err(access)? {
                    let Some(line_box) = line.r#box(&view).map_err(access)? else { simple = false; continue };
                    if rect(&line_box, &view)?.is_none() { simple = false; }
                }
            }
            paragraphs.push(Paragraph { index: paragraph_index, bounds, text, simple });
        }
        pages.push(Page { index, number, selected, bounds: page_bounds, paragraphs, obstacles });
    }
    let mut pairs = Vec::new();
    for pair in pages.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        if left.index.checked_add(1) != Some(right.index) { continue; }
        if let Some((left_index, right_index)) = paired_boundaries(left, right) {
            pairs.push(((left.index, left_index), (right.index, right_index)));
            continue;
        }
        let (Some((left_index, left_columns, left_column)), Some((right_index, right_columns, right_column))) = (boundary(left, true), boundary(right, false)) else { continue };
        let left_paragraph = &left.paragraphs.iter().find(|p| p.index == left_index).unwrap();
        let right_paragraph = &right.paragraphs.iter().find(|p| p.index == right_index).unwrap();
        let page_facts = |page: &Page, columns| PageFacts { number: page.number, selected: page.selected, bounds: page.bounds, reading_order_unambiguous: true, body_columns: columns };
        if is_continuation(page_facts(left, left_columns), facts(left_paragraph, left_column), page_facts(right, right_columns), facts(right_paragraph, right_column)) {
            pairs.push(((left.index, left_index), (right.index, right_index)));
        }
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(index: usize, bounds: Rect) -> Paragraph {
        Paragraph { index, bounds, text: "body text".into(), simple: true }
    }

    fn page(paragraphs: Vec<Paragraph>) -> Page {
        Page {
            index: 0, number: 0, selected: true,
            bounds: Rect { left: 0.0, bottom: 0.0, right: 600.0, top: 800.0 },
            paragraphs, obstacles: vec![],
        }
    }

    #[test]
    fn ignores_only_narrow_running_headers_in_page_margin() {
        let page = page(vec![]).bounds;
        let header = Rect { left: 41.0, bottom: 736.0, right: 152.0, top: 753.0 };
        assert!(is_running_header(Some("title"), Some("Review article"), false, header, page));
        assert!(!is_running_header(Some("title"), Some("Clinical review of the current article"), false, header, page));
        assert!(!is_running_header(Some("title"), Some("Review article"), true, header, page));
        assert!(!is_running_header(Some("plain text"), Some("Review article"), false, header, page));
        assert!(!is_running_header(Some("title"), Some("Methods"), false, header, page));
        assert!(!is_running_header(Some("title"), Some("Review article"), false, Rect { bottom: 650.0, top: 667.0, ..header }, page));
        assert!(!is_running_header(Some("title"), Some("Review article"), false, Rect { right: 500.0, ..header }, page));
    }

    #[test]
    fn narrow_body_blocks_boundary_but_footer_does_not() {
        let main = Rect { left: 60.0, bottom: 120.0, right: 540.0, top: 150.0 };
        let narrow = Rect { left: 60.0, bottom: 70.0, right: 180.0, top: 90.0 };
        assert!(boundary(&page(vec![paragraph(0, main), paragraph(1, narrow)]), true).is_none());
        let footer = Rect { bottom: 10.0, top: 30.0, ..narrow };
        assert_eq!(boundary(&page(vec![paragraph(0, main), paragraph(1, footer)]), true), Some((0, 1, 0)));
    }

    #[test]
    fn narrow_body_above_first_paragraph_blocks_boundary() {
        let main = Rect { left: 60.0, bottom: 650.0, right: 540.0, top: 700.0 };
        let narrow = Rect { left: 60.0, bottom: 715.0, right: 180.0, top: 735.0 };
        assert!(boundary(&page(vec![paragraph(0, main), paragraph(1, narrow)]), false).is_none());
    }

    #[test]
    fn distant_columns_do_not_change_a_boundary() {
        let last_column = Rect { left: 353.0, bottom: 47.0, right: 553.0, top: 321.0 };
        let boxed_text = Rect { left: 48.0, bottom: 53.0, right: 337.0, top: 284.0 };
        let top_column = Rect { left: 142.0, bottom: 623.0, right: 345.0, top: 731.0 };
        let references = Rect { left: 217.0, bottom: 48.0, right: 380.0, top: 501.0 };
        assert_eq!(boundary(&page(vec![paragraph(0, last_column), paragraph(1, boxed_text)]), true), Some((0, 2, 1)));
        assert_eq!(boundary(&page(vec![paragraph(0, top_column), paragraph(1, references)]), false), Some((0, 1, 0)));
    }

    #[test]
    fn finds_two_column_boundaries_despite_other_regions() {
        let mut left = page(vec![
            paragraph(0, Rect { left: 142.0, bottom: 440.0, right: 344.0, top: 728.0 }),
            paragraph(1, Rect { left: 353.0, bottom: 47.0, right: 553.0, top: 321.0 }),
            paragraph(2, Rect { left: 48.0, bottom: 53.0, right: 337.0, top: 284.0 }),
        ]);
        left.paragraphs[1].text = "A sentence continues across".into();
        let mut right = page(vec![
            paragraph(0, Rect { left: 142.0, bottom: 623.0, right: 345.0, top: 731.0 }),
            paragraph(1, Rect { left: 353.0, bottom: 543.0, right: 556.0, top: 731.0 }),
            paragraph(2, Rect { left: 217.0, bottom: 48.0, right: 380.0, top: 501.0 }),
        ]);
        right.number = 1;
        right.paragraphs[0].text = "the page boundary. Another sentence.".into();
        assert_eq!(paired_boundaries(&left, &right), Some((1, 0)));
        left.paragraphs.push(paragraph(3, Rect { left: 360.0, bottom: 42.0, right: 470.0, top: 45.0 }));
        assert_eq!(paired_boundaries(&left, &right), None);
    }
}
