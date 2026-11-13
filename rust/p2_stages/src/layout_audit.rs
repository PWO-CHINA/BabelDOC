use std::collections::HashSet;
use babeldoc_ir_core::{self as ir, Document, ReadOnlyView, TreeView};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl Bounds {
    fn valid(self) -> bool {
        [self.left, self.bottom, self.right, self.top].iter().all(|value| value.is_finite())
            && self.left < self.right && self.bottom < self.top
    }

    fn contains(self, other: Self) -> bool {
        self.left <= other.left && self.bottom <= other.bottom
            && self.right >= other.right && self.top >= other.top
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.left < other.right && other.left < self.right
            && self.bottom < other.top && other.bottom < self.top
    }
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub text: Option<String>,
    pub glyph_id: Option<u32>,
    pub font_size: Option<f64>,
    pub bounds: Option<Bounds>,
}

#[derive(Clone, Debug)]
pub struct Formula {
    pub id: String,
    pub bounds: Option<Bounds>,
}

#[derive(Clone, Debug)]
pub struct NeighborEvidence {
    pub complete: bool,
    pub bounds: Vec<Option<Bounds>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObstacleKind { Paragraph, Character, Figure, Rectangle, Curve, Form }

#[derive(Clone, Debug, PartialEq)]
pub struct ObservedObstacle {
    pub kind: ObstacleKind,
    pub index: usize,
    pub bounds: Option<Bounds>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObstacleUncertainty {
    MissingOrInvalidCropBox,
    MissingTarget,
    MissingOrInvalidTargetBox,
    TargetOutsideCropBox,
    MissingOrInvalidObstacleBox { kind: ObstacleKind, index: usize },
    CoordinateSpaceAndPaintExtentUnverified,
    PageAndXobjectOperationsUnexamined,
    GraphicStateAndClippingUnexamined,
    IrCollectionCompletenessUnproven,
}

#[derive(Clone, Debug)]
pub struct PageObstacleReport {
    pub page_identity: u64,
    pub target_identity: Option<u64>,
    pub target_xobject: Option<i64>,
    pub crop_box: Option<Bounds>,
    pub target_box: Option<Bounds>,
    pub obstacles: Vec<ObservedObstacle>,
    pub uncertainties: Vec<ObstacleUncertainty>,
}

impl PageObstacleReport {
    pub fn neighbor_evidence(&self) -> NeighborEvidence {
        NeighborEvidence { complete: self.uncertainties.is_empty(), bounds: self.obstacles.iter().map(|item| item.bounds).collect() }
    }

