use _rust_il::cross_boundary_preview::{inspect_target_overlaps, inspect_target_overlaps_with_cancellation, Component, InspectionCancelled, TargetPosition, TargetPreview, Uncertainty, MAX_COMPONENT_COMPARISONS, MAX_OVERLAP_EVIDENCE};
use babeldoc_p2_stages::layout_audit::Bounds;
use babeldoc_p2_stages::stages::typesetting::{PreviewFormula, PreviewGlyph, PreviewRejection, PreviewUnit, Typesetting, TypesettingPreview};
use babeldoc_p2_stages::{font_mapper::font_mapper_from_asset_paths, NoopCancellation, NoopProgress, StageContext};
use babeldoc_ir_core as ir;

fn bounds(left: f64, right: f64) -> Bounds {
    Bounds { left, right, bottom: 0.0, top: 10.0 }
}

fn glyph(bounds: Option<Bounds>) -> PreviewGlyph {
    PreviewGlyph { text: Some("A".into()), glyph_id: Some(1), font_size: Some(10.0), font_id: Some("test".into()), xobj_id: None, bounds }
}

fn preview(glyph_bounds: Option<Bounds>) -> TypesettingPreview {
    let glyph = glyph(glyph_bounds);
    TypesettingPreview { original_bounds: Some(bounds(0.0, 100.0)), glyphs: vec![glyph.clone()], units: vec![PreviewUnit::Glyph(glyph)], formulas: vec![], formula_identities_match: None, rejections: vec![PreviewRejection::ObstacleAndClipEvidenceUnavailable] }
}

fn target(page: usize, paragraph: usize, preview: &TypesettingPreview) -> TargetPreview<'_> {
    TargetPreview { target: TargetPosition { page, paragraph }, preview }
}

#[test]
fn dense_overlaps_report_limit_without_silent_truncation() {
    let item = preview(Some(bounds(0.0, 10.0)));
    let exact: Vec<_> = (0..4097).map(|index| target(if index < 2 { 0 } else { index }, index, &item)).collect();
    assert_eq!(inspect_target_overlaps(&exact).overlaps.len(), 1);
    let dense: Vec<_> = (0..100).map(|index| target(0, index, &item)).collect();
    let report = inspect_target_overlaps(&dense);
    assert_eq!(report.overlaps.len(), MAX_OVERLAP_EVIDENCE);
    assert!(report.uncertainties.contains(&Uncertainty::OverlapEvidenceLimitExceeded { limit: MAX_OVERLAP_EVIDENCE }));
}

#[test]
fn exactly_at_evidence_limit_is_not_uncertain() {
    let item = preview(Some(bounds(0.0, 10.0)));
    let mut left = item.clone();
    left.glyphs = vec![glyph(Some(bounds(0.0, 10.0))); 64];
    left.units = left.glyphs.iter().cloned().map(PreviewUnit::Glyph).collect();
    let report = inspect_target_overlaps(&[target(0, 0, &left), target(0, 1, &left)]);
    assert_eq!(report.overlaps.len(), MAX_OVERLAP_EVIDENCE);
    assert!(report.uncertainties.is_empty());
}

#[test]
fn sorted_scan_matches_brute_force_across_pages() {
    let previews: Vec<_> = (0..180).map(|index| {
        let left = ((index * 37) % 83) as f64 - 41.0;
        let mut rect = bounds(left, left + (index % 7 + 1) as f64);
        rect.bottom = (index % 9) as f64;
        rect.top = rect.bottom + 3.0;
        preview(Some(rect))
    }).collect();
    let targets: Vec<_> = previews.iter().enumerate().map(|(index, item)| target(index % 4, index, item)).collect();
    let report = inspect_target_overlaps(&targets);
    assert!(report.uncertainties.is_empty());
    let mut expected = Vec::new();
    for first in 0..targets.len() {
        for second in first + 1..targets.len() {
            if targets[first].target.page == targets[second].target.page && previews[first].glyphs[0].bounds.unwrap().overlaps(previews[second].glyphs[0].bounds.unwrap()) {
                expected.push((targets[first].target, targets[second].target));
            }
        }
    }
    assert_eq!(report.overlaps.iter().map(|overlap| (overlap.first.target, overlap.second.target)).collect::<Vec<_>>(), expected);
}

#[test]
fn handles_many_disjoint_components_and_cancels_during_scan() {
    let previews: Vec<_> = (0..12000).map(|index| preview(Some(bounds(index as f64 * 2.0, index as f64 * 2.0 + 1.0)))).collect();
    let targets: Vec<_> = previews.iter().enumerate().map(|(index, item)| target(0, index, item)).collect();
    let report = inspect_target_overlaps(&targets);
    assert!(report.overlaps.is_empty() && report.uncertainties.is_empty());
    let mut checks = 0;
    let result = inspect_target_overlaps_with_cancellation(&targets, || { checks += 1; checks == 200 });
    assert_eq!(result, Err(InspectionCancelled));
    assert_eq!(checks, 200);
    assert_eq!(inspect_target_overlaps_with_cancellation(&targets, || true), Err(InspectionCancelled));
}

