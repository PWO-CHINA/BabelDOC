//! Serial paragraph construction. Selections preserve source-object identity.
use super::paragraph_finder_inputs::{DebugIdSource, ParagraphFinderInputs};
use super::tree_metrics::*;
use crate::shared::layout_helper::{
    self as lh, HEIGHT_NOT_USFUL_CHAR_IN_CHAR, Layout, LayoutIndex, SPACE_REGEX, UnicodePart,
};
use crate::shared::style::{INDIGO, PaletteColor, WHITE};
use crate::{StageContext, StageError};
use babeldoc_il_types::{Box as B, PdfCharacter};
use babeldoc_ir_core::{self as ir, Cursor, ParagraphFinderView, TreeView, types};
use log::{error, warn};
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;
type View<'a> = TreeView<'a, ParagraphFinderView>;
type P = Cursor<types::PdfParagraph>;
type C = Cursor<types::PdfCharacter>;
type Co = Cursor<types::PdfParagraphComposition>;
type Page = Cursor<types::Page>;
const LOG_TARGET: &str = "babeldoc.format.pdf.document_il.midend.paragraph_finder";
static CID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\(cid:\d+\)$").expect("literal CID regex"));
static DOTS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.{20,}").expect("literal dots regex"));
fn median(xs: &[f64]) -> f64 {
    match xs.len() {
        0 => 0.,
        n if n % 2 == 1 => xs[n / 2],
        n => (xs[n / 2 - 1] + xs[n / 2]) / 2.,
    }
}
fn python_space(c: char) -> bool {
    matches!(c as u32,0x09..=0x0d|0x1c..=0x20|0x85|0xa0|0x1680|0x2000..=0x200a|0x2028..=0x2029|0x202f|0x205f|0x3000)
}
fn space(s: &str) -> bool {
    !s.is_empty() && s.chars().all(python_space)
}
fn id(ids: &mut dyn DebugIdSource) -> Result<String, StageError> {
    let id = ids.next_id()?;
    if id.len() != 5
        || !id
            .chars()
            .all(|c| "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz".contains(c))
    {
        return Err(StageError::failed(
            "injected debug ID must be five Base58 characters",
        ));
    }
    Ok(id)
}
fn chars(c: &Co, v: &View<'_>) -> Result<Vec<C>, StageError> {
    if let Some(l) = c.pdf_line(v)? {
        Ok(l.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?)
    } else if let Some(f) = c.pdf_formula(v)? {
        Ok(f.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?)
    } else if let Some(s) = c.pdf_same_style_characters(v)? {
        Ok(s.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?)
    } else if let Some(ch) = c.pdf_character(v)? {
        Ok(vec![ch])
    } else {
        Ok(vec![])
    }
}
fn all_chars(p: &P, v: &View<'_>) -> Result<Vec<C>, StageError> {
    let mut out = vec![];
    for composition in p.pdf_paragraph_composition_iter(v)? {
        let c = composition?;
        out.extend(chars(&c, v)?);
    }
    Ok(out)
}
fn text_chars(cs: &[C], v: &View<'_>) -> Result<String, StageError> {
    let mut out = String::new();
    for c in cs {
        out.push_str(c.char_unicode(v)?.as_deref().unwrap_or(""));
    }
    Ok(out)
}
fn text_chars_iter(
    characters: impl Iterator<Item = Result<C, ir::AccessError>>,
    v: &View<'_>,
) -> Result<String, StageError> {
    let mut out = String::new();
    for character in characters {
        out.push_str(character?.char_unicode(v)?.as_deref().unwrap_or(""));
    }
    Ok(out)
}
fn update(p: &P, v: &mut View<'_>, unicode: bool) -> Result<(), StageError> {
    if p.pdf_paragraph_composition_len(v)? == 0 {
        return Ok(());
    }
    let mut cs = vec![];
    for c in p.pdf_paragraph_composition_iter(v)? {
        let c = c?;
        if c.pdf_line(v)?.is_some() || c.pdf_formula(v)?.is_some() || c.pdf_character(v)?.is_some()
        {
            cs.extend(chars(&c, v)?);
        } else if c.pdf_same_style_unicode_characters(v)?.is_none() {
            error!(target:LOG_TARGET,"Unexpected composition type in PdfParagraphComposition. This type only appears in the IL after the translation is completed.");
        }
    }
    if cs.is_empty() {
        return Ok(());
    }
    if unicode {
        let values = cs
            .iter()
            .map(|c| character(c, v))
            .collect::<Result<Vec<_>, _>>()?;
        let txt = lh::get_char_unicode_string(
            &values
                .iter()
                .map(UnicodePart::Character)
                .collect::<Vec<_>>(),
        )?;
        p.set_unicode(v, Some(txt))?;
    }
    let b = union(
        &cs.iter()
            .map(|c| visual(c, v))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let first = cs.first().unwrap();
    let x = need(b.x, "box.x")?;
    p.set_vertical(v, first.vertical(v)?)?;
    p.set_xobj_id(v, first.xobj_id(v)?)?;
    let indent = if let Some(l) = p.pdf_paragraph_composition_at(v, 0)?.pdf_line(v)? {
        match l.pdf_character_iter(v)?.next().transpose()? {
            Some(c) => need(visual(&c, v)?.x, "box.x")? - x > 1.,
            None => false,
        }
    } else {
        false
    };
    p.set_first_line_indent(v, Some(indent))?;
    p.set_box(v, Some(box_value(b)))?;
    Ok(())
}
fn line(
    cs: &[C],
    v: &mut View<'_>,
) -> Result<ir::FieldValue<types::PdfParagraphComposition>, StageError> {
    let b = union(
        &cs.iter()
            .map(|c| visual(c, v))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let values = cs
        .iter()
        .map(|c| c.share(v).map_err(StageError::from))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ir::PdfParagraphCompositionInit {
        pdf_line: Some(
            ir::PdfLineInit {
                r#box: Some(box_value(b)),
                pdf_character: values,
                ..Default::default()
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value())
}
fn plain_reading_character(character: &PdfCharacter) -> bool {
    let Some(text) = character.char_unicode.as_deref() else { return false; };
    !text.is_empty()
        && text.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch.is_ascii_whitespace()
                || matches!(ch, ',' | '.' | ';' | ':' | '-' | '\'' | '"' | '(' | ')' | '/' | '&' | '©' | '–' | '—' | '’')
                || ('\u{00c0}'..='\u{024f}').contains(&ch)
                || ('\u{fb00}'..='\u{fb06}').contains(&ch)
        })
}
fn character_geometry(character: &PdfCharacter) -> Option<(f64, f64, f64, f64, f64)> {
    let bounds = character.r#box.as_deref()?;
    let size = character.pdf_style.as_deref()?.font_size?;
    let (x, y, x2, y2) = (bounds.x?, bounds.y?, bounds.x2?, bounds.y2?);
    if [x, y, x2, y2, size].iter().all(|value| value.is_finite())
        && size > 0.0 && x2 > x && y2 > y {
        Some((x, y, x2, y2, size))
    } else {
        None
    }
}
fn reading_lines(characters: &[PdfCharacter]) -> Option<Vec<Vec<usize>>> {
    if characters.is_empty() { return None; }
    let first = &characters[0];
    let first_geometry = character_geometry(first)?;
    let geometries = characters.iter().map(|character| {
        let geometry = character_geometry(character)?;
        if character.vertical.unwrap_or(false)
            || character.formula_layout_id.is_some()
            || character.xobj_id != first.xobj_id
            || character.pdf_style.as_deref()?.font_id.is_none()
            || !matches!(character.scale, None | Some(1.0))
            || (geometry.4 / first_geometry.4 - 1.0).abs() > 0.05
            || !plain_reading_character(character) {
            return None;
        }
        Some(geometry)
    }).collect::<Option<Vec<_>>>()?;
    let mut indexes: Vec<_> = (0..characters.len()).collect();
    indexes.sort_by(|left, right| geometries[*right].1.total_cmp(&geometries[*left].1));
    let mut lines: Vec<Vec<usize>> = Vec::new();
    for index in indexes {
        if let Some(last) = lines.last_mut() {
            let baseline = geometries[last[0]].1;
            if (baseline - geometries[index].1).abs() <= first_geometry.4 * 0.12 {
                last.push(index);
                continue;
            }
            if (baseline - geometries[index].1).abs() < first_geometry.4 * 0.55 {
                return None;
            }
        }
        lines.push(vec![index]);
    }
    for line in &mut lines {
        line.sort_by(|left, right| geometries[*left].0.total_cmp(&geometries[*right].0));
        for pair in line.windows(2) {
            let left = geometries[pair[0]];
            let right = geometries[pair[1]];
            let gap = right.0 - left.2;
            if gap > first_geometry.4 * 1.5
                || gap < -(left.2 - left.0).min(right.2 - right.0) * 0.45 {
                return None;
            }
        }
    }
    for pair in lines.windows(2) {
        let left = geometries[*pair[0].first()?].0.max(geometries[*pair[1].first()?].0);
        let right = pair.iter().map(|line| {
            line.iter().map(|index| geometries[*index].2).fold(f64::NEG_INFINITY, f64::max)
        }).fold(f64::INFINITY, f64::min);
        if right <= left {
            return None;
        }
    }
    Some(lines)
}
fn token_character(character: &PdfCharacter) -> bool {
    character.char_unicode.as_deref().is_some_and(|text| {
        !text.is_empty() && text.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '/' | '-' | '_' | ':' | '?' | '#' | '%' | '&' | '=' | '+' | '~' | '@')
                || ('\u{00c0}'..='\u{024f}').contains(&ch)
                || ('\u{fb00}'..='\u{fb06}').contains(&ch)
        })
    })
}
fn token_neighbors(left: &PdfCharacter, right: &PdfCharacter) -> bool {
    let (Some(a), Some(b)) = (character_geometry(left), character_geometry(right)) else { return false; };
    let (Some(left_style), Some(right_style)) = (left.pdf_style.as_deref(), right.pdf_style.as_deref()) else { return false; };
    let gap = b.0 - a.2;
    token_character(left) && token_character(right)
        && !left.vertical.unwrap_or(false) && !right.vertical.unwrap_or(false)
        && matches!(left.scale, None | Some(1.0)) && matches!(right.scale, None | Some(1.0))
        && left.xobj_id == right.xobj_id
        && left.formula_layout_id.is_none()
        && right.formula_layout_id.is_none()
        && left_style.font_id.is_some() && left_style.font_id == right_style.font_id
        && (a.4 / b.4 - 1.0).abs() <= 0.05
        && (a.1 - b.1).abs() <= a.4 * 0.12
        && gap >= -(a.2 - a.0).min(b.2 - b.0) * 0.45
        && gap <= a.4 * 0.2
}
fn overlapping_layouts(first: i64, second: i64, layouts: &[babeldoc_il_types::PageLayout]) -> bool {
    let bounds = |id| layouts.iter().find(|layout| layout.id == Some(id))
        .and_then(|layout| layout.r#box.as_deref())
        .and_then(|bounds| Some((bounds.x?, bounds.y?, bounds.x2?, bounds.y2?)));
    let (Some(a), Some(b)) = (bounds(first), bounds(second)) else { return false; };
    [a.0, a.1, a.2, a.3, b.0, b.1, b.2, b.3].iter().all(|value| value.is_finite())
        && a.2 > a.0 && b.2 > b.0 && a.3 > a.1 && b.3 > b.1
        && a.2.min(b.2) - a.0.max(b.0) >= (a.2 - a.0).min(b.2 - b.0) * 0.5
        && a.1 <= b.3 && b.1 <= a.3
}
fn reconcile_token_layouts(
    characters: &[PdfCharacter],
    assignments: &mut [Option<Layout>],
    layouts: &[babeldoc_il_types::PageLayout],
) {
    let allowed = |layout: &Option<Layout>| layout.as_ref().is_some_and(|layout| {
        layout.id.is_some() && matches!(layout.name.as_deref(), Some("plain text" | "tiny text" | "text" | "title"))
    });
    let mut start = 0;
    while start < characters.len() {
        let mut end = start + 1;
        while end < characters.len() && allowed(&assignments[end - 1]) && allowed(&assignments[end])
            && token_neighbors(&characters[end - 1], &characters[end]) {
            end += 1;
        }
        if end - start >= 3 {
            let mut counts: HashMap<i64, usize> = HashMap::new();
            for layout in assignments[start..end].iter().flatten() {
                if let Some(id) = layout.id { *counts.entry(id).or_default() += 1; }
            }
            if counts.len() > 1 {
                if let Some((&id, &count)) = counts.iter().max_by_key(|entry| entry.1) {
                    let owner_name = assignments[start..end].iter().flatten()
                        .find(|layout| layout.id == Some(id)).and_then(|layout| layout.name.as_deref());
                    if count * 5 >= (end - start) * 3
                        && counts.keys().all(|other| *other == id || overlapping_layouts(id, *other, layouts))
                        && characters[start..end].iter().zip(&assignments[start..end]).all(|(character, assignment)| {
                            if assignment.as_ref().and_then(|layout| layout.id) == Some(id) {
                                return true;
                            }
                            if assignment.as_ref().and_then(|layout| layout.name.as_deref()) == owner_name {
                                return false;
                            }
                            let owner = layouts.iter().find(|layout| layout.id == Some(id)).and_then(|layout| layout.r#box.as_deref());
                            let visual = character.visual_bbox.as_deref().and_then(|visual| visual.r#box.as_deref());
                            match (owner, visual) {
                                (Some(owner), Some(visual)) => {
                                    lh::calculate_iou_for_boxes(visual, owner).is_ok_and(|overlap| overlap >= 0.5)
                                }
                                _ => false,
                            }
                        }) {
                        let owner = assignments[start..end].iter().flatten().find(|layout| layout.id == Some(id)).cloned();
                        for assignment in &mut assignments[start..end] { *assignment = owner.clone(); }
                    }
                }
            }
        }
        start = end;
    }
}
fn split_lines(p: &P, v: &mut View<'_>) -> Result<bool, StageError> {
    let mut cs = vec![];
    let mut others = vec![];
    for child_index in 0..p.pdf_paragraph_composition_len(v)? {
        let c = p.pdf_paragraph_composition_at(v, child_index)?;
        if let Some(ch) = c.pdf_character(v)? {
            cs.push(ch.retain(v)?);
        } else {
            others.push(c.retain(v)?);
        }
    }
    if cs.is_empty() {
        return Ok(false);
    }
    if others.is_empty() && !matches!(p.layout_label(v)?.as_deref(),
        Some("table_text" | "table_cell" | "table_cell_hybrid" | "wired_table_cell" | "wireless_table_cell")) {
        let snapshots = cs.iter().map(|ch| character(ch, v)).collect::<Result<Vec<_>, _>>()?;
        if let Some(lines) = reading_lines(&snapshots) {
            let mut out = Vec::with_capacity(lines.len());
            for indices in lines {
                let selected = indices.into_iter().map(|index| cs[index].clone()).collect::<Vec<_>>();
                out.push(line(&selected, v)?);
            }
            p.set_pdf_paragraph_composition(v, out)?;
            update(p, v, false)?;
            return Ok(true);
        }
    }
    let bounds = cs
        .iter()
        .map(|c| {
            let b = visual(c, v)?;
            Ok((need(b.y, "y")?, need(b.y2, "y2")?))
        })
        .collect::<Result<Vec<_>, StageError>>()?;
    let min = bounds.iter().map(|b| b.0).fold(f64::INFINITY, f64::min);
    let max = bounds.iter().map(|b| b.1).fold(f64::NEG_INFINITY, f64::max);
    let step = 0.25;
    let m = ((max - min) / step).ceil() as usize;
    let mut separators = vec![];
    if max - min >= 5. && m > 0 {
        let mut hist = vec![0i32; m + 1];
        for (lo, hi) in &bounds {
            let start = ((max as f32 - *hi as f32) / step as f32).floor() as i32;
            let end =
                (((max as f32 - *lo as f32) / step as f32).floor() as i32 + 1).clamp(0, m as i32);
            for (i, delta) in [(start, 1i32), (end, -1i32)] {
                let i = if i < 0 { i + (m + 1) as i32 } else { i };
                if i >= 0 && (i as usize) <= m {
                    hist[i as usize] = hist[i as usize].wrapping_add(delta);
                }
            }
        }
        let mut sum = 0i32;
        let mut gap = None;
        for (i, h) in hist[..m].iter().enumerate() {
            sum = sum.wrapping_add(*h);
            if sum < 1 && gap.is_none() {
                gap = Some(i);
            } else if sum >= 1 {
                if let Some(start) = gap.take() {
                    separators.push(max - start as f64 * step);
                }
            }
        }
        if let Some(start) = gap {
            separators.push(max - start as f64 * step);
        }
        separators.sort_by(|a, b| b.total_cmp(a));
    }
    let mut lists = vec![vec![]; separators.len() + 1];
    for (ch, b) in cs.into_iter().zip(bounds) {
        let center = (b.0 + b.1) / 2.;
        let mut slot = 0;
        for sep in &separators {
            if center > *sep {
                break;
            }
            slot += 1;
        }
        lists[slot].push(ch);
    }
    let mut out = vec![];
    for cs in lists {
        if !cs.is_empty() {
            out.push(line(&cs, v)?);
        }
    }
    for c in others {
        out.push(c.share(v)?);
    }
    p.set_pdf_paragraph_composition(v, out)?;
    update(p, v, false)?;
    Ok(false)
}
fn dummy(
    source: &C,
    b: B,
    v: &mut View<'_>,
) -> Result<ir::FieldValue<types::PdfCharacter>, StageError> {
    let style = source.pdf_style(v)?.map(|s| s.share(v)).transpose()?;
    let shared = box_value(b.clone());
    // Python gives the dummy's PDF and visual boxes the same object.
    Ok(ir::PdfCharacterInit {
        pdf_style: style,
        r#box: Some(shared),
        char_unicode: Some(" ".into()),
        synthetic_spacing: Some(true),
        scale: source.scale(v)?,
        advance: Some(need(b.x2, "x2")? - need(b.x, "x")?),
        visual_bbox: Some(
            ir::VisualBboxInit {
                r#box: Some(box_value(b)),
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value())
}
fn dummy_share_box(ch: &C, v: &mut View<'_>) -> Result<(), StageError> {
    let b = need(ch.r#box(v)?, "dummy.box")?.share(v)?;
    need(ch.visual_bbox(v)?, "dummy.visual")?.set_box(v, Some(b))?;
    Ok(())
}
enum SpaceList {
    Line(Cursor<types::PdfLine>),
    Formula(Cursor<types::PdfFormula>),
    Same(Cursor<types::PdfSameStyleCharacters>),
}
impl SpaceList {
    fn characters(&self, v: &View<'_>) -> Result<Vec<C>, StageError> {
        Ok(match self {
            Self::Line(c) => c.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?,
            Self::Formula(c) => c.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?,
            Self::Same(c) => c.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?,
        })
    }
    fn replace(
        &self,
        v: &mut View<'_>,
        values: Vec<ir::FieldValue<types::PdfCharacter>>,
    ) -> Result<(), StageError> {
        match self {
            Self::Line(c) => c.set_pdf_character(v, values)?,
            Self::Formula(c) => c.set_pdf_character(v, values)?,
            Self::Same(c) => c.set_pdf_character(v, values)?,
        };
        Ok(())
    }
    fn push(
        &self,
        v: &mut View<'_>,
        value: ir::FieldValue<types::PdfCharacter>,
    ) -> Result<(), StageError> {
        match self {
            Self::Line(c) => c.push_pdf_character(v, value)?,
            Self::Formula(c) => c.push_pdf_character(v, value)?,
            Self::Same(c) => c.push_pdf_character(v, value)?,
        };
        Ok(())
    }
}
fn space_list(c: &Co, v: &View<'_>) -> Result<Option<SpaceList>, StageError> {
    Ok(if let Some(l) = c.pdf_line(v)? {
        Some(SpaceList::Line(l))
    } else if let Some(s) = c.pdf_same_style_characters(v)? {
        Some(SpaceList::Same(s))
    } else {
        c.pdf_formula(v)?.map(SpaceList::Formula)
    })
}
#[cfg(test)]
fn add_spaces(p: &P, v: &mut View<'_>) -> Result<(), StageError> {
    add_spaces_with_trusted_lines(p, v, false)
}
fn add_spaces_with_trusted_lines(p: &P, v: &mut View<'_>, trusted_lines: bool) -> Result<(), StageError> {
    for child_index in 0..p.pdf_paragraph_composition_len(v)? {
        let c = p.pdf_paragraph_composition_at(v, child_index)?;
        let Some(l) = space_list(&c, v)? else {
            continue;
        };
        let cs = l
            .characters(v)?
            .iter()
            .map(|c| c.retain(v))
            .collect::<Result<Vec<_>, _>>()?;
        let mut distances = vec![];
        for pair in cs.windows(2) {
            let a = box_read(&need(pair[0].r#box(v)?, "box")?, v)?;
            let b = box_read(&need(pair[1].r#box(v)?, "box")?, v)?;
            let d = need(b.x, "x")? - need(a.x2, "x2")?;
            if d > 1. {
                distances.push(d);
            }
        }
        distances.sort_by(f64::total_cmp);
        distances.dedup_by(|a, b| a == b);
        let med = match distances.as_slice() {
            [] => 1.,
            [one] => *one,
            many => many[1],
        };
        let mut out = vec![];
        let mut dummy_indices = vec![];
        for (i, ch) in cs.iter().enumerate() {
            out.push(ch.share(v)?);
            if let Some(next) = cs.get(i + 1) {
                let a = character(ch, v)?;
                let b = character(next, v)?;
                let bb = a.r#box.as_deref().unwrap();
                let d = need(b.r#box.as_deref().unwrap().x, "x")? - need(bb.x2, "x2")?;
                let newline = Layout::is_newline(Some(&a), &b);
                if d >= med || newline {
                    let x = need(bb.x2, "x2")?;
                    dummy_indices.push(out.len());
                    out.push(dummy(
                        ch,
                        B {
                            x: Some(x),
                            y: bb.y,
                            x2: Some(if newline { x } else { x + d.min(med) }),
                            y2: bb.y2,
                        },
                        v,
                    )?);
                }
            }
        }
        l.replace(v, out)?;
        for i in dummy_indices {
            dummy_share_box(&l.characters(v)?[i], v)?;
        }
    }
    let composition_len = p.pdf_paragraph_composition_len(v)?;
    for child_index in 0..composition_len.saturating_sub(1) {
        let first_composition = p.pdf_paragraph_composition_at(v, child_index)?;
        let second_composition = p.pdf_paragraph_composition_at(v, child_index + 1)?;
        let first = chars(&first_composition, v)?;
        let second = chars(&second_composition, v)?;
        let (Some(last), Some(next)) = (first.last(), second.first()) else {
            continue;
        };
        let a = box_read(&need(last.r#box(v)?, "box")?, v)?;
        let b = box_read(&need(next.r#box(v)?, "box")?, v)?;
        let d = need(b.x, "x")? - need(a.x2, "x2")?;
        let left = character(last, v)?;
        let right = character(next, v)?;
        let separated_line = trusted_lines && match (character_geometry(&left), character_geometry(&right)) {
            (Some(first), Some(second)) => {
                (first.1 - second.1).abs() >= first.4.min(second.4) * 0.55
                    && !space(left.char_unicode.as_deref().unwrap_or_default())
                    && !space(right.char_unicode.as_deref().unwrap_or_default())
            }
            _ => false,
        };
        if d > 1. || separated_line {
            if let Some(l) = space_list(&first_composition, v)? {
                let x = need(a.x2, "x2")?;
                let index = l.characters(v)?.len();
                let value = dummy(
                    last,
                    B {
                        x: Some(x),
                        y: a.y,
                        x2: Some(if separated_line { x } else { x + d }),
                        y2: a.y2,
                    },
                    v,
                )?;
                l.push(v, value)?;
                dummy_share_box(&l.characters(v)?[index], v)?;
            }
        }
    }
    Ok(())
}
fn spacing(p: &P, v: &mut View<'_>) -> Result<(), StageError> {
    let mut out = vec![];
    let composition_len = p.pdf_paragraph_composition_len(v)?;
    let mut following_text = vec![false; composition_len];
    let mut has_following_text = false;
    for child_index in (0..composition_len).rev() {
        following_text[child_index] = has_following_text;
        let composition = p.pdf_paragraph_composition_at(v, child_index)?;
        has_following_text |= text_chars(&chars(&composition, v)?, v)?.chars()
            .any(|character| !python_space(character));
    }
    for child_index in 0..composition_len {
        let c = p.pdf_paragraph_composition_at(v, child_index)?;
        if let Some(l) = c.pdf_line(v)? {
            let cs = l.pdf_character_iter(v)?.collect::<Result<Vec<_>, _>>()?;
            let text = text_chars(&cs, v)?;
            if text.trim_matches(python_space).is_empty() {
                continue;
            }
            let mut kept = vec![];
            for ch in cs {
                if !space(ch.char_unicode(v)?.as_deref().unwrap_or("")) || !kept.is_empty() {
                    kept.push(ch.retain(v)?);
                }
            }
            while let Some(c) = kept.last() {
                if !space(c.char_unicode(v)?.as_deref().unwrap_or("")) {
                    break;
                }
                if child_index + 1 < composition_len
                {
                    let source_separator = c.synthetic_spacing(v)? != Some(true)
                        && c.pdf_character_id(v)?.is_some() && following_text[child_index];
                    let wrapped_separator = c.synthetic_spacing(v)? == Some(true)
                        && c.r#box(v)?.is_some_and(|bounds| bounds.x(v).ok() == bounds.x2(v).ok());
                    if source_separator || wrapped_separator {
                        break;
                    }
                }
                kept.pop();
            }
            if !kept.is_empty() {
                out.push(line(&kept, v)?);
            }
        } else {
            out.push(c.share(v)?);
        }
    }
    p.set_pdf_paragraph_composition(v, out)?;
    update(p, v, false)
}
struct Palette {
    colors: HashMap<usize, Cursor<types::GraphicState>>,
}
impl Palette {
    fn value(
        &self,
        color: &'static PaletteColor,
        v: &mut View<'_>,
    ) -> Result<ir::FieldValue<types::GraphicState>, StageError> {
        if let Some(c) = self.colors.get(&(color as *const _ as usize)) {
            return Ok(c.share(v)?);
        }
        let state = color.get();
        let Some(babeldoc_il_types::PassthroughValue::Materialized(s)) =
            &state.as_ref().passthrough_per_char_instruction
        else {
            return Err(StageError::failed("palette"));
        };
        Ok(ir::GraphicStateInit {
            passthrough_per_char_instruction: Some(ir::Instructions::Materialized(s.clone())),
            ..Default::default()
        }
        .into_value())
    }
    fn rectangle(
        &mut self,
        page: &Page,
        v: &mut View<'_>,
        b: ir::FieldValue<types::Box>,
        color: &'static PaletteColor,
        fill: bool,
        debug: bool,
        width: Option<f64>,
        xobj: Option<i64>,
    ) -> Result<(), StageError> {
        let state = self.value(color, v)?;
        let i = page.pdf_rectangle_len(v)?;
        page.push_pdf_rectangle(
            v,
            ir::PdfRectangleInit {
                r#box: Some(b),
                graphic_state: Some(state),
                fill_background: Some(fill),
                debug_info: Some(debug),
                line_width: width,
                xobj_id: xobj,
                ..Default::default()
            }
            .into_value(),
        )?;
        let state = need(page.pdf_rectangle_at(v, i)?.graphic_state(v)?, "state")?.retain(v)?;
        self.colors
            .entry(color as *const _ as usize)
            .or_insert(state);
        Ok(())
    }
}
pub fn process_with(
    inputs: &mut ParagraphFinderInputs<'_>,
    document: &mut ir::Document,
    context: &mut StageContext<'_>,
) -> Result<(), StageError> {
    let mut v = document.view::<ParagraphFinderView>();
    let total = v.document().page_len(&v)? as u64;
    let mut palette = Palette {
        colors: HashMap::new(),
    };
    for page_index in 0..total as usize {
        let page = v.document().page_at(&v, page_index)?;
        context.check_cancelled()?;
        process_page(&page, &mut v, inputs, &mut palette)?;
        context.report_progress(1, Some(total))?;
    }
    if total == 0 {
        return Ok(());
    }
    let mut count = 0;
    let mut cid_count = 0;
    for page_index in 0..total as usize {
        let page = v.document().page_at(&v, page_index)?;
        for p in page.pdf_paragraph_iter(&v)? {
            let p = p?;
            count += 1;
            let cs = all_chars(&p, &v)?;
            let mut n = 0;
            for c in &cs {
                if c.char_unicode(&v)?
                    .as_deref()
                    .is_some_and(|s| CID_PATTERN.is_match(s))
                {
                    n += 1;
                }
            }
            if n as f64 > cs.len() as f64 * 0.8 {
                cid_count += 1;
            }
        }
    }
    if count == 0 {
        return Err(StageError::ExtractText(
            "The document contains no paragraphs.".into(),
        ));
    }
    if cid_count as f64 / count as f64 > 0.8 {
        return Err(StageError::ExtractText(
            "The document contains too many CID paragraphs.".into(),
        ));
    }
    drop(palette);
    drop(v);
    document.compact();
    Ok(())
}
fn process_page(
    page: &Page,
    v: &mut View<'_>,
    inputs: &mut ParagraphFinderInputs<'_>,
    palette: &mut Palette,
) -> Result<(), StageError> {
    let original_layouts = layouts(page, v)?;
    let (index, _) = LayoutIndex::build(&original_layouts)?;
    let text_boxes = original_layouts
        .iter()
        .filter(|l| lh::is_text_layout(Some(&Layout::new(l.id, l.class_name.clone()))))
        .filter_map(|l| l.r#box.as_deref())
        .collect::<Vec<_>>();
    for (i, l) in original_layouts.iter().enumerate() {
        if l.class_name.as_deref() == Some("formula") && !text_boxes.is_empty() {
            let mut isolated = true;
            for b in &text_boxes {
                if lh::calculate_iou_for_boxes(need(l.r#box.as_deref(), "layout.box")?, b)? >= 0.5 {
                    isolated = false;
                    break;
                }
            }
            if isolated {
                page.page_layout_at(v, i)?
                    .set_class_name(v, Some("isolate_formula".into()))?;
            }
        }
    }
    // Python layout_map holds the original objects, so relabeling is visible.
    let (_, map) = LayoutIndex::build(&layouts(page, v)?)?;
    let mut areas = page
        .pdf_character_iter(v)?
        .map(|c| {
            let c = c?;
            let b = visual(&c, v)?;
            Ok((need(b.x2, "x2")? - need(b.x, "x")?) * (need(b.y2, "y2")? - need(b.y, "y")?))
        })
        .collect::<Result<Vec<_>, StageError>>()?;
    areas.sort_by(f64::total_cmp);
    let med = median(&areas);
    let mut current = None::<P>;
    let mut current_layout = None::<Layout>;
    let mut skipped = vec![];
    let mut snapshots = Vec::with_capacity(page.pdf_character_len(v)?);
    let mut assignments = Vec::with_capacity(page.pdf_character_len(v)?);
    for child_index in 0..page.pdf_character_len(v)? {
        let ch = page.pdf_character_at(v, child_index)?;
        let mut snapshot = character(&ch, v)?;
        assignments.push(lh::get_character_layout(&snapshot, &index, &map)?);
        let fid = lh::is_character_in_formula_layout(
            &snapshot,
            &babeldoc_il_types::Page::default(),
            &index,
            &map,
        )?;
        ch.set_formula_layout_id(v, fid)?;
        snapshot.formula_layout_id = fid;
        snapshots.push(snapshot);
    }
    reconcile_token_layouts(&snapshots, &mut assignments, &original_layouts);
    for (child_index, (snapshot, layout)) in snapshots.iter().zip(assignments).enumerate() {
        let ch = page.pdf_character_at(v, child_index)?.retain(v)?;
        if !lh::is_text_layout(layout.as_ref())
            || matches!(
                snapshot.char_unicode.as_deref(),
                Some("(cid:122)" | "(cid:123)" | "(cid:124)" | "(cid:125)")
            )
        {
            skipped.push(ch.share(v)?);
            continue;
        }
        let layout = layout.unwrap();
        let b = visual(&ch, v)?;
        let area = (need(b.x2, "x2")? - need(b.x, "x")?) * (need(b.y2, "y2")? - need(b.y, "y")?);
        let mut new = current.is_none();
        if let (Some(p), Some(old)) = (&current, &current_layout) {
            let comps_len = p.pdf_paragraph_composition_len(v)?;
            if !(area < med * 0.05 && comps_len > 0 && layout.id == old.id)
                && !HEIGHT_NOT_USFUL_CHAR_IN_CHAR
                    .iter()
                    .any(|s| *s == snapshot.char_unicode.as_deref())
            {
                let different_xobj = match comps_len.checked_sub(1) {
                    Some(last) => {
                        match p.pdf_paragraph_composition_at(v, last)?.pdf_character(v)? {
                            Some(c) => c.xobj_id(v)? != snapshot.xobj_id,
                            None => false,
                        }
                    }
                    None => false,
                };
                if (layout.id != old.id
                    && !SPACE_REGEX
                        .find(snapshot.char_unicode.as_deref().unwrap_or(""))
                        .is_some_and(|m| m.start() == 0))
                    || different_xobj
                    || (lh::is_bullet_point(&snapshot) && comps_len == 0)
                {
                    new = true;
                }
            }
        }
        if new {
            let n = page.pdf_paragraph_len(v)?;
            let debug_id = id(inputs.debug_ids)?;
            page.push_pdf_paragraph(
                v,
                ir::PdfParagraphInit {
                    debug_id: Some(debug_id),
                    layout_id: layout.id,
                    layout_label: layout.name.clone(),
                    ..Default::default()
                }
                .into_value(),
            )?;
            current = Some(page.pdf_paragraph_at(v, n)?.retain(v)?);
            current_layout = Some(layout);
        }
        let value = ch.share(v)?;
        current.as_ref().unwrap().push_pdf_paragraph_composition(
            v,
            ir::PdfParagraphCompositionInit {
                pdf_character: Some(value),
                ..Default::default()
            }
            .into_value(),
        )?;
    }
    page.set_pdf_character(v, skipped)?;
    for child_index in 0..page.pdf_paragraph_len(v)? {
        let p = page.pdf_paragraph_at(v, child_index)?;
        update(&p, v, false)?;
    }
    // Even though line threading does not use these sets, Python collects
    // them here; retain its font decoding errors and None/empty regex semantics.
    for font in page.pdf_font_iter(v)? {
        let font = font?;
        crate::shared::formula_helper::font_with_matcher(
            font.name(v)?.as_deref().unwrap_or(""),
            inputs.config.formula_font_pattern_hits.as_ref(),
            inputs.config.formula_matcher.as_deref(),
        )?;
    }
    for xobject in page.pdf_xobject_iter(v)? {
        let xobject = xobject?;
        for font in xobject.pdf_font_iter(v)? {
            let font = font?;
            crate::shared::formula_helper::font_with_matcher(
                font.name(v)?.as_deref().unwrap_or(""),
                inputs.config.formula_font_pattern_hits.as_ref(),
                inputs.config.formula_matcher.as_deref(),
            )?;
        }
    }
    let mut trusted_lines = Vec::with_capacity(page.pdf_paragraph_len(v)?);
    for child_index in 0..page.pdf_paragraph_len(v)? {
        let p = page.pdf_paragraph_at(v, child_index)?;
        trusted_lines.push(split_lines(&p, v)?);
    }
    for child_index in 0..page.pdf_paragraph_len(v)? {
        let p = page.pdf_paragraph_at(v, child_index)?;
        add_spaces_with_trusted_lines(&p, v, trusted_lines[child_index])?;
        spacing(&p, v)?;
        update(&p, v, false)?;
    }
    // Splitting and merging mutate the page paragraph collection, so keep its initial snapshot.
    let initial_paragraphs = page.pdf_paragraph_iter(v)?.collect::<Result<Vec<_>, _>>()?;
    let mut paras = initial_paragraphs
        .iter()
        .map(|p| p.retain(v))
        .collect::<Result<Vec<_>, _>>()?;
    let mut widths = vec![];
    for p in &paras {
        for c in p.pdf_paragraph_composition_iter(v)? {
            let c = c?;
            if let Some(l) = c.pdf_line(v)? {
                let b = box_read(&need(l.r#box(v)?, "line.box")?, v)?;
                widths.push(need(b.x2, "x2")? - need(b.x, "x")?);
            }
        }
    }
    widths.sort_by(f64::total_cmp);
    let width = median(&widths);
    let mut i = 0;
    while i < paras.len() {
        let p = paras[i].clone();
        let composition_len = p.pdf_paragraph_composition_len(v)?;
        for j in 1..composition_len {
            let previous_composition = p.pdf_paragraph_composition_at(v, j - 1)?;
            let current_composition = p.pdf_paragraph_composition_at(v, j)?;
            let Some(prev) = previous_composition.pdf_line(v)? else {
                continue;
            };
            let b = box_read(&need(prev.r#box(v)?, "line.box")?, v)?;
            let text = text_chars_iter(prev.pdf_character_iter(v)?, v)?;
            let bullet = match current_composition.pdf_line(v)? {
                Some(l) if l.pdf_character_len(v)? > 0 => {
                    lh::is_bullet_point(&character(&l.pdf_character_at(v, 0)?, v)?)
                }
                Some(_) => false,
                None => false,
            };
            if DOTS_PATTERN.is_match(&text)
                || (inputs.config.split_short_lines
                    && need(b.x2, "x2")? - need(b.x, "x")?
                        < width * inputs.config.short_line_split_factor)
                || bullet
            {
                let mut tail = Vec::with_capacity(composition_len - j);
                for index in j..composition_len {
                    tail.push(p.pdf_paragraph_composition_at(v, index)?.share(v)?);
                }
                let mut head = Vec::with_capacity(j);
                for index in 0..j {
                    head.push(p.pdf_paragraph_composition_at(v, index)?.share(v)?);
                }
                let label = p.layout_label(v)?.clone();
                let layout_id = p.layout_id(v)?;
                let debug_id = id(inputs.debug_ids)?;
                let n = page.pdf_paragraph_len(v)?;
                page.push_pdf_paragraph(
                    v,
                    ir::PdfParagraphInit {
                        r#box: Some(box_value(B {
                            x: Some(0.),
                            y: Some(0.),
                            x2: Some(0.),
                            y2: Some(0.),
                        })),
                        unicode: Some(String::new()),
                        debug_id: Some(debug_id),
                        layout_label: label,
                        layout_id,
                        pdf_paragraph_composition: tail,
                        ..Default::default()
                    }
                    .into_value(),
                )?;
                let new = page.pdf_paragraph_at(v, n)?.retain(v)?;
                p.set_pdf_paragraph_composition(v, head)?;
                update(&p, v, false)?;
                update(&new, v, false)?;
                paras.insert(i + 1, new);
                break;
            }
        }
        i += 1;
    }
    if inputs.config.merge_alternating_line_numbers.unwrap_or(true) {
        let mut i = 0;
        while i + 2 < paras.len() {
            let mut j = i + 1;
            while j < paras.len() {
                // Python's line-number predicate reads raw line/direct characters;
                // formula and post-translation compositions do not contribute.
                let mut is_line_number = true;
                for composition in paras[j].pdf_paragraph_composition_iter(v)? {
                    let composition = composition?;
                    let mut check_character = |character: C| -> Result<(), StageError> {
                        if !character
                            .char_unicode(v)?
                            .as_deref()
                            .unwrap_or_default()
                            .chars()
                            .all(|c| c.is_ascii_digit() || python_space(c))
                        {
                            is_line_number = false;
                        }
                        Ok(())
                    };
                    if let Some(line) = composition.pdf_line(v)? {
                        for character in line.pdf_character_iter(v)? {
                            check_character(character?)?;
                        }
                    } else if let Some(character) = composition.pdf_character(v)? {
                        check_character(character)?;
                    }
                }
                if is_line_number {
                    j += 1
                } else {
                    break;
                }
            }
            if j > i + 1
                && j < paras.len()
                && paras[i].layout_id(v)?.is_some()
                && paras[i].layout_id(v)? == paras[j].layout_id(v)?
                && paras[i].xobj_id(v)?.is_some()
                && paras[i].xobj_id(v)? == paras[j].xobj_id(v)?
            {
                let tail = paras.remove(j);
                for child_index in 0..tail.pdf_paragraph_composition_len(v)? {
                    let c = tail.pdf_paragraph_composition_at(v, child_index)?;
                    let value = c.share(v)?;
                    paras[i].push_pdf_paragraph_composition(v, value)?;
                }
                update(&paras[i], v, false)?;
            } else {
                i += 1;
            }
        }
    }
    let values = paras
        .iter()
        .map(|p| p.share(v))
        .collect::<Result<Vec<_>, _>>()?;
    page.set_pdf_paragraph(v, values)?;
    for p in &paras {
        update(p, v, true)?;
    }
    if inputs.config.ocr_workaround {
        let layouts = layouts(page, v)?;
        for p in &paras {
            let Some(layout_id) = p.layout_id(v)? else {
                continue;
            };
            let layout = layouts
                .iter()
                .find(|l| l.id == Some(layout_id))
                .ok_or_else(|| StageError::failed("layout ID lookup"))?;
            let Some(b) = p.r#box(v)? else { continue };
            let b = box_read(&b, v)?;
            let lb = need(layout.r#box.as_deref(), "layout.box")?;
            let combined = union(&[b, lb.clone()])?;
            let xobj = p.xobj_id(v)?;
            palette.rectangle(
                page,
                v,
                box_value(combined),
                &WHITE,
                true,
                false,
                None,
                xobj,
            )?;
        }
        page.set_pdf_character(v, vec![])?;
    }
    fix_overlap(&paras, page, v)?;
    for p in &paras {
        let mut order = 9_999_999_999_999_999_i64;
        for child_index in 0..p.pdf_paragraph_composition_len(v)? {
            let c = p.pdf_paragraph_composition_at(v, child_index)?;
            if inputs.config.debug {
                if let Some(l) = c.pdf_line(v)? {
                    let b = need(l.r#box(v)?, "line.box")?.share(v)?;
                    palette.rectangle(page, v, b, &INDIGO, false, true, Some(0.2), None)?;
                }
            }
            if c.pdf_line(v)?.is_some()
                || c.pdf_character(v)?.is_some()
                || c.pdf_formula(v)?.is_some()
            {
                for ch in chars(&c, v)? {
                    if let Some(o) = ch.render_order(v)? {
                        order = order.min(o);
                    }
                }
            }
        }
        if order != 9_999_999_999_999_999_i64 {
            p.set_render_order(v, Some(order))?;
        }
    }
    Ok(())
}
fn fix_overlap(paras: &[P], page: &Page, v: &mut View<'_>) -> Result<(), StageError> {
    let n = paras.len();
    if n < 2 {
        return Ok(());
    }
    let limit = n * n;
    let mut iterations = 0;
    while iterations < limit {
        iterations += 1;
        let mut found = false;
        for i in 0..n {
            for j in i + 1..n {
                let (Some(a), Some(b)) = (paras[i].r#box(v)?, paras[j].r#box(v)?) else {
                    continue;
                };
                let aa = box_read(&a, v)?;
                let bb = box_read(&b, v)?;
                if paras[i].xobj_id(v)? != paras[j].xobj_id(v)? {
                    continue;
                }
                let (ax, ay, ax2, ay2) = (
                    need(aa.x, "x")?,
                    need(aa.y, "y")?,
                    need(aa.x2, "x2")?,
                    need(aa.y2, "y2")?,
                );
                let (bx, by, bx2, by2) = (
                    need(bb.x, "x")?,
                    need(bb.y, "y")?,
                    need(bb.x2, "x2")?,
                    need(bb.y2, "y2")?,
                );
                if ax >= bx2
                    || ax2 <= bx
                    || ay >= by2
                    || ay2 <= by
                    || (ay <= by && ay2 >= by2)
                    || (by <= ay && by2 >= ay2)
                {
                    continue;
                }
                let ys = ay.max(by);
                let ye = ay2.min(by2);
                if ye - ys <= 1e-6 || ax2.min(bx2) - ax.max(bx) <= 1e-6 {
                    continue;
                }
                found = true;
                let hi = if ay2 > by && ay < by {
                    j
                } else if ay2 < by2 {
                    j
                } else {
                    i
                };
                let mid = ys + (ye - ys) / 2.;
                let (h, l) = if hi == i { (&a, &b) } else { (&b, &a) };
                if mid > h.y(v)?.unwrap_or(0.) && mid < l.y2(v)?.unwrap_or(0.) {
                    h.set_y(v, Some(mid + 1.))?;
                    l.set_y2(v, Some(mid - 1.))?;
                } else {
                    let higher = paras[hi].debug_id(v)?.as_deref().unwrap_or("None");
                    let lower = paras[if hi == i { j } else { i }]
                        .debug_id(v)?
                        .as_deref()
                        .unwrap_or("None");
                    let hb = box_read(h, v)?;
                    let lb = box_read(l, v)?;
                    warn!(target:LOG_TARGET,"Could not resolve overlap between paragraphs {higher} and {lower} using simple midpoint strategy. Midpoint: {mid}, Higher Box: {hb:?}, Lower Box: {lb:?}");
                }
            }
        }
        if !found {
            break;
        }
    }
    if iterations == limit {
        warn!(target:LOG_TARGET,"Maximum iterations ({iterations}) reached in fix_overlapping_paragraphs for page {}. Some overlaps might remain.",page.page_number(v)?.unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/support/paragraph_spacing_cases.rs"]
mod paragraph_spacing_cases;

#[cfg(test)]
#[path = "../../tests/support/synthetic_spacing_cases.rs"]
mod synthetic_spacing_cases;

#[cfg(test)]
#[path = "../../tests/support/paragraph_reading_cases.rs"]
mod paragraph_reading_cases;