    pub fn matches_fork_target(&self, candidate: &Self) -> bool {
        self.page_identity == candidate.page_identity
            && self.target_identity.is_some()
            && self.target_identity == candidate.target_identity
            && self.target_xobject == candidate.target_xobject
            && self.crop_box.is_some()
            && self.crop_box == candidate.crop_box
            && self.target_box.is_some()
            && self.target_box == candidate.target_box
    }
}

fn ir_bounds(value: Option<ir::Cursor<ir::types::Box>>, view: &TreeView<'_, ReadOnlyView>) -> Result<Option<Bounds>, ir::AccessError> {
    let Some(value) = value else { return Ok(None) };
    let (Some(left), Some(bottom), Some(right), Some(top)) =
        (value.x(view)?, value.y(view)?, value.x2(view)?, value.y2(view)?) else { return Ok(None) };
    let bounds = Bounds { left, bottom, right, top };
    Ok(bounds.valid().then_some(bounds))
}

/// Collects inspectable IR boxes, but does not establish a complete paint or clip map.
/// Call on the original document before target replacement; a report is never a safety proof.
pub fn inspect_page_obstacles(document: &mut Document, page_index: usize, target_index: usize) -> Result<PageObstacleReport, ir::AccessError> {
    let view = document.view::<ReadOnlyView>();
    let page = view.document().page_at(&view, page_index)?;
    let crop_box = match page.cropbox(&view)? {
        Some(crop) => ir_bounds(crop.r#box(&view)?, &view)?,
        None => None,
    };
    let mut report = PageObstacleReport {
        page_identity: page.identity(), target_identity: None, target_xobject: None,
        crop_box, target_box: None, obstacles: Vec::new(),
        uncertainties: vec![ObstacleUncertainty::CoordinateSpaceAndPaintExtentUnverified,
            ObstacleUncertainty::PageAndXobjectOperationsUnexamined,
            ObstacleUncertainty::GraphicStateAndClippingUnexamined,
            ObstacleUncertainty::IrCollectionCompletenessUnproven],
    };
    if crop_box.is_none() { report.uncertainties.push(ObstacleUncertainty::MissingOrInvalidCropBox); }
    let mut target_found = false;
    macro_rules! collect {
        ($iter:expr, $kind:expr) => {
            for (index, item) in $iter?.enumerate() {
                let item = item?;
                let bounds = ir_bounds(item.r#box(&view)?, &view)?;
                if bounds.is_none() { report.uncertainties.push(ObstacleUncertainty::MissingOrInvalidObstacleBox { kind: $kind, index }); }
                report.obstacles.push(ObservedObstacle { kind: $kind, index, bounds });
            }
        };
    }
    for (index, paragraph) in page.pdf_paragraph_iter(&view)?.enumerate() {
        let paragraph = paragraph?;
        let bounds = ir_bounds(paragraph.r#box(&view)?, &view)?;
        if index == target_index {
            target_found = true;
            report.target_identity = Some(paragraph.identity());
            report.target_xobject = paragraph.xobj_id(&view)?;
            report.target_box = bounds;
            if bounds.is_none() { report.uncertainties.push(ObstacleUncertainty::MissingOrInvalidTargetBox); }
            if crop_box.zip(bounds).is_some_and(|(crop, target)| !crop.contains(target)) { report.uncertainties.push(ObstacleUncertainty::TargetOutsideCropBox); }
        } else {
            if bounds.is_none() { report.uncertainties.push(ObstacleUncertainty::MissingOrInvalidObstacleBox { kind: ObstacleKind::Paragraph, index }); }
            report.obstacles.push(ObservedObstacle { kind: ObstacleKind::Paragraph, index, bounds });
        }
    }
    if !target_found { report.uncertainties.push(ObstacleUncertainty::MissingTarget); }
    collect!(page.pdf_character_iter(&view), ObstacleKind::Character);
    collect!(page.pdf_figure_iter(&view), ObstacleKind::Figure);
    collect!(page.pdf_rectangle_iter(&view), ObstacleKind::Rectangle);
    collect!(page.pdf_curve_iter(&view), ObstacleKind::Curve);
    collect!(page.pdf_form_iter(&view), ObstacleKind::Form);
    Ok(report)
}

#[derive(Clone, Debug)]
pub enum TextPart {
    Glyph(Glyph),
    SuppressedWhitespace { text: String, confirmed_layout_suppression: bool },
    Formula(Formula),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectedPart {
    Text(String),
    Formula(String),
}

#[derive(Clone, Debug)]
pub struct AuditInput {
    pub page_bounds: Option<Bounds>,
    pub expected_parts: Option<Vec<ExpectedPart>>,
    pub text_parts: Option<Vec<TextPart>>,
    pub neighbors: Option<NeighborEvidence>,
    pub minimum_font_size: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    InvalidMinimumFontSize,
    MissingOrInvalidPageBounds,
    MissingExpectedText,
    InvalidFormulaIdentity,
    FormulaMismatch,
    MissingGlyphs,
    MissingGlyphText(usize),
    MissingGlyph(usize),
    InvalidFontSize(usize),
    MissingOrInvalidGlyphBounds(usize),
    GlyphOutsidePage(usize),
    MissingNeighbors,
    MissingOrInvalidNeighborBounds(usize),
    IncompleteNeighbors,
    NeighborOverlap { glyph: usize, neighbor: usize },
    UnprovenWhitespace(usize),
    MissingOrInvalidFormulaBounds(usize),
    FormulaOutsidePage(usize),
    FormulaNeighborOverlap { formula: usize, neighbor: usize },
    FormulaUnitOverlap { formula: usize, other: usize },
    TextMismatch,
}

pub fn audit(input: &AuditInput) -> Result<(), Vec<Rejection>> {
    let mut reasons = Vec::new();
    if !input.minimum_font_size.is_finite() || input.minimum_font_size <= 0.0 {
        reasons.push(Rejection::InvalidMinimumFontSize);
    }
    let page = input.page_bounds.filter(|bounds| bounds.valid());
    if page.is_none() { reasons.push(Rejection::MissingOrInvalidPageBounds); }
    let expected = input.expected_parts.as_ref().filter(|parts| !parts.is_empty());
    if expected.is_none() { reasons.push(Rejection::MissingExpectedText); }
    let neighbors = input.neighbors.as_ref();
    if neighbors.is_none() { reasons.push(Rejection::MissingNeighbors); }
    if let Some(neighbors) = neighbors {
        if !neighbors.complete { reasons.push(Rejection::IncompleteNeighbors); }
        for (index, bounds) in neighbors.bounds.iter().enumerate() {
            if !bounds.is_some_and(Bounds::valid) { reasons.push(Rejection::MissingOrInvalidNeighborBounds(index)); }
        }
    }
    if let Some(parts) = &input.text_parts {
        for (index, formula) in parts.iter().enumerate().filter_map(|(index, part)| match part { TextPart::Formula(formula) => Some((index, formula)), _ => None }) {
            match formula.bounds.filter(|bounds| bounds.valid()) {
                None => reasons.push(Rejection::MissingOrInvalidFormulaBounds(index)),
                Some(bounds) => {
                    if page.is_some_and(|page| !page.contains(bounds)) { reasons.push(Rejection::FormulaOutsidePage(index)); }
                    if let Some(neighbors) = neighbors {
                        for (neighbor, other) in neighbors.bounds.iter().enumerate() {
                            if other.is_some_and(|other| other.valid() && bounds.overlaps(other)) {
                                reasons.push(Rejection::FormulaNeighborOverlap { formula: index, neighbor });
                            }
                        }
                    }
                    for (other_index, other) in parts.iter().enumerate() {
                        if other_index == index || (other_index < index && matches!(other, TextPart::Formula(_))) { continue; }
                        let other_bounds = match other { TextPart::Glyph(glyph) => glyph.bounds, TextPart::Formula(formula) => formula.bounds, TextPart::SuppressedWhitespace { .. } => None };
                        if other_bounds.is_some_and(|other| other.valid() && bounds.overlaps(other)) {
                            reasons.push(Rejection::FormulaUnitOverlap { formula: index, other: other_index });
                        }
                    }
                }
            }
        }
    }
    match &input.text_parts {
        None => reasons.push(Rejection::MissingGlyphs),
        Some(parts) => {
            let mut observed = Vec::new();
            let mut text = String::new();
            if parts.is_empty() { reasons.push(Rejection::MissingGlyphs); }
            for (index, part) in parts.iter().enumerate() {
                let glyph = match part {
                    TextPart::Glyph(glyph) => glyph,
                    TextPart::SuppressedWhitespace { text: whitespace, confirmed_layout_suppression } => {
                        if !confirmed_layout_suppression || whitespace.is_empty() || !whitespace.chars().all(char::is_whitespace) {
                            reasons.push(Rejection::UnprovenWhitespace(index));
                        }
                        text.push_str(whitespace);
                        continue;
                    }
                    TextPart::Formula(formula) => {
                        if !text.is_empty() { observed.push(ExpectedPart::Text(std::mem::take(&mut text))); }
                        observed.push(ExpectedPart::Formula(formula.id.clone()));
                        continue;
                    }
                };
                match glyph.text.as_deref().filter(|value| !value.is_empty()) {
                    Some(value) => text.push_str(value),
                    None => reasons.push(Rejection::MissingGlyphText(index)),
                }
                if glyph.glyph_id.is_none_or(|id| id == 0) { reasons.push(Rejection::MissingGlyph(index)); }
                if glyph.font_size.is_none_or(|size| !size.is_finite() || size < input.minimum_font_size || size <= 0.0) {
                    reasons.push(Rejection::InvalidFontSize(index));
                }
                match glyph.bounds.filter(|bounds| bounds.valid()) {
                    None => reasons.push(Rejection::MissingOrInvalidGlyphBounds(index)),
                    Some(bounds) => {
                        if page.is_some_and(|page| !page.contains(bounds)) { reasons.push(Rejection::GlyphOutsidePage(index)); }
                        if let Some(neighbors) = neighbors {
                            for (neighbor, other) in neighbors.bounds.iter().enumerate() {
                                if other.is_some_and(|other| other.valid() && bounds.overlaps(other)) {
                                    reasons.push(Rejection::NeighborOverlap { glyph: index, neighbor });
                                }
                            }
                        }
                    }
                }
            }
            if !text.is_empty() { observed.push(ExpectedPart::Text(text)); }
            let normalize = |parts: &[ExpectedPart]| {
                let mut result = Vec::new();
                for part in parts {
                    match part {
                        ExpectedPart::Text(value) if value.is_empty() => {},
                        ExpectedPart::Text(value) => match result.last_mut() {
                            Some(ExpectedPart::Text(existing)) => existing.push_str(value),
                            _ => result.push(ExpectedPart::Text(value.clone())),
                        },
                        ExpectedPart::Formula(id) => result.push(ExpectedPart::Formula(id.clone())),
                    }
                }
                result
            };
            let ids = |parts: &[ExpectedPart]| parts.iter().filter_map(|part| match part { ExpectedPart::Formula(id) => Some(id.clone()), _ => None }).collect::<Vec<_>>();
            let rendered_ids = ids(&observed);
            if rendered_ids.iter().any(|id| id.is_empty()) || rendered_ids.iter().collect::<HashSet<_>>().len() != rendered_ids.len() { reasons.push(Rejection::InvalidFormulaIdentity); }
            if let Some(expected) = expected {
                let expected_ids = ids(expected);
                if expected_ids.iter().any(|id| id.is_empty()) || expected_ids.iter().collect::<HashSet<_>>().len() != expected_ids.len() { reasons.push(Rejection::InvalidFormulaIdentity); }
                if expected_ids != rendered_ids { reasons.push(Rejection::FormulaMismatch); }
                if normalize(expected) != observed { reasons.push(Rejection::TextMismatch); }
            }
        }
    }
    if reasons.is_empty() { Ok(()) } else { Err(reasons) }
}
