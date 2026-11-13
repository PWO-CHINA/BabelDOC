use babeldoc_ir_core::{self as ir, Document, ReadOnlyView, TreeView};
use pyo3::{exceptions::PyRuntimeError, prelude::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Bounds {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub page: usize,
    pub paragraph: usize,
    pub text: String,
    pub label: Option<String>,
    pub bounds: Bounds,
    pub font_size: Option<f64>,
    pub vertical: bool,
    pub heading_barrier: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_region: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PageContext {
    pub page: usize,
    pub paragraphs: Vec<Candidate>,
    pub figures: Vec<Bounds>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BoundaryKind { Column, Page }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Boundary {
    pub kind: BoundaryKind,
    pub left: Vec<Candidate>,
    pub right: Vec<Candidate>,
    pub context: Vec<PageContext>,
}

pub(crate) fn known_non_body(candidate: &Candidate) -> bool {
    if marginal_hint(candidate) && !body_label(candidate.label.as_deref()) {
        return true;
    }
    if matches!(candidate.label.as_deref(), Some("reference" | "reference_hybrid" | "reference_content")) {
        return true;
    }
    matches!(candidate.label.as_deref(), Some("abandon"))
        && (candidate.bounds.bottom < 0.08 || candidate.bounds.top > 0.92)
}

pub(crate) fn non_body_label(candidate: &Candidate) -> bool {
    known_non_body(candidate) || matches!(candidate.label.as_deref(), Some(
        "caption" | "figure_caption" | "figure_label" | "figure_text" | "axis_label"
            | "table_caption" | "table" | "header" | "footer" | "footnote" | "page_number"
    ))
}

fn mark_running_headers(pages: &mut [Page]) {
    let mut running = HashSet::new();
    for page in pages.iter() {
        for candidate in &page.context.paragraphs {
            if candidate.bounds.bottom < 0.9 || candidate.text.trim().is_empty() { continue; }
            if pages.iter().filter(|other| other.index != page.index).any(|other| {
                other.context.paragraphs.iter().any(|item| {
                    item.text.trim() == candidate.text.trim() && item.bounds.bottom >= 0.9
                        && (item.bounds.left - candidate.bounds.left).abs() < 0.02
                        && (item.bounds.top - candidate.bounds.top).abs() < 0.02
                        && item.font_size.zip(candidate.font_size).is_some_and(|(left, right)| (left - right).abs() < 0.5)
                })
            }) { running.insert((candidate.page, candidate.paragraph)); }
        }
    }
    for page in pages {
        for candidate in &mut page.context.paragraphs {
            if running.contains(&(candidate.page, candidate.paragraph)) {
                candidate.label = Some("page_header".into());
                candidate.heading_barrier = false;
            }
        }
    }
}

fn reference_entries(text: &str) -> usize {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    tokens.windows(3).filter(|pair| {
        let number = pair[0].trim_matches(['[', ']']).trim_end_matches('.');
        let initials = pair[2].trim_end_matches(',');
        !number.is_empty() && number.len() <= 3 && number.chars().all(|character| character.is_ascii_digit())
            && (pair[0].ends_with('.') || pair[0].ends_with(']'))
            && pair[1].ends_with(',') && pair[1].starts_with(char::is_uppercase)
            && initials.ends_with('.') && initials.len() <= 8
            && initials.chars().any(char::is_uppercase)
            && initials.chars().all(|character| character.is_uppercase() || matches!(character, '.' | '-'))
    }).count()
}

fn mark_reference_regions(pages: &mut [Page]) {
    for page in pages {
        let body_size = page.context.paragraphs.iter().filter(|item| body_label(item.label.as_deref()))
            .filter_map(|item| item.font_size).fold(0.0_f64, f64::max);
        for candidate in &mut page.context.paragraphs {
            let entries = reference_entries(&candidate.text);
            let years = candidate.text.split(|character: char| !character.is_ascii_digit())
                .filter(|word| word.len() == 4 && word.parse::<u16>().is_ok_and(|year| (1800..=2100).contains(&year))).count();
            if !candidate.heading_barrier && entries >= 3 && years >= 3
                && candidate.font_size.is_some_and(|size| size < body_size * 0.85)
            {
                candidate.label = Some("reference".into());
            }
        }
    }
}

fn adjacent_line_fragments(left: &Candidate, right: &Candidate) -> bool {
    if left.page != right.page { return false; }
    let height = (left.bounds.top - left.bounds.bottom).max(right.bounds.top - right.bounds.bottom);
    let overlap_y = left.bounds.top.min(right.bounds.top) - left.bounds.bottom.max(right.bounds.bottom);
    let gap = right.bounds.left - left.bounds.right;
    overlap_y >= height * 0.5 && gap >= -height * 0.2 && gap <= height * 1.5
        && (left.bounds.right - left.bounds.left).max(right.bounds.right - right.bounds.left) < 0.15
}

pub(crate) fn focus(mut boundary: Boundary) -> Option<Boundary> {
    let usable = |candidate: &Candidate| !candidate.heading_barrier && !known_non_body(candidate)
        && !candidate.text.trim().is_empty();
    let body = |candidate: &Candidate| usable(candidate) && body_label(candidate.label.as_deref());
    let left_anchor = boundary.left.iter().rposition(body).or_else(|| boundary.left.iter().rposition(usable))?;
    let right_anchor = boundary.right.iter().position(body).or_else(|| boundary.right.iter().position(usable))?;
    let mut left_start = boundary.left[..left_anchor].iter().rposition(body)
        .or_else(|| boundary.left[..left_anchor].iter().rposition(usable)).unwrap_or(left_anchor);
    while left_start > 0
        && babeldoc_p2_stages::translation::cross_boundary::sentences::sentence_ends(&boundary.left[left_start].text).is_empty()
    {
        let previous = &boundary.left[left_start - 1];
        if previous.heading_barrier { break; }
        if known_non_body(previous) { left_start -= 1; continue; }
        if babeldoc_p2_stages::translation::cross_boundary::sentences::sentence_ends(&previous.text).last().copied()
            == Some(previous.text.trim_end().len()) { break; }
        left_start -= 1;
    }
    let mut right_end = boundary.right[right_anchor + 1..].iter().position(body)
        .or_else(|| boundary.right[right_anchor + 1..].iter().position(usable))
        .map_or(right_anchor + 1, |offset| right_anchor + offset + 2);
    while right_end < boundary.right.len()
        && babeldoc_p2_stages::translation::cross_boundary::sentences::sentence_ends(&boundary.right[right_end - 1].text).is_empty()
    {
        if boundary.right[right_end].heading_barrier { break; }
        right_end += 1;
    }
    boundary.left = boundary.left[left_start..].to_vec();
    boundary.right.truncate(right_end);
    let locations = boundary.left.iter().chain(&boundary.right)
        .map(|candidate| (candidate.page, candidate.paragraph)).collect::<HashSet<_>>();
    for page in &mut boundary.context {
        page.paragraphs.retain(|candidate| locations.contains(&(candidate.page, candidate.paragraph))
            || matches!(candidate.label.as_deref(), Some("caption" | "figure_caption" | "table_caption")));
    }
    Some(boundary)
}

fn related_caption(page: &PageContext, candidate: &Candidate) -> bool {
    page.paragraphs.iter().any(|item| {
        matches!(item.label.as_deref(), Some("caption" | "figure_caption" | "table_caption"))
            && candidate.font_size.zip(item.font_size).is_some_and(|(first, second)| (first - second).abs() < 0.6)
            && ((candidate.bounds.top - item.bounds.top).abs() < 0.02
                && (candidate.bounds.bottom - item.bounds.bottom).abs() < 0.02
                || candidate.label.as_deref() == Some("fallback_line")
                    && candidate.bounds.bottom >= item.bounds.top
                    && candidate.bounds.top - candidate.bounds.bottom < 0.025)
    })
}

pub(crate) fn selectable(boundary: &Boundary, candidate: &Candidate) -> bool {
    if candidate.heading_barrier || can_skip(boundary, candidate) { return false; }
    let Some(page) = boundary.context.iter().find(|page| page.page == candidate.page) else { return false; };
    let body_size = page.paragraphs.iter().filter(|item| body_label(item.label.as_deref()))
        .filter_map(|item| item.font_size).fold(0.0_f64, f64::max);
    if candidate.font_size.is_some_and(|size| size < body_size * 0.9) || related_caption(page, candidate) {
        return false;
    }
    if boundary.right.iter().any(|item| item.page == candidate.page && item.paragraph == candidate.paragraph) {
        let text = candidate.text.trim_start();
        let digits = text.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && text[digits..].strip_prefix('.').is_some_and(|rest| rest.starts_with(char::is_whitespace)) {
            return false;
        }
    }
    true
}

pub(crate) fn can_skip(boundary: &Boundary, candidate: &Candidate) -> bool {
    if candidate.heading_barrier { return false; }
    if non_body_label(candidate) { return true; }
    let lower = candidate.text.trim_start().to_ascii_lowercase();
    if ["fig. ", "figure ", "table "].iter().any(|prefix| lower.strip_prefix(prefix).is_some_and(|rest| {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with(['.', ':', '|'])
            && rest[digits + 1..].starts_with(char::is_whitespace)
    })) { return true; }
    let Some(page) = boundary.context.iter().find(|page| page.page == candidate.page) else { return false; };
    if matches!(candidate.label.as_deref(), Some("fallback_line" | "abandon"))
        && (candidate.bounds.bottom < 0.06 || candidate.bounds.top > 0.94)
        && candidate.text.chars().count() <= 12 { return true; }
    let inside_figure = page.figures.iter().any(|figure| {
        candidate.bounds.left >= figure.left && candidate.bounds.right <= figure.right
            && candidate.bounds.bottom >= figure.bottom && candidate.bounds.top <= figure.top
    });
    if inside_figure { return true; }
    let body_size = page.paragraphs.iter().filter(|item| body_label(item.label.as_deref()))
        .filter_map(|item| item.font_size).fold(0.0_f64, f64::max);
    let small = candidate.font_size.is_some_and(|size| size < body_size * 0.9);
    small && (!body_label(candidate.label.as_deref()) && related_caption(page, candidate) || page.figures.iter().any(|figure| {
            overlap(candidate.bounds, *figure) && candidate.bounds.top <= figure.bottom + 0.03
        }))
}

struct Page {
    index: usize,
    number: usize,
    selected: bool,
    context: PageContext,
}

fn access(error: impl std::fmt::Display) -> PyErr { PyRuntimeError::new_err(error.to_string()) }

fn bounds(value: &ir::Cursor<ir::types::Box>, view: &TreeView<'_, ReadOnlyView>) -> PyResult<Option<Bounds>> {
    let (Some(left), Some(bottom), Some(right), Some(top)) =
        (value.x(view).map_err(access)?, value.y(view).map_err(access)?,
         value.x2(view).map_err(access)?, value.y2(view).map_err(access)?) else { return Ok(None) };
    Ok(([left, bottom, right, top].iter().all(|value| value.is_finite()) && left < right && bottom < top)
        .then_some(Bounds { left, bottom, right, top }))
}

fn relative(item: Bounds, page: Bounds) -> Bounds {
    let width = page.right - page.left;
    let height = page.top - page.bottom;
    Bounds { left: (item.left - page.left) / width, right: (item.right - page.left) / width,
        bottom: (item.bottom - page.bottom) / height, top: (item.top - page.bottom) / height }
}

fn body_label(label: Option<&str>) -> bool {
    matches!(label, Some("text" | "plain text" | "paragraph" | "paragraph_hybrid" | "content" | "abstract" | "list_item" | "list_item_hybrid"))
}

fn heading_label(label: Option<&str>) -> bool {
    matches!(label, Some("title" | "paragraph_title" | "doc_title" | "title_hybrid" | "section_header" | "heading" | "chapter_title"))
}

fn marginal_hint(item: &Candidate) -> bool {
    if matches!(item.label.as_deref(), Some("number" | "header" | "footer" | "page_header" | "page_footer" | "page_header_hybrid" | "page_footer_hybrid" | "page_number_hybrid" | "line_number_hybrid")) {
        return true;
    }
    let text = item.text.trim();
    item.bounds.right - item.bounds.left <= 0.15 && !text.is_empty() && text.chars().count() <= 12
        && text.chars().any(|character| character.is_numeric())
        && text.chars().all(|character| character.is_numeric() || character.is_whitespace() || "-–—.()[]".contains(character))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegionKind { Column, Spanning, Heading }

#[derive(Clone)]
struct Region {
    items: Vec<Candidate>,
    band: usize,
    kind: RegionKind,
}

struct PageLayout {
    regions: Vec<Region>,
    alternate_regions: Option<Vec<Region>>,
    alternatives: Vec<(Vec<Candidate>, Vec<Candidate>)>,
}

struct ColumnGroup<'a> {
    items: Vec<&'a Candidate>,
    center: f64,
    width: f64,
    body: bool,
}

fn overlap(first: Bounds, second: Bounds) -> bool {
    let overlap = (first.right.min(second.right) - first.left.max(second.left)).max(0.0);
    overlap >= 0.5 * (first.right - first.left).min(second.right - second.left)
}

fn sort_reading(items: &mut [Candidate]) {
    items.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top).then(a.paragraph.cmp(&b.paragraph)));
}

fn separated_extremes<'a>(items: &[&'a Candidate]) -> Option<(&'a Candidate, &'a Candidate)> {
    let left = *items.iter().min_by(|a, b| a.bounds.right.total_cmp(&b.bounds.right))?;
    let right = *items.iter().max_by(|a, b| a.bounds.left.total_cmp(&b.bounds.left))?;
    (left.bounds.right + 0.02 <= right.bounds.left).then_some((left, right))
}