#[test]
fn same_target_x_overlap_hits_comparison_limit_and_remains_cancellable() {
    let mut item = preview(Some(bounds(0.0, 10.0)));
    item.glyphs = (0..1500).map(|index| {
        let mut rect = bounds(0.0, 10.0);
        rect.bottom = index as f64 * 2.0;
        rect.top = rect.bottom + 1.0;
        glyph(Some(rect))
    }).collect();
    item.units = item.glyphs.iter().cloned().map(PreviewUnit::Glyph).collect();
    let targets = [target(0, 0, &item)];
    let report = inspect_target_overlaps(&targets);
    assert!(report.overlaps.is_empty());
    assert!(report.uncertainties.contains(&Uncertainty::ComparisonLimitExceeded { limit: MAX_COMPONENT_COMPARISONS }));
    let mut checks = 0;
    let result = inspect_target_overlaps_with_cancellation(&targets, || { checks += 1; checks == 50 });
    assert_eq!(result, Err(InspectionCancelled));
}

#[test]
fn compares_actual_components_and_reports_both_positions() {
    let first = preview(Some(bounds(0.0, 10.0)));
    let mut second = preview(Some(bounds(20.0, 30.0)));
    second.formulas.push(PreviewFormula { source_identity: None, character_source_identities: vec![None], bounds: Some(bounds(20.0, 30.0)), character_bounds: vec![Some(bounds(9.0, 11.0))], curve_bounds: vec![Some(bounds(8.0, 12.0))], form_bounds: vec![Some(bounds(7.0, 13.0))] });
    second.units.push(PreviewUnit::Formula { bounds: Some(bounds(20.0, 30.0)) });
    let report = inspect_target_overlaps(&[target(4, 2, &first), target(4, 7, &second)]);
    assert!(report.uncertainties.is_empty(), "{report:?}");
    assert_eq!(report.overlaps.len(), 3);
    for (overlap, component) in report.overlaps.iter().zip([Component::FormulaCharacter(0, 0), Component::FormulaCurve(0, 0), Component::FormulaForm(0, 0)]) {
        assert_eq!(overlap.first.target, TargetPosition { page: 4, paragraph: 2 });
        assert_eq!(overlap.first.component, Component::Glyph(0));
        assert_eq!(overlap.second.target, TargetPosition { page: 4, paragraph: 7 });
        assert_eq!(overlap.second.component, component);
    }
    assert!(first.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
}

#[test]
fn permits_edge_contact_cross_page_and_same_target_components() {
    let first = preview(Some(bounds(0.0, 10.0)));
    let touching = preview(Some(bounds(10.0, 20.0)));
    let other_page = preview(Some(bounds(0.0, 10.0)));
    let report = inspect_target_overlaps(&[target(0, 0, &first), target(0, 1, &touching), target(1, 0, &other_page)]);
    assert!(report.overlaps.is_empty());
    assert!(report.uncertainties.is_empty());
}

#[test]
fn missing_invalid_empty_and_unknown_evidence_is_uncertain() {
    assert!(inspect_target_overlaps(&[]).uncertainties.contains(&Uncertainty::NoTargets));
    for invalid in [None, Some(bounds(5.0, 5.0)), Some(bounds(f64::NAN, 10.0)), Some(bounds(f64::NEG_INFINITY, 10.0))] {
        let item = preview(invalid);
        let report = inspect_target_overlaps(&[target(0, 0, &item)]);
        assert!(report.uncertainties.iter().any(|item| matches!(item, Uncertainty::InvalidBounds(_))));
    }
    let mut item = preview(Some(bounds(0.0, 1.0)));
    item.formulas.push(PreviewFormula { source_identity: None, character_source_identities: vec![], bounds: None, character_bounds: vec![None], curve_bounds: vec![None], form_bounds: vec![None] });
    item.units.push(PreviewUnit::Formula { bounds: None });
    item.units.push(PreviewUnit::Unknown);
    let report = inspect_target_overlaps(&[target(0, 0, &item), target(0, 0, &item)]);
    assert!(report.uncertainties.contains(&Uncertainty::DuplicateTarget(TargetPosition { page: 0, paragraph: 0 })));
    assert_eq!(report.uncertainties.iter().filter(|item| matches!(item, Uncertainty::InvalidBounds(_))).count(), 10);
    item.glyphs.clear(); item.formulas.clear(); item.units.clear();
    assert!(inspect_target_overlaps(&[target(0, 0, &item)]).uncertainties.contains(&Uncertainty::EmptyPreview(TargetPosition { page: 0, paragraph: 0 })));
}

#[test]
fn compares_real_typesetting_fork_previews() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file());
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1) }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let make_paragraph = || {
            let rect = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(50.0), y2: Some(30.0) }.into_value();
            ir::PdfParagraphInit { r#box: Some(rect()), unicode: Some("A".into()), pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit { pdf_line: Some(ir::PdfLineInit { r#box: Some(rect()), pdf_character: vec![ir::PdfCharacterInit { char_unicode: Some("A".into()), pdf_character_id: Some(36), r#box: Some(rect()), pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()), ..Default::default() }.into_value()], ..Default::default() }.into_value()), ..Default::default() }.into_value()], ..Default::default() }.into_value()
        };
        page.set_pdf_paragraph(&mut view, vec![make_paragraph(), make_paragraph()]).unwrap();
    }
    let before = document.artifact().unwrap();
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let previews = stage.audit_fork(&document, &[(0, 0), (0, 1)], &mut context).unwrap();
    let report = inspect_target_overlaps(&[target(0, 0, &previews[0]), target(0, 1, &previews[1])]);
    assert!(report.overlaps.iter().any(|item| item.first.component == Component::Glyph(0) && item.second.component == Component::Glyph(0)), "{report:?}");
    assert!(previews.iter().all(|item| item.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable)));
    assert_eq!(before, document.artifact().unwrap());
}

