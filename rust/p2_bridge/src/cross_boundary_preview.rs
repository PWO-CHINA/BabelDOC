use babeldoc_p2_stages::layout_audit::Bounds;
use babeldoc_p2_stages::stages::typesetting::{PreviewUnit, TypesettingPreview};
use std::collections::{BTreeMap, HashSet};
use std::ops::Bound::{Excluded, Unbounded};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetPosition {
    pub page: usize,
    pub paragraph: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Glyph(usize),
    Formula(usize),
    FormulaCharacter(usize, usize),
    FormulaCurve(usize, usize),
    FormulaForm(usize, usize),
    UnknownUnit(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentPosition {
    pub target: TargetPosition,
    pub component: Component,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Overlap {
    pub first: ComponentPosition,
    pub first_bounds: Bounds,
    pub second: ComponentPosition,
    pub second_bounds: Bounds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uncertainty {
    NoTargets,
    DuplicateTarget(TargetPosition),
    EmptyPreview(TargetPosition),
    EmptyFormula(ComponentPosition),
    InconsistentFormulaCharacters(ComponentPosition),
    InconsistentUnits(TargetPosition),
    InvalidBounds(ComponentPosition),
    OverlapEvidenceLimitExceeded { limit: usize },
    ComparisonLimitExceeded { limit: usize },
}

pub const MAX_OVERLAP_EVIDENCE: usize = 4096;
pub const MAX_COMPONENT_COMPARISONS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectionCancelled;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlapReport {
    pub overlaps: Vec<Overlap>,
    pub uncertainties: Vec<Uncertainty>,
}

pub struct TargetPreview<'a> {
    pub target: TargetPosition,
    pub preview: &'a TypesettingPreview,
}

fn valid(bounds: Bounds) -> bool {
    [bounds.left, bounds.bottom, bounds.right, bounds.top].iter().all(|value| value.is_finite())
        && bounds.left < bounds.right && bounds.bottom < bounds.top
}

/// Reports only cross-target geometry; callers must also preserve all existing preview rejections.
pub fn inspect_target_overlaps(targets: &[TargetPreview<'_>]) -> OverlapReport {
    inspect_target_overlaps_with_cancellation(targets, || false).expect("non-cancelling inspection")
}

/// Cancellation returns no partial report; callers must treat it as a rejection.
pub fn inspect_target_overlaps_with_cancellation(
    targets: &[TargetPreview<'_>],
    mut cancelled: impl FnMut() -> bool,
) -> Result<OverlapReport, InspectionCancelled> {
    if cancelled() { return Err(InspectionCancelled); }
    let mut work = 0usize;
    macro_rules! check {
        () => {{
            work += 1;
            if work == 256 {
                work = 0;
                if cancelled() { return Err(InspectionCancelled); }
            }
        }};
    }
    let mut report = OverlapReport::default();
    if targets.is_empty() {
        report.uncertainties.push(Uncertainty::NoTargets);
    }
    let mut components = BTreeMap::new();
    let mut seen_targets = HashSet::new();
    let mut component_index = 0usize;
    for (target_index, item) in targets.iter().enumerate() {
        check!();
        if !seen_targets.insert((item.target.page, item.target.paragraph)) {
            report.uncertainties.push(Uncertainty::DuplicateTarget(item.target));
        }
        let preview = item.preview;
        if preview.glyphs.is_empty() && preview.formulas.is_empty() && preview.units.is_empty() {
            report.uncertainties.push(Uncertainty::EmptyPreview(item.target));
        }
        let mut glyph_index = 0;
        let mut formula_index = 0;
        let mut units_match = true;
        for unit in &preview.units {
            check!();
            match unit {
                PreviewUnit::Glyph(glyph) => {
                    units_match &= preview.glyphs.get(glyph_index).is_some_and(|expected| expected.bounds == glyph.bounds);
                    glyph_index += 1;
                }
                PreviewUnit::Formula { bounds } => {
                    units_match &= preview.formulas.get(formula_index).is_some_and(|expected| expected.bounds == *bounds);
                    formula_index += 1;
                }
                _ => {}
            }
        }
        if !units_match || glyph_index != preview.glyphs.len() || formula_index != preview.formulas.len() {
            report.uncertainties.push(Uncertainty::InconsistentUnits(item.target));
        }
        for (index, formula) in preview.formulas.iter().enumerate() {
            check!();
            let position = ComponentPosition { target: item.target, component: Component::Formula(index) };
            if formula.character_bounds.is_empty() && formula.curve_bounds.is_empty() && formula.form_bounds.is_empty() {
                report.uncertainties.push(Uncertainty::EmptyFormula(position));
            }
            if formula.character_source_identities.len() != formula.character_bounds.len() {
                report.uncertainties.push(Uncertainty::InconsistentFormulaCharacters(position));
            }
        }
        let mut add = |component, bounds: Option<Bounds>| {
            let position = ComponentPosition { target: item.target, component };
            match bounds.filter(|bounds| valid(*bounds)) {
                Some(bounds) => {
                    let bits = bounds.left.to_bits();
                    let ordered_left = if bits >> 63 == 1 { !bits } else { bits ^ (1 << 63) };
                    components.insert((item.target.page, ordered_left, component_index), (target_index, position, bounds));
                    component_index += 1;
                }
                None => report.uncertainties.push(Uncertainty::InvalidBounds(position)),
            }
        };
        for (index, glyph) in preview.glyphs.iter().enumerate() {
            check!();
            add(Component::Glyph(index), glyph.bounds);
        }
        for (index, formula) in preview.formulas.iter().enumerate() {
            check!();
            add(Component::Formula(index), formula.bounds);
            for (part, bounds) in formula.character_bounds.iter().enumerate() {
                check!();
                add(Component::FormulaCharacter(index, part), *bounds);
            }
            for (part, bounds) in formula.curve_bounds.iter().enumerate() {
                check!();
                add(Component::FormulaCurve(index, part), *bounds);
            }
            for (part, bounds) in formula.form_bounds.iter().enumerate() {
                check!();
                add(Component::FormulaForm(index, part), *bounds);
            }
        }
        for (index, unit) in preview.units.iter().enumerate() {
            check!();
            if matches!(unit, PreviewUnit::Unknown) {
                add(Component::UnknownUnit(index), None);
            }
        }
    }
    let mut evidence = Vec::new();
    let mut comparisons = 0usize;
    'scan: for (key, (first_target, first, first_bounds)) in &components {
        check!();
        for (second_key, (second_target, second, second_bounds)) in components.range((Excluded(*key), Unbounded)) {
            check!();
            if second_key.0 != key.0 || second_bounds.left >= first_bounds.right { break; }
            if comparisons == MAX_COMPONENT_COMPARISONS {
                report.uncertainties.push(Uncertainty::ComparisonLimitExceeded { limit: MAX_COMPONENT_COMPARISONS });
                break 'scan;
            }
            comparisons += 1;
            if first_target != second_target && first_bounds.overlaps(*second_bounds) {
                if evidence.len() == MAX_OVERLAP_EVIDENCE {
                    report.uncertainties.push(Uncertainty::OverlapEvidenceLimitExceeded { limit: MAX_OVERLAP_EVIDENCE });
                    break 'scan;
                }
                let (first_index, second_index, overlap) = if key.2 < second_key.2 {
                    (key.2, second_key.2, Overlap { first: *first, first_bounds: *first_bounds, second: *second, second_bounds: *second_bounds })
                } else {
                    (second_key.2, key.2, Overlap { first: *second, first_bounds: *second_bounds, second: *first, second_bounds: *first_bounds })
                };
                evidence.push((first_index, second_index, overlap));
            }
        }
    }
    evidence.sort_unstable_by_key(|(first, second, _)| (*first, *second));
    report.overlaps = evidence.into_iter().map(|(_, _, overlap)| overlap).collect();
    if cancelled() { return Err(InspectionCancelled); }
    Ok(report)
}