fn columns(page: &Page) -> PageLayout {
    let items: Vec<_> = page.context.paragraphs.iter().filter(|item| !item.vertical).collect();
    let mut spanning = HashSet::new();
    let mut separators = Vec::new();
    for item in &items {
        let contained: Vec<_> = items.iter().copied().filter(|other| {
            let other_center = (other.bounds.left + other.bounds.right) / 2.0;
            other.paragraph != item.paragraph && !marginal_hint(other)
                && other.bounds.right - other.bounds.left < item.bounds.right - item.bounds.left
                && item.bounds.left <= other_center && item.bounds.right >= other_center
        }).collect();
        let has_body = contained.iter().any(|other| body_label(other.label.as_deref()));
        if (!item.heading_barrier && !has_body) || separated_extremes(&contained).is_none() { continue; }
        spanning.insert(item.paragraph);
        if item.heading_barrier { separators.push(*item); }
    }
    let mut anchors: Vec<_> = items.iter().copied()
        .filter(|item| !spanning.contains(&item.paragraph) && !marginal_hint(item)
            && !item.heading_barrier && body_label(item.label.as_deref()))
        .collect();
    for item in items.iter().copied().filter(|item| !spanning.contains(&item.paragraph)
        && !marginal_hint(item) && !item.heading_barrier) {
        let overlaps = anchors.iter().any(|anchor| overlap(anchor.bounds, item.bounds));
        if !overlaps { anchors.push(item); }
    }
    anchors.sort_by(|a, b| a.bounds.left.total_cmp(&b.bounds.left));
    let mut groups: Vec<Vec<&Candidate>> = Vec::new();
    for item in anchors {
        if let Some(group) = groups.iter_mut().find(|group| overlap(group[0].bounds, item.bounds)) {
            group.push(item);
        } else { groups.push(vec![item]); }
    }
    let groups: Vec<_> = groups.into_iter().map(|items| ColumnGroup {
        center: items.iter().map(|item| (item.bounds.left + item.bounds.right) / 2.0).sum::<f64>() / items.len() as f64,
        width: items.iter().map(|item| item.bounds.right - item.bounds.left).fold(0.0_f64, f64::max),
        body: items.iter().any(|item| body_label(item.label.as_deref())),
        items,
    }).collect();
    let widest_body = groups.iter().filter(|group| group.body).map(|group| group.width).fold(0.0_f64, f64::max);
    let substantial = groups.iter().filter(|group| group.body && group.width >= 0.5 * widest_body).count();
    let active: Vec<_> = groups.iter().filter(|group| substantial < 2 || group.body && group.width >= 0.5 * widest_body).collect();
    let extra: Vec<_> = groups.iter().filter(|group| !active.iter().any(|active| std::ptr::eq(*active, *group))).collect();
    let extra_locations: HashMap<_, _> = extra.iter().enumerate().flat_map(|(index, group)|
        group.items.iter().map(move |item| (item.paragraph, index))).collect();
    if active.is_empty() {
        let mut fallback: Vec<_> = items.into_iter().cloned().collect();
        sort_reading(&mut fallback);
        for item in &mut fallback { item.reading_region = Some(0); }
        return PageLayout { regions: if fallback.is_empty() { vec![] } else {
            vec![Region { items: fallback, band: 0, kind: RegionKind::Column }]
        }, alternate_regions: None, alternatives: vec![] };
    }
    separators.sort_by(|a, b| b.bounds.top.total_cmp(&a.bounds.top).then(a.paragraph.cmp(&b.paragraph)));
    let separator_ids: HashSet<_> = separators.iter().map(|item| item.paragraph).collect();
    let mut extents = vec![None::<(f64, f64)>; separators.len() + 1];
    for item in active.iter().flat_map(|group| &group.items) {
        let band = separators.partition_point(|separator| separator.bounds.top >= item.bounds.top);
        extents[band] = Some(match extents[band] {
            Some((highest, lowest)) => (highest.max(item.bounds.top), lowest.min(item.bounds.top)),
            None => (item.bounds.top, item.bounds.top),
        });
    }
    let mut bands = vec![vec![Vec::new(); active.len()]; separators.len() + 1];
    let mut above = vec![Vec::new(); bands.len()];
    let mut below = vec![Vec::new(); bands.len()];
    for item in page.context.paragraphs.iter().filter(|item|
        !item.vertical && !separator_ids.contains(&item.paragraph)) {
        let center = (item.bounds.left + item.bounds.right) / 2.0;
        let band = separators.partition_point(|separator| separator.bounds.top >= item.bounds.top);
        if spanning.contains(&item.paragraph) {
            if let Some((highest, lowest)) = extents[band] {
                if item.bounds.top > highest { above[band].push(item.clone()); continue; }
                if item.bounds.top < lowest { below[band].push(item.clone()); continue; }
            }
        }
        let (index, _) = active.iter().enumerate().min_by(|(_, a), (_, b)|
            (center - a.center).abs().total_cmp(&(center - b.center).abs())).expect("active column");
        bands[band][index].push(item.clone());
    }
    let mut result = Vec::new();
    let mut alternate_result = Vec::new();
    let mut has_alternate = false;
    let mut alternatives = Vec::new();
    for (band, columns) in bands.into_iter().enumerate() {
        sort_reading(&mut above[band]);
        sort_reading(&mut below[band]);
        for item in &above[band] {
            let region = Region { items: vec![item.clone()], band, kind: RegionKind::Spanning };
            result.push(region.clone());
            alternate_result.push(region);
        }
        let mut extra_in_band = BTreeMap::<usize, Vec<Candidate>>::new();
        for item in columns.iter().flatten() {
            if let Some(index) = extra_locations.get(&item.paragraph) {
                extra_in_band.entry(*index).or_default().push(item.clone());
            }
        }
        for (index, mut items) in extra_in_band {
            sort_reading(&mut items);
            let item_ids: HashSet<_> = items.iter().map(|item| item.paragraph).collect();
            let center = extra[index].center;
            let left = active.iter().enumerate().filter(|(_, group)| group.center < center)
                .max_by(|(_, a), (_, b)| a.center.total_cmp(&b.center));
            let right = active.iter().enumerate().filter(|(_, group)| group.center > center)
                .min_by(|(_, a), (_, b)| a.center.total_cmp(&b.center));
            if let Some((column, _)) = left {
                let mut preceding: Vec<_> = columns[column].iter().filter(|item| !item_ids.contains(&item.paragraph)).cloned().collect();
                sort_reading(&mut preceding);
                if !preceding.is_empty() { alternatives.push((preceding, items.clone())); }
            }
            if let Some((column, _)) = right {
                let mut following: Vec<_> = columns[column].iter().filter(|item| !item_ids.contains(&item.paragraph)).cloned().collect();
                sort_reading(&mut following);
                if !following.is_empty() { alternatives.push((items, following)); }
            }
        }
        let mut middle: Vec<_> = columns.iter().flatten().filter(|item| spanning.contains(&item.paragraph)).cloned().collect();
        sort_reading(&mut middle);
        if !middle.is_empty() {
            has_alternate = true;
            let mut segments = vec![vec![Vec::new(); active.len()]; middle.len() + 1];
            for (column, items) in columns.iter().enumerate() {
                for item in items.iter().filter(|item| !spanning.contains(&item.paragraph)) {
                    let segment = middle.partition_point(|span| span.bounds.top >= item.bounds.top);
                    segments[segment][column].push(item.clone());
                }
            }
            let mut alternate_regions = Vec::new();
            for (segment, columns) in segments.into_iter().enumerate() {
                for mut items in columns {
                    sort_reading(&mut items);
                    if !items.is_empty() { alternate_regions.push(items); }
                }
                if let Some(span) = middle.get(segment) { alternate_regions.push(vec![span.clone()]); }
            }
            alternatives.extend(alternate_regions.windows(2).map(|pair| (pair[0].clone(), pair[1].clone())));
            alternate_result.extend(alternate_regions.into_iter().map(|items| Region { items, band, kind: RegionKind::Column }));
        } else {
            for mut items in columns.iter().cloned() {
                sort_reading(&mut items);
                if !items.is_empty() { alternate_result.push(Region { items, band, kind: RegionKind::Column }); }
            }
        }
        for mut items in columns {
            sort_reading(&mut items);
            if !items.is_empty() { result.push(Region { items, band, kind: RegionKind::Column }); }
        }
        for item in &below[band] {
            let region = Region { items: vec![item.clone()], band, kind: RegionKind::Spanning };
            result.push(region.clone());
            alternate_result.push(region);
        }
        if let Some(separator) = separators.get(band) {
            let region = Region { items: vec![(*separator).clone()], band, kind: RegionKind::Heading };
            result.push(region.clone());
            alternate_result.push(region);
        }
    }
    for (index, region) in result.iter_mut().enumerate() {
        for item in &mut region.items { item.reading_region = Some(index); }
    }
    for (index, region) in alternate_result.iter_mut().enumerate() {
        for item in &mut region.items { item.reading_region = Some(index); }
    }
    PageLayout { regions: result, alternate_regions: has_alternate.then_some(alternate_result), alternatives }
}