#[test]
fn real_mixed_formula_previews_match_overlap_units() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file());
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for relocate in [false, true] {
        let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1) }));
        let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
        {
            let mut view = document.view::<ir::ParagraphFinderView>();
            let page = view.document().page_at(&view, 0).unwrap();
            let rect = || ir::BoxInit { x: Some(20.0), y: Some(110.0), x2: Some(25.0), y2: Some(120.0) }.into_value();
            let style = || ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value();
            let character = || ir::PdfCharacterInit { char_unicode: Some("x".into()), pdf_character_id: Some(91), r#box: Some(rect()), visual_bbox: Some(ir::VisualBboxInit { r#box: Some(rect()) }.into_value()), pdf_style: Some(style()), ..Default::default() }.into_value();
            let formula = || ir::PdfParagraphCompositionInit { pdf_formula: Some(ir::PdfFormulaInit {
                r#box: Some(rect()), x_offset: Some(0.0), y_offset: Some(0.0), x_advance: Some(5.0),
                pdf_character: vec![character()], pdf_curve: vec![ir::PdfCurveInit { r#box: Some(rect()), ..Default::default() }.into_value()],
                pdf_form: vec![ir::PdfFormInit { r#box: Some(rect()), ..Default::default() }.into_value()], ..Default::default()
            }.into_value()), ..Default::default() }.into_value();
            let text = || if relocate {
                ir::PdfParagraphCompositionInit { pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit { unicode: Some("A".into()), pdf_style: Some(style()), ..Default::default() }.into_value()), ..Default::default() }.into_value()
            } else {
                ir::PdfParagraphCompositionInit { pdf_line: Some(ir::PdfLineInit { pdf_character: vec![character()], ..Default::default() }.into_value()), ..Default::default() }.into_value()
            };
            page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
                r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(100.0), x2: Some(200.0), y2: Some(140.0) }.into_value()),
                unicode: Some("AxAxA".into()), pdf_style: Some(style()), debug_id: Some("synthetic".into()),
                pdf_paragraph_composition: vec![text(), formula(), text(), formula(), text()], ..Default::default()
            }.into_value()]).unwrap();
        }
        let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
        let (previews, fork) = stage.audit_fork_with_document(&document, &[(0, 0)], &mut context).unwrap();
        assert!(fork.is_some(), "relocate={relocate}: {previews:?}");
        let item = &previews[0];
        assert_eq!(item.formulas.len(), 2);
        assert_eq!(item.glyphs.len(), 3);
        let other = preview(item.formulas[0].character_bounds[0]);
        let report = inspect_target_overlaps(&[target(0, 0, item), target(0, 1, &other)]);
        assert!(!report.uncertainties.contains(&Uncertainty::InconsistentUnits(TargetPosition { page: 0, paragraph: 0 })), "relocate={relocate}: {report:?}");
        for component in [Component::FormulaCharacter(0, 0), Component::FormulaCurve(0, 0), Component::FormulaForm(0, 0)] {
            assert!(report.overlaps.iter().any(|overlap| overlap.first.component == component || overlap.second.component == component), "relocate={relocate}: {component:?} {report:?}");
        }
        assert!(item.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
        assert!(item.rejections.contains(&PreviewRejection::GlyphResourceUnavailable));
        assert!(item.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
        let mut missing = item.clone();
        missing.units.retain(|unit| !matches!(unit, PreviewUnit::Formula { .. }));
        assert!(inspect_target_overlaps(&[target(0, 0, &missing)]).uncertainties.contains(&Uncertainty::InconsistentUnits(TargetPosition { page: 0, paragraph: 0 })));
        let mut unknown = item.clone();
        unknown.units.push(PreviewUnit::Unknown);
        assert!(inspect_target_overlaps(&[target(0, 0, &unknown)]).uncertainties.iter().any(|uncertainty| matches!(uncertainty, Uncertainty::InvalidBounds(_))));
        let mut wrong_order = item.clone();
        wrong_order.formulas[1].bounds = Some(bounds(-10.0, -5.0));
        wrong_order.units[3] = PreviewUnit::Formula { bounds: wrong_order.formulas[1].bounds };
        wrong_order.units.swap(1, 3);
        assert!(inspect_target_overlaps(&[target(0, 0, &wrong_order)]).uncertainties.contains(&Uncertainty::InconsistentUnits(TargetPosition { page: 0, paragraph: 0 })));
    }
}