fn collect_pages(pages: &[Page]) -> Vec<Boundary> {
    let layouts: Vec<_> = pages.iter().map(columns).collect();
    let mut result = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        if !page.selected { continue; }
        let mut seen = HashSet::new();
        for pair in layouts[index].regions.windows(2) {
            if pair[0].kind != RegionKind::Heading && pair[1].kind != RegionKind::Heading && pair[0].band == pair[1].band {
                let left = pair[0].items.clone();
                let right = pair[1].items.clone();
                seen.insert((left.iter().map(|item| item.paragraph).collect::<Vec<_>>(), right.iter().map(|item| item.paragraph).collect::<Vec<_>>()));
                result.push(Boundary { kind: BoundaryKind::Column, left, right, context: vec![page.context.clone()] });
            }
        }
        for (left, right) in &layouts[index].alternatives {
            let key = (left.iter().map(|item| item.paragraph).collect::<Vec<_>>(), right.iter().map(|item| item.paragraph).collect::<Vec<_>>());
            if seen.insert(key) {
                let mut left = left.clone();
                let mut right = right.clone();
                for item in &mut left { item.reading_region = Some(0); }
                for item in &mut right { item.reading_region = Some(1); }
                result.push(Boundary { kind: BoundaryKind::Column, left, right, context: vec![page.context.clone()] });
            }
        }
        if let Some(next) = pages.get(index + 1) {
            if next.selected && page.index.checked_add(1) == Some(next.index)
                && page.number.checked_add(1) == Some(next.number) {
                let left_layouts = std::iter::once(&layouts[index].regions).chain(layouts[index].alternate_regions.iter());
                let right_layouts = std::iter::once(&layouts[index + 1].regions).chain(layouts[index + 1].alternate_regions.iter()).collect::<Vec<_>>();
                let mut page_orders = HashSet::new();
                for left_regions in left_layouts {
                    let left = left_regions.iter().flat_map(|region| region.items.iter().cloned()).collect::<Vec<_>>();
                    for right_regions in &right_layouts {
                        let right = right_regions.iter().flat_map(|region| region.items.iter().cloned()).collect::<Vec<_>>();
                        let key = (left.iter().map(|item| item.paragraph).collect::<Vec<_>>(), right.iter().map(|item| item.paragraph).collect::<Vec<_>>());
                        if !left.is_empty() && !right.is_empty() && page_orders.insert(key) {
                            result.push(Boundary { kind: BoundaryKind::Page, left: left.clone(), right, context: vec![page.context.clone(), next.context.clone()] });
                        }
                    }
                }
            }
        }
    }
    result
}

pub(crate) fn validate_selection(
    boundary: &Boundary, selected: &[(usize, usize)], skipped: &[(usize, usize)],
) -> Result<(), &'static str> {
    if selected.len() < 2 { return Err("selection needs both sides"); }
    let ordered: Vec<_> = boundary.left.iter().chain(&boundary.right).collect();
    let mut positions = HashMap::new();
    for (index, item) in ordered.iter().enumerate() {
        if positions.insert((item.page, item.paragraph), index).is_some() { return Err("duplicate boundary location"); }
    }
    let mut selected_indices = HashSet::new();
    let mut previous = None;
    for location in selected {
        let Some(&index) = positions.get(location) else { return Err("unknown selected location"); };
        if previous.is_some_and(|previous| index <= previous) { return Err("selected locations are repeated or out of order"); }
        if ordered[index].heading_barrier { return Err("selection crosses a heading"); }
        selected_indices.insert(index);
        previous = Some(index);
    }
    let first = positions[&selected[0]];
    let last = positions[&selected[selected.len() - 1]];
    if first >= boundary.left.len() || last < boundary.left.len() { return Err("selection does not cross the boundary"); }
    if boundary.kind == BoundaryKind::Column {
        let left = selected.iter().filter_map(|location| positions.get(location).copied())
            .filter(|index| *index < boundary.left.len()).last().unwrap();
        let right = selected.iter().filter_map(|location| positions.get(location).copied())
            .find(|index| *index >= boundary.left.len()).unwrap();
        if adjacent_line_fragments(ordered[left], ordered[right]) {
            return Err("same-line fragments are not a column boundary");
        }
    }
    let mut skipped_indices = HashSet::new();
    for location in skipped {
        let Some(&index) = positions.get(location) else { return Err("unknown skipped location"); };
        if index <= first || index >= last || selected_indices.contains(&index) || !skipped_indices.insert(index) {
            return Err("invalid skipped location");
        }
    }
    for index in first..=last {
        if ordered[index].heading_barrier { return Err("selection crosses a heading"); }
        if !selected_indices.contains(&index) && !skipped_indices.contains(&index) {
            return Err("intervening location was not accounted for");
        }
    }
    Ok(())
}

pub(crate) fn collect(document: &mut Document, config: &Bound<'_, PyAny>) -> PyResult<Vec<Boundary>> {
    let view = document.view::<ReadOnlyView>();
    let mut pages = Vec::new();
    for (index, page) in view.document().page_iter(&view).map_err(access)?.enumerate() {
        let page = page.map_err(access)?;
        let Some(number) = page.page_number(&view).map_err(access)?.and_then(|number| usize::try_from(number).ok()) else { continue };
        let Some(crop) = page.cropbox(&view).map_err(access)? else { continue };
        let Some(crop_box) = crop.r#box(&view).map_err(access)? else { continue };
        let Some(page_bounds) = bounds(&crop_box, &view)? else { continue };
        let selected = config.call_method1("should_translate_page", (number + 1,))?.is_truthy()?;
        let mut paragraphs = Vec::new();
        for (paragraph_index, paragraph) in page.pdf_paragraph_iter(&view).map_err(access)?.enumerate() {
            let paragraph = paragraph.map_err(access)?;
            let Some(item_box) = paragraph.r#box(&view).map_err(access)? else { continue };
            let Some(item_bounds) = bounds(&item_box, &view)? else { continue };
            let Some(text) = paragraph.unicode(&view).map_err(access)? else { continue };
            let font_size = match paragraph.pdf_style(&view).map_err(access)? {
                Some(style) => style.font_size(&view).map_err(access)?.filter(|size| size.is_finite()), None => None,
            };
            let label = paragraph.layout_label(&view).map_err(access)?.clone();
            paragraphs.push(Candidate { page: index, paragraph: paragraph_index, text: text.clone(),
                heading_barrier: heading_label(label.as_deref()), label, bounds: relative(item_bounds, page_bounds),
                font_size, vertical: paragraph.vertical(&view).map_err(access)?.unwrap_or(false), reading_region: None });
        }
        let mut figures = Vec::new();
        for figure in page.pdf_figure_iter(&view).map_err(access)? {
            let figure = figure.map_err(access)?;
            if let Some(figure_box) = figure.r#box(&view).map_err(access)? {
                if let Some(item_bounds) = bounds(&figure_box, &view)? { figures.push(relative(item_bounds, page_bounds)); }
            }
        }
        pages.push(Page { index, number, selected, context: PageContext { page: index, paragraphs, figures } });
    }
    mark_running_headers(&mut pages);
    mark_reference_regions(&mut pages);
    Ok(collect_pages(&pages))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_numbered_references_need_typographic_and_bibliographic_evidence() {
        let reference_text = "1. Hurd, M. W. Circadian organization. Journal 13, 430–436 (1998). 2. Martino, T. A. Circadian rhythm. Journal 294, 1675–1683 (2008). 3. Ouyang, Y. Clocks. Journal 95, 8660–8664 (1998).";
        let mut reference = item(0, 1, "plain text", 0.1, 0.35, 0.6);
        reference.text = reference_text.into();
        reference.font_size = Some(6.0);
        let mut pages = vec![page(0, vec![item(0, 0, "text", 0.1, 0.45, 0.9), reference])];
        mark_reference_regions(&mut pages);
        assert_eq!(pages[0].context.paragraphs[1].label.as_deref(), Some("reference"));
        pages[0].context.paragraphs[1].label = Some("plain text".into());
        pages[0].context.paragraphs[1].text = "1. First, we analyzed 1998 samples. 2. Next, we assessed 2008 controls. 3. Finally, we found 2020 genes.".into();
        mark_reference_regions(&mut pages);
        assert_eq!(pages[0].context.paragraphs[1].label.as_deref(), Some("plain text"));
    }

    #[test]
    fn saved_patke_reference_region_does_not_block_body_continuation() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join(".crossboundary-acceptance-r2-20261112/05-Patke/requests.jsonl");
        if !path.is_file() { return; }
        let records = std::fs::read_to_string(path).unwrap();
        let boundary = records.lines().filter_map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).ok()?;
            let prompt = record["prompt"].as_str()?;
            let metadata = prompt.split_once("Metadata: ")?.1
                .split_once(" Formula markers and source characters: ")?.0;
            let mut metadata: serde_json::Value = serde_json::from_str(metadata).ok()?;
            metadata["context"] = serde_json::json!([]);
            let boundary: Boundary = serde_json::from_value(metadata).ok()?;
            (boundary.kind == BoundaryKind::Page
                && boundary.right.iter().any(|item| item.paragraph == 5)).then_some(boundary)
        }).next().expect("saved Patke page-boundary metadata");
        let mut pages = vec![page(0, boundary.left), page(1, boundary.right)];
        mark_reference_regions(&mut pages);
        let reference = pages[1].context.paragraphs.iter().find(|item| item.paragraph == 5).unwrap();
        assert_eq!(reference.label.as_deref(), Some("reference"));
        let body = pages[1].context.paragraphs.iter().find(|item| item.paragraph == 0).unwrap();
        assert_eq!(body.label.as_deref(), Some("plain text"));
        let boundary = Boundary { kind: BoundaryKind::Page,
            left: pages[0].context.paragraphs.clone(), right: pages[1].context.paragraphs.clone(),
            context: pages.iter().map(|page| page.context.clone()).collect() };
        assert!(can_skip(&boundary, reference));
        assert!(!can_skip(&boundary, body));
    }

    #[test]
    fn adjacent_fragments_are_not_real_columns() {
        let left = item(0, 0, "plain text", 0.5145, 0.5388, 0.355);
        let mut right = item(0, 1, "plain text", 0.5425, 0.5877, 0.355);
        assert!(adjacent_line_fragments(&left, &right));
        right.bounds.left = 0.7;
        right.bounds.right = 0.95;
        assert!(!adjacent_line_fragments(&left, &right));
    }

    fn item(page: usize, paragraph: usize, label: &str, left: f64, right: f64, top: f64) -> Candidate {
        Candidate { page, paragraph, text: format!("synthetic {paragraph}"), label: Some(label.into()),
            bounds: Bounds { left, right, top, bottom: top - 0.05 }, font_size: Some(10.0), vertical: false,
            heading_barrier: heading_label(Some(label)), reading_region: None }
    }

    fn page(index: usize, paragraphs: Vec<Candidate>) -> Page {
        Page { index, number: index, selected: true, context: PageContext { page: index, paragraphs, figures: vec![] } }
    }

    #[test]
    fn collects_columns_and_cross_page_with_context_and_no_edge_cutoff() {
        let mut first = page(0, vec![item(0, 0, "text", 0.1, 0.45, 0.8),
            item(0, 1, "text", 0.1, 0.45, 0.5), item(0, 2, "caption", 0.1, 0.45, 0.3),
            item(0, 3, "text", 0.55, 0.9, 0.8), item(0, 4, "text", 0.55, 0.9, 0.45),
            item(0, 5, "title", 0.55, 0.9, 0.3)]);
        first.context.figures.push(Bounds { left: 0.55, right: 0.9, bottom: 0.05, top: 0.25 });
        let second = page(1, vec![item(1, 0, "title", 0.1, 0.9, 0.95), item(1, 1, "text", 0.1, 0.9, 0.65), item(1, 2, "text", 0.1, 0.9, 0.55)]);
        let result = collect_pages(&[first, second]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].kind, BoundaryKind::Column);
        assert_eq!(result[0].left.iter().map(|item| item.paragraph).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(result[0].right.iter().map(|item| item.paragraph).collect::<Vec<_>>(), vec![3, 4, 5]);
        assert_eq!(result[1].kind, BoundaryKind::Page);
        assert!(result[1].left.iter().any(|item| item.paragraph == 4));
        assert!(result[1].right.iter().any(|item| item.paragraph == 1));
        assert_eq!(result[1].context[0].figures.len(), 1);
        assert!(serde_json::to_string(&result).unwrap().contains("caption"));
    }

    #[test]
    fn never_crosses_unselected_or_missing_pages() {
        let first = page(0, vec![item(0, 0, "text", 0.1, 0.9, 0.5)]);
        let mut second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.5)]);
        second.selected = false;
        let third = page(2, vec![item(2, 0, "text", 0.1, 0.9, 0.5)]);
        assert!(collect_pages(&[first, second, third]).is_empty());
        assert!(collect_pages(&[page(0, vec![item(0, 0, "text", 0.1, 0.9, 0.5)]), page(2, vec![item(2, 0, "text", 0.1, 0.9, 0.5)])]).is_empty());
        let first = page(0, vec![item(0, 0, "text", 0.1, 0.9, 0.5)]);
        let mut second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.5)]);
        second.number = 2;
        assert!(collect_pages(&[first, second]).is_empty());
    }

    #[test]
    fn full_width_body_does_not_merge_two_columns() {
        let first = page(0, vec![
            item(0, 0, "text", 0.1, 0.9, 0.9),
            item(0, 1, "text", 0.1, 0.45, 0.7),
            item(0, 2, "text", 0.55, 0.9, 0.7),
        ]);
        let result = collect_pages(&[first]);
        let boundary = result.iter().find(|boundary| boundary.left.iter().any(|item| item.paragraph == 1)
            && boundary.right.iter().any(|item| item.paragraph == 2)).unwrap();
        assert_eq!(boundary.kind, BoundaryKind::Column);
    }

    #[test]
    fn short_columns_remain_distinct_under_full_width_body() {
        for font_size in [Some(10.0), None] {
            let mut left = item(0, 1, "text", 0.1, 0.2, 0.6);
            left.text = "We find".into();
            left.font_size = font_size;
            let mut right = item(0, 2, "text", 0.7, 0.8, 0.6);
            right.text = "a cure.".into();
            right.font_size = font_size;
            let first = page(0, vec![item(0, 0, "text", 0.05, 0.95, 0.9), left, right]);
            let result = collect_pages(&[first]);
            let boundary = result.iter().find(|boundary| boundary.left.iter().any(|item| item.text == "We find")
                && boundary.right.iter().any(|item| item.text == "a cure.")).unwrap();
            assert_eq!(boundary.kind, BoundaryKind::Column);
            assert_eq!(boundary.left.iter().map(|candidate| candidate.text.as_str()).collect::<Vec<_>>(), vec!["We find"]);
            assert_eq!(boundary.right.iter().map(|candidate| candidate.text.as_str()).collect::<Vec<_>>(), vec!["a cure."]);
        }
    }

    #[test]
    fn spanning_caption_and_small_text_do_not_split_column_band() {
        for (label, font_size, left_size) in [
            ("caption", 10.0, Some(10.0)), ("figure_caption", 10.0, Some(10.0)),
            ("tiny text", 8.0, Some(10.0)), ("text", 8.0, Some(10.0)), ("text", 8.0, None),
        ] {
            let mut non_body = item(0, 2, label, 0.05, 0.95, 0.7);
            non_body.font_size = Some(font_size);
            let mut upper_left = item(0, 0, "text", 0.1, 0.45, 0.9);
            upper_left.font_size = left_size;
            let mut lower_left = item(0, 3, "text", 0.1, 0.45, 0.5);
            lower_left.font_size = left_size;
            let first = page(0, vec![
                upper_left, item(0, 1, "text", 0.55, 0.9, 0.9),
                non_body,
                lower_left, item(0, 4, "text", 0.55, 0.9, 0.5),
            ]);
            let result = collect_pages(&[first]);
            let boundary = result.iter().find(|boundary| boundary.left.iter().any(|item| item.paragraph == 0)
                && boundary.left.iter().any(|item| item.paragraph == 3)
                && boundary.right.iter().any(|item| item.paragraph == 1)
                && boundary.right.iter().any(|item| item.paragraph == 4)).unwrap();
            assert_eq!(boundary.left.iter().chain(&boundary.right).filter(|candidate| candidate.paragraph == 2).count(), 1);
        }
    }

    #[test]
    fn two_caption_fragments_do_not_create_columns_in_full_width_body() {
        let first = page(0, vec![
            item(0, 0, "text", 0.05, 0.95, 0.9),
            item(0, 1, "caption", 0.1, 0.2, 0.6),
            item(0, 2, "caption", 0.7, 0.8, 0.6),
        ]);
        assert!(collect_pages(&[first]).is_empty());
    }

    #[test]
    fn mislabelled_short_column_remains_a_candidate() {
        let first = page(0, vec![
            item(0, 0, "text", 0.05, 0.95, 0.9),
            item(0, 1, "caption", 0.1, 0.2, 0.6),
            item(0, 2, "text", 0.7, 0.8, 0.6),
        ]);
        let result = collect_pages(&[first]);
        assert!(result.iter().any(|boundary| boundary.left.iter().any(|candidate| candidate.paragraph == 1)
            && boundary.right.iter().any(|candidate| candidate.paragraph == 2)));
        assert!(result.iter().any(|boundary| boundary.left.iter().chain(&boundary.right).any(|candidate| candidate.paragraph == 0)));
    }

    #[test]
    fn recognized_heading_labels_separate_column_bands() {
        for label in ["paragraph_title", "doc_title"] {
            let first = page(0, vec![
                item(0, 0, "text", 0.1, 0.45, 0.9), item(0, 1, "text", 0.55, 0.9, 0.9),
                item(0, 2, label, 0.1, 0.9, 0.7),
                item(0, 3, "text", 0.1, 0.45, 0.5), item(0, 4, "text", 0.55, 0.9, 0.5),
            ]);
            let result = collect_pages(&[first]);
            assert_eq!(result.len(), 2);
            assert_eq!(result[0].left[0].paragraph, 0);
            assert_eq!(result[0].right[0].paragraph, 1);
            assert_eq!(result[1].left[0].paragraph, 3);
            assert_eq!(result[1].right[0].paragraph, 4);
        }
    }

    #[test]
    fn mislabelled_column_without_body_anchor_is_still_a_candidate() {
        for include_spanning in [false, true] {
            let mut paragraphs = vec![
                item(0, 0, "caption", 0.1, 0.45, 0.7),
                item(0, 1, "text", 0.55, 0.9, 0.7),
            ];
            if include_spanning { paragraphs.push(item(0, 2, "text", 0.1, 0.9, 0.9)); }
            let result = collect_pages(&[page(0, paragraphs)]);
            assert!(result.iter().any(|boundary| boundary.left.iter().any(|candidate| candidate.paragraph == 0)
                && boundary.right.iter().any(|candidate| candidate.paragraph == 1)));
        }
    }

    #[test]
    fn cross_page_keeps_body_when_last_region_is_only_caption() {
        for caption_label in ["caption", "text"] {
            let first = page(0, vec![
                item(0, 0, "text", 0.1, 0.45, 0.7),
                item(0, 1, caption_label, 0.55, 0.9, 0.3),
            ]);
            let second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.7)]);
            let result = collect_pages(&[first, second]);
            let boundary = result.iter().find(|boundary| boundary.kind == BoundaryKind::Page).unwrap();
            assert_eq!(boundary.left.iter().map(|item| (item.paragraph, item.reading_region)).collect::<Vec<_>>(), vec![(0, Some(0)), (1, Some(1))]);
            assert_eq!(boundary.right.iter().map(|item| item.paragraph).collect::<Vec<_>>(), vec![0]);
        }
    }

    #[test]
    fn cross_page_exposes_ordered_reading_regions() {
        let first = page(0, vec![item(0, 0, "text", 0.1, 0.45, 0.8), item(0, 1, "text", 0.55, 0.9, 0.8)]);
        let second = page(1, vec![item(1, 0, "text", 0.1, 0.45, 0.8), item(1, 1, "text", 0.55, 0.9, 0.8)]);
        let result = collect_pages(&[first, second]);
        let boundary = result.iter().find(|boundary| boundary.kind == BoundaryKind::Page).unwrap();
        assert_eq!(boundary.left.iter().map(|item| (item.paragraph, item.reading_region)).collect::<Vec<_>>(), vec![(0, Some(0)), (1, Some(1))]);
        assert_eq!(boundary.right.iter().map(|item| (item.paragraph, item.reading_region)).collect::<Vec<_>>(), vec![(0, Some(0)), (1, Some(1))]);
    }

    #[test]
    fn spanning_heading_separates_column_bands() {
        let first = page(0, vec![
            item(0, 0, "text", 0.1, 0.45, 0.9), item(0, 1, "text", 0.55, 0.9, 0.9),
            item(0, 2, "title", 0.1, 0.9, 0.7),
            item(0, 3, "text", 0.1, 0.45, 0.5), item(0, 4, "text", 0.55, 0.9, 0.5),
        ]);
        let result = collect_pages(&[first]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].left[0].paragraph, 0);
        assert_eq!(result[0].right[0].paragraph, 1);
        assert_eq!(result[1].left[0].paragraph, 3);
        assert_eq!(result[1].right[0].paragraph, 4);
        assert!(result.iter().all(|boundary| boundary.context[0].paragraphs.iter().any(|item| item.paragraph == 2)));
    }

    #[test]
    fn spanning_body_after_columns_is_last_page_region() {
        let first = page(0, vec![
            item(0, 0, "text", 0.1, 0.45, 0.8), item(0, 1, "text", 0.55, 0.9, 0.8),
            item(0, 2, "text", 0.1, 0.9, 0.4),
        ]);
        let second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.8)]);
        let result = collect_pages(&[first, second]);
        let boundary = result.iter().find(|boundary| boundary.kind == BoundaryKind::Page).unwrap();
        assert_eq!(boundary.left.last().unwrap().paragraph, 2);
        assert_eq!(boundary.left.last().unwrap().reading_region, Some(2));
    }

    #[test]
    fn marginal_objects_do_not_steal_body_column_adjacency() {
        for (label, text) in [("number", "17"), ("text", "17"), ("page_header", "Journal title"),
            ("caption", "Fig. 1"), ("text", "Fig. 1")] {
            let mut middle = item(0, 1, label, 0.48, 0.52, 0.96);
            middle.text = text.into();
            let first = page(0, vec![
                item(0, 0, "text", 0.1, 0.45, 0.8), middle,
                item(0, 2, "text", 0.55, 0.9, 0.8),
            ]);
            let result = collect_pages(&[first]);
            let boundary = result.iter().find(|boundary| boundary.left.iter().any(|item| item.paragraph == 0)
                && boundary.right.iter().any(|item| item.paragraph == 2)).unwrap();
            assert_eq!(boundary.context[0].paragraphs[1].text, text);
            assert!(boundary.left.iter().chain(&boundary.right).any(|item| item.paragraph == 1));
            let ordered: Vec<_> = boundary.left.iter().chain(&boundary.right).map(|item| item.paragraph).collect();
            let middle_position = ordered.iter().position(|paragraph| *paragraph == 1).unwrap();
            let left_position = ordered.iter().position(|paragraph| *paragraph == 0).unwrap();
            let skipped = if middle_position > left_position { vec![(0, 1)] } else { vec![] };
            assert!(validate_selection(boundary, &[(0, 0), (0, 2)], &skipped).is_ok());
        }
    }

    #[test]
    fn narrow_mislabelled_body_also_has_alternative_column_boundaries() {
        let first = page(0, vec![
            item(0, 0, "text", 0.1, 0.45, 0.8), item(0, 1, "caption", 0.48, 0.52, 0.8),
            item(0, 2, "text", 0.55, 0.9, 0.8),
        ]);
        let result = collect_pages(&[first]);
        assert!(result.iter().any(|boundary| boundary.left.iter().any(|item| item.paragraph == 0)
            && boundary.right.iter().any(|item| item.paragraph == 2)));
        assert!(result.iter().any(|boundary| boundary.left.iter().any(|item| item.paragraph == 0)
            && boundary.right.iter().any(|item| item.paragraph == 1)));
        assert!(result.iter().any(|boundary| boundary.left.iter().any(|item| item.paragraph == 1)
            && boundary.right.iter().any(|item| item.paragraph == 2)));
    }

    #[test]
    fn full_width_objects_keep_both_body_and_spanning_continuations() {
        for label in ["caption", "text"] {
            let first = page(0, vec![
                item(0, 0, "text", 0.1, 0.45, 0.9), item(0, 1, "text", 0.55, 0.9, 0.9),
                item(0, 2, label, 0.05, 0.95, 0.7),
                item(0, 3, "text", 0.1, 0.45, 0.5), item(0, 4, "text", 0.55, 0.9, 0.5),
            ]);
            let second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.8)]);
            let result = collect_pages(&[first, second]);
            let direct = result.iter().find(|boundary| boundary.kind == BoundaryKind::Column
                && boundary.left.iter().any(|item| item.paragraph == 3)
                && boundary.right.iter().any(|item| item.paragraph == 1)).unwrap();
            assert!(direct.left.iter().chain(&direct.right).any(|item| item.paragraph == 2));
            assert!(result.iter().any(|boundary| boundary.kind == BoundaryKind::Column
                && boundary.left.iter().any(|item| item.paragraph == 1)
                && boundary.right.iter().any(|item| item.paragraph == 2)));
            assert!(result.iter().any(|boundary| boundary.kind == BoundaryKind::Column
                && boundary.left.iter().any(|item| item.paragraph == 2)
                && boundary.right.iter().any(|item| item.paragraph == 3)));
            assert!(result.iter().filter(|boundary| boundary.kind == BoundaryKind::Page).any(|boundary| {
                let order: Vec<_> = boundary.left.iter().map(|item| item.paragraph).collect();
                order == vec![0, 1, 2, 3, 4]
            }));
        }
    }

    #[test]
    fn full_width_body_at_band_edges_has_bidirectional_neighbors() {
        for label in ["text", "caption"] {
            let first = page(0, vec![
                item(0, 0, label, 0.1, 0.9, 0.9), item(0, 1, "text", 0.1, 0.45, 0.7),
                item(0, 2, "text", 0.55, 0.9, 0.7), item(0, 3, label, 0.1, 0.9, 0.4),
            ]);
            let result = collect_pages(&[first]);
            for (left, right) in [(0, 1), (1, 2), (2, 3)] {
                assert!(result.iter().any(|boundary| boundary.left.iter().any(|item| item.paragraph == left)
                    && boundary.right.iter().any(|item| item.paragraph == right)));
            }
        }
    }

    #[test]
    fn selection_requires_every_intervening_location_and_never_skips_heading() {
        for label in ["caption", "text"] {
            let first = page(0, vec![
                item(0, 0, "text", 0.1, 0.45, 0.8), item(0, 1, label, 0.1, 0.45, 0.5),
                item(0, 2, "text", 0.55, 0.9, 0.8),
            ]);
            let result = collect_pages(&[first]);
            let boundary = result.iter().find(|boundary| boundary.left.iter().any(|item| item.paragraph == 0)
                && boundary.right.iter().any(|item| item.paragraph == 2)).unwrap();
            assert!(validate_selection(boundary, &[(0, 0), (0, 2)], &[]).is_err());
            assert!(validate_selection(boundary, &[(0, 0), (0, 2)], &[(0, 1)]).is_ok());
            assert!(validate_selection(boundary, &[(0, 0), (0, 2)], &[(0, 1), (0, 1)]).is_err());
            assert!(validate_selection(boundary, &[(0, 0), (0, 2)], &[(0, 9)]).is_err());
            assert!(validate_selection(boundary, &[(0, 2), (0, 0)], &[(0, 1)]).is_err());
        }

        let first = page(0, vec![item(0, 0, "text", 0.1, 0.9, 0.8)]);
        let second = page(1, vec![item(1, 0, "title", 0.1, 0.9, 0.9), item(1, 1, "text", 0.1, 0.9, 0.7)]);
        let result = collect_pages(&[first, second]);
        let boundary = result.iter().find(|boundary| boundary.kind == BoundaryKind::Page).unwrap();
        assert!(boundary.right[0].heading_barrier);
        assert!(validate_selection(boundary, &[(0, 0), (1, 1)], &[(1, 0)]).is_err());
        assert!(validate_selection(boundary, &[(0, 0), (1, 0)], &[]).is_err());
    }

    #[test]
    fn spanning_heading_separates_mislabelled_body_columns() {
        let first = page(0, vec![
            item(0, 0, "caption", 0.1, 0.45, 0.9), item(0, 1, "caption", 0.55, 0.9, 0.9),
            item(0, 2, "paragraph_title", 0.1, 0.9, 0.7),
            item(0, 3, "caption", 0.1, 0.45, 0.5), item(0, 4, "caption", 0.55, 0.9, 0.5),
        ]);
        let second = page(1, vec![item(1, 0, "text", 0.1, 0.9, 0.7)]);
        let result = collect_pages(&[first, second]);
        let boundary = result.iter().find(|boundary| boundary.kind == BoundaryKind::Page).unwrap();
        assert_eq!(boundary.left.iter().map(|item| item.paragraph).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        assert!(validate_selection(boundary, &[(0, 1), (1, 0)], &[(0, 2), (0, 3), (0, 4)]).is_err());
    }
}
