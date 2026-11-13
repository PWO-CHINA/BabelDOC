use babeldoc_ir_core as ir;
use babeldoc_p2_stages::{font_mapper::font_mapper_from_asset_paths, stages::typesetting::{PreviewRejection, Typesetting}, NoopCancellation, NoopProgress, StageContext};

struct Cancelled;
impl babeldoc_p2_stages::Cancellation for Cancelled { fn is_cancelled(&self) -> bool { true } }

#[derive(Default)]
struct CancelAfterFirstProgress(std::sync::atomic::AtomicBool);
impl babeldoc_p2_stages::Cancellation for CancelAfterFirstProgress {
    fn is_cancelled(&self) -> bool { self.0.load(std::sync::atomic::Ordering::SeqCst) }
}
impl babeldoc_p2_stages::ProgressReporter for CancelAfterFirstProgress {
    type Error = babeldoc_p2_stages::StageError;
    fn report(&self, _: u64, _: Option<u64>) -> Result<(), Self::Error> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst); Ok(())
    }
}

struct FailedProgress;
impl babeldoc_p2_stages::ProgressReporter for FailedProgress {
    type Error = babeldoc_p2_stages::StageError;
    fn report(&self, _: u64, _: Option<u64>) -> Result<(), Self::Error> {
        Err(babeldoc_p2_stages::StageError::Failed("test progress failure".into()))
    }
}

#[test]
fn preview_runs_on_fork_and_never_claims_unverified_safety() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    if !std::path::Path::new(font).is_file() { return; }
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let paths = serde_json::json!({"test":font});
    let mapper = font_mapper_from_asset_paths(&family, &paths, None).unwrap();
    let stage = Typesetting::new("zh", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1),
    }));
    let document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    let before = document.artifact().unwrap();
    let progress = CancelAfterFirstProgress::default();
    let mut empty_context = StageContext::new(&progress, &progress);
    assert!(stage.audit_fork(&document, &[], &mut empty_context).unwrap().is_empty());
    assert!(!progress.0.load(std::sync::atomic::Ordering::SeqCst));
    let (empty, fork) = stage.audit_fork_with_document(&document, &[], &mut empty_context).unwrap();
    assert!(empty.is_empty());
    assert!(fork.is_none());
    assert!(!progress.0.load(std::sync::atomic::Ordering::SeqCst));
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let previews = stage.audit_fork(&document, &[(0, 0), (0, 1)], &mut context).unwrap();
    assert_eq!(previews.len(), 2);
    let preview = &previews[0];
    assert!(preview.rejections.contains(&PreviewRejection::TargetMissing));
    assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
    assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
    assert_eq!(before, document.artifact().unwrap());
    let mut cancelled = StageContext::new(&Cancelled, &NoopProgress);
    assert!(matches!(stage.audit_fork(&document, &[(0, 0)], &mut cancelled), Err(babeldoc_p2_stages::StageError::Cancelled)));
    assert!(matches!(stage.audit_fork_with_document(&document, &[(0, 0)], &mut cancelled), Err(babeldoc_p2_stages::StageError::Cancelled)));
}

#[test]
fn captures_target_glyph_on_fork_without_mutating_original() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    if !std::path::Path::new(font).is_file() { return; }
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let bounds = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(50.0), y2: Some(30.0) }.into_value();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(bounds()), unicode: Some("A".into()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit { r#box: Some(bounds()), pdf_character: vec![ir::PdfCharacterInit {
                    char_unicode: Some("A".into()), pdf_character_id: Some(36), r#box: Some(bounds()),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()), ..Default::default()
                }.into_value()], ..Default::default() }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()]).unwrap();
    }
    let before = document.artifact().unwrap();
    let progress = CancelAfterFirstProgress::default();
    let mut interrupted = StageContext::new(&progress, &progress);
    assert!(matches!(stage.audit_fork(&document, &[(0, 0)], &mut interrupted), Err(babeldoc_p2_stages::StageError::Cancelled)));
    assert_eq!(before, document.artifact().unwrap());
    let progress = CancelAfterFirstProgress::default();
    let mut interrupted = StageContext::new(&progress, &progress);
    assert!(matches!(stage.audit_fork_with_document(&document, &[(0, 0)], &mut interrupted), Err(babeldoc_p2_stages::StageError::Cancelled)));
    let mut failed = StageContext::new(&NoopCancellation, &FailedProgress);
    let (failed_previews, failed_fork) = stage.audit_fork_with_document(&document, &[(0, 0)], &mut failed).unwrap();
    assert!(failed_previews[0].rejections.contains(&PreviewRejection::TypesettingFailed));
    assert!(failed_fork.is_none());
    assert_eq!(before, document.artifact().unwrap());
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let (previews, fork) = stage.audit_fork_with_document(&document, &[(0, 0), (0, 0), (0, 1)], &mut context).unwrap();
    assert_eq!(previews.len(), 3);
    assert_eq!(previews[1].glyphs.len(), 1);
    assert!(previews[2].rejections.contains(&PreviewRejection::TargetMissing));
    let preview = &previews[0];
    assert_eq!(preview.glyphs.len(), 1);
    assert_eq!(preview.glyphs[0].text.as_deref(), Some("A"));
    assert_eq!(preview.glyphs[0].font_id.as_deref(), Some("test"));
    assert!(!preview.rejections.contains(&PreviewRejection::TextMismatch));
    assert!(!preview.rejections.contains(&PreviewRejection::TextEvidenceUnavailable));
    assert_eq!(preview.original_bounds.unwrap().right, 50.0);
    assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
    assert!(fork.is_none());
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let (_, fork) = stage.audit_fork_with_document(&document, &[(0, 0), (0, 0)], &mut context).unwrap();
    let artifact = fork.unwrap().artifact().unwrap();
    let character = &artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_character"];
    assert_eq!(character["char_unicode"], preview.glyphs[0].text.as_deref().unwrap());
    assert_eq!(character["pdf_character_id"].as_i64(), preview.glyphs[0].glyph_id);
    assert_ne!(before, artifact);
    assert_eq!(before, document.artifact().unwrap());
}

#[test]
fn preview_rejects_text_dropped_by_font_selection() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let bounds = ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(100.0), y2: Some(30.0) }.into_value();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(bounds), unicode: Some("A\u{10ffff}".into()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some("A\u{10ffff}".into()),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()),
                    ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()]).unwrap();
    }
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let preview = stage.audit_fork(&document, &[(0, 0)], &mut context).unwrap().remove(0);
    assert!(preview.rejections.contains(&PreviewRejection::TypesettingFailed), "{preview:?}");
}

#[test]
fn preview_rejects_glyph_outside_original_target() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let target = ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(11.0), y2: Some(11.0) }.into_value();
        let glyph = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(50.0), y2: Some(30.0) }.into_value();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(target), unicode: Some("A".into()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit { r#box: Some(glyph()), pdf_character: vec![ir::PdfCharacterInit {
                    char_unicode: Some("A".into()), pdf_character_id: Some(36), r#box: Some(glyph()),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()), ..Default::default()
                }.into_value()], ..Default::default() }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()]).unwrap();
    }
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let preview = stage.audit_fork(&document, &[(0, 0)], &mut context).unwrap().remove(0);
    assert!(preview.rejections.contains(&PreviewRejection::OutsideOriginalTargetBounds), "{preview:?}");
}

#[test]
fn preview_tracks_formula_identity_and_components_during_relocation() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for (missing_visual, curve_overflow, missing_curve, duplicate_character) in [(false, false, false, false), (true, false, false, false), (false, true, false, false), (false, false, true, false), (false, false, false, true)] {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))], total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let target = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(200.0), y2: Some(50.0) }.into_value();
        let formula_box = || ir::BoxInit { x: Some(20.0), y: Some(20.0), x2: Some(25.0), y2: Some(30.0) }.into_value();
        let style = || ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value();
        let formula = ir::PdfFormulaInit {
            r#box: Some(formula_box()), x_offset: Some(0.0), y_offset: Some(0.0), x_advance: Some(5.0),
            pdf_character: (0..2).map(|_| ir::PdfCharacterInit {
                char_unicode: Some("x".into()), pdf_character_id: Some(91), r#box: Some(formula_box()),
                visual_bbox: (!missing_visual).then(|| ir::VisualBboxInit { r#box: Some(formula_box()) }.into_value()),
                pdf_style: Some(style()), vertical: Some(false), ..Default::default()
            }.into_value()).collect(),
            pdf_curve: vec![ir::PdfCurveInit {
                r#box: (!missing_curve).then(|| if curve_overflow {
                    ir::BoxInit { x: Some(500.0), y: Some(20.0), x2: Some(505.0), y2: Some(30.0) }.into_value()
                } else { formula_box() }),
                ..Default::default()
            }.into_value()],
            pdf_form: vec![ir::PdfFormInit { r#box: Some(formula_box()), ..Default::default() }.into_value()],
            ..Default::default()
        }.into_value();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(target()), unicode: Some("Ax".into()), pdf_style: Some(style()), debug_id: Some("synthetic".into()),
            pdf_paragraph_composition: vec![
                ir::PdfParagraphCompositionInit { pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some("A".into()), pdf_style: Some(style()), ..Default::default()
                }.into_value()), ..Default::default() }.into_value(),
                ir::PdfParagraphCompositionInit { pdf_formula: Some(formula), ..Default::default() }.into_value(),
            ],
            ..Default::default()
        }.into_value()]).unwrap();
    }
    if duplicate_character {
        let mut view = document.view::<ir::TypesettingView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let paragraph = page.pdf_paragraph_at(&view, 0).unwrap();
        let composition = paragraph.pdf_paragraph_composition_at(&view, 1).unwrap();
        let formula = composition.pdf_formula(&view).unwrap().unwrap();
        let character = formula.pdf_character_at(&view, 0).unwrap().retain(&mut view).unwrap();
        let first = character.share(&mut view).unwrap();
        let second = character.share(&mut view).unwrap();
        formula.set_pdf_character(&mut view, vec![first, second]).unwrap();
    }
    let before = document.artifact().unwrap();
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let preview = stage.audit_fork(&document, &[(0, 0)], &mut context).unwrap().remove(0);
    if duplicate_character {
        assert_eq!(preview.formula_identities_match, Some(false), "{preview:?}");
        assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityMismatch));
        assert_eq!(before, document.artifact().unwrap());
        continue;
    }
    if missing_visual {
        assert_ne!(preview.formula_identities_match, Some(true), "{preview:?}");
        assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
        assert_eq!(before, document.artifact().unwrap());
        continue;
    }
    assert_eq!(preview.formula_identities_match, Some(true), "{preview:?}");
    assert_eq!(preview.formulas.len(), 1, "{preview:?}");
    let formula = &preview.formulas[0];
    assert!(formula.source_identity.is_some());
    assert!(formula.bounds.is_some());
    assert_eq!(formula.character_bounds.len(), 2);
    assert_eq!(formula.character_source_identities.len(), 2);
    assert!(formula.character_source_identities.iter().all(Option::is_some));
    assert_ne!(formula.character_source_identities[0], formula.character_source_identities[1]);
    assert_eq!(formula.curve_bounds.len(), 1);
    assert_eq!(formula.form_bounds.len(), 1);
    assert!(formula.character_bounds[0].is_some());
    assert_eq!(formula.curve_bounds[0].is_none(), missing_curve);
    assert!(formula.form_bounds[0].is_some());
    assert_eq!(preview.rejections.contains(&PreviewRejection::OutsideOriginalTargetBounds), curve_overflow);
    assert_eq!(preview.rejections.contains(&PreviewRejection::InvalidFormulaEvidence), missing_curve);
    assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
    assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
    assert_eq!(before, document.artifact().unwrap());
    }
}

#[derive(Clone, Copy, Debug)]
enum ReusedFormulaComponent {
    Curve,
    Form,
}

#[derive(Clone, Copy, Debug)]
enum FormulaComponentSharing {
    None,
    WithinFormula,
    WithinParagraph,
    OtherTargetParagraph,
    OtherUntargetedParagraph,
    OtherTargetPage,
    OtherUntargetedPage,
    DirectSamePage,
    DirectOtherPage,
}

fn formula_component_document(relocate: bool) -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: (0..2).map(|_| ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))).collect(),
        total_pages: Some(2),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        for page_index in 0..2 {
            let page = view.document().page_at(&view, page_index).unwrap();
            let paragraphs = (0..2).map(|paragraph_index| {
                let bottom = if paragraph_index == 0 { 100.0 } else { 10.0 };
                let style = || ir::PdfStyleInit {
                    font_id: Some("test".into()), font_size: Some(10.0), ..Default::default()
                }.into_value();
                let formula = |left: f64| {
                    let bounds = || ir::BoxInit {
                        x: Some(left), y: Some(bottom + 10.0), x2: Some(left + 5.0), y2: Some(bottom + 20.0),
                    }.into_value();
                    ir::PdfFormulaInit {
                        r#box: Some(bounds()), x_offset: Some(0.0), y_offset: Some(0.0), x_advance: Some(5.0),
                        pdf_character: vec![ir::PdfCharacterInit {
                            char_unicode: Some("x".into()), pdf_character_id: Some(91), r#box: Some(bounds()),
                            visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds()) }.into_value()),
                            pdf_style: Some(style()), ..Default::default()
                        }.into_value()],
                        pdf_curve: vec![ir::PdfCurveInit { r#box: Some(bounds()), ..Default::default() }.into_value()],
                        pdf_form: vec![ir::PdfFormInit { r#box: Some(bounds()), ..Default::default() }.into_value()],
                        ..Default::default()
                    }.into_value()
                };
                let mut compositions = Vec::new();
                if relocate {
                    compositions.push(ir::PdfParagraphCompositionInit {
                        pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                            unicode: Some("A".into()), pdf_style: Some(style()), ..Default::default()
                        }.into_value()), ..Default::default()
                    }.into_value());
                }
                for _ in 0..2 {
                    compositions.push(ir::PdfParagraphCompositionInit {
                        pdf_formula: Some(formula(20.0)), ..Default::default()
                    }.into_value());
                }
                ir::PdfParagraphInit {
                    r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(bottom), x2: Some(200.0), y2: Some(bottom + 40.0) }.into_value()),
                    unicode: Some("Axx".into()), pdf_style: Some(style()), debug_id: Some("synthetic".into()),
                    pdf_paragraph_composition: compositions, ..Default::default()
                }.into_value()
            }).collect();
            page.set_pdf_paragraph(&mut view, paragraphs).unwrap();
        }
    }
    document
}

#[test]
fn passthrough_keeps_shared_formula_identity() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, false, Default::default());
    let mut document = formula_component_document(false);
    let identity = {
        let mut view = document.view::<ir::TypesettingView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        let formula = paragraph.pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap().retain(&mut view).unwrap();
        let identity = formula.identity();
        let shared = formula.share(&mut view).unwrap();
        paragraph.pdf_paragraph_composition_at(&view, 1).unwrap().set_pdf_formula(&mut view, Some(shared)).unwrap();
        identity
    };
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    stage.process(&mut document, &mut context).unwrap();
    let view = document.view::<ir::TypesettingView>();
    let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
    for index in 0..2 {
        let formula = paragraph.pdf_paragraph_composition_at(&view, index).unwrap().pdf_formula(&view).unwrap().unwrap();
        assert_eq!(formula.identity(), identity);
    }
}

#[test]
fn preview_returns_captured_fork_for_multiple_formulas_and_text() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for relocate in [false, true] {
        let mut document = formula_component_document(relocate);
        {
            let mut view = document.view::<ir::TypesettingView>();
            let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
            let mut parts = Vec::new();
            for index in 0..paragraph.pdf_paragraph_composition_len(&view).unwrap() {
                let part = paragraph.pdf_paragraph_composition_at(&view, index).unwrap().retain(&mut view).unwrap();
                parts.push(part.share(&mut view).unwrap());
                if index >= usize::from(relocate) {
                    parts.push(ir::PdfParagraphCompositionInit {
                        pdf_line: Some(ir::PdfLineInit { pdf_character: vec![ir::PdfCharacterInit {
                            char_unicode: Some(if index == usize::from(relocate) { "B" } else { "C" }.into()),
                            pdf_character_id: Some(37),
                            r#box: Some(ir::BoxInit { x: Some(40.0), y: Some(110.0), x2: Some(45.0), y2: Some(120.0) }.into_value()),
                            visual_bbox: Some(ir::VisualBboxInit { r#box: Some(ir::BoxInit { x: Some(40.0), y: Some(110.0), x2: Some(45.0), y2: Some(120.0) }.into_value()) }.into_value()),
                            pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()),
                            ..Default::default()
                        }.into_value()], ..Default::default() }.into_value()), ..Default::default()
                    }.into_value());
                }
            }
            paragraph.set_pdf_paragraph_composition(&mut view, parts).unwrap();
        }
        let before = document.artifact().unwrap();
        let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
        let (previews, fork) = stage.audit_fork_with_document(&document, &[(0, 0)], &mut context).unwrap();
        assert!(fork.is_some(), "relocate={relocate}: {previews:?}");
        assert_eq!(previews[0].formulas.len(), 2);
        let kinds: Vec<_> = previews[0].units.iter().map(|unit| match unit {
            babeldoc_p2_stages::stages::typesetting::PreviewUnit::Glyph(_) => "glyph",
            babeldoc_p2_stages::stages::typesetting::PreviewUnit::Formula { .. } => "formula",
            _ => "unknown",
        }).collect();
        assert_eq!(kinds, if relocate { vec!["glyph", "formula", "glyph", "formula", "glyph"] } else { vec!["formula", "glyph", "formula", "glyph"] }, "relocate={relocate}");
        assert_eq!(previews[0].formula_identities_match, Some(true));
        assert!(previews[0].rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
        assert!(previews[0].rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
        assert_eq!(before, document.artifact().unwrap());
        assert_ne!(before, fork.unwrap().artifact().unwrap());
    }
}

#[test]
fn preview_rejects_dropped_text_and_shared_formula_capture() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for shared in [false, true] {
        let mut document = formula_component_document(true);
        {
            let mut view = document.view::<ir::TypesettingView>();
            let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
            if shared {
                let formula = paragraph.pdf_paragraph_composition_at(&view, 1).unwrap().pdf_formula(&view).unwrap().unwrap().retain(&mut view).unwrap();
                let duplicate = formula.share(&mut view).unwrap();
                paragraph.pdf_paragraph_composition_at(&view, 2).unwrap().set_pdf_formula(&mut view, Some(duplicate)).unwrap();
            } else {
                paragraph.pdf_paragraph_composition_at(&view, 0).unwrap().pdf_same_style_unicode_characters(&view).unwrap().unwrap().set_pdf_style(&mut view, None).unwrap();
            }
        }
        let before = document.artifact().unwrap();
        let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
        let (previews, fork) = stage.audit_fork_with_document(&document, &[(0, 0)], &mut context).unwrap();
        assert!(fork.is_none(), "shared={shared}: {previews:?}");
        assert!(previews[0].rejections.contains(&if shared { PreviewRejection::CaptureFailed } else { PreviewRejection::TypesettingFailed }));
        assert_eq!(before, document.artifact().unwrap());
    }
}

fn check_formula_component_sharing(component: ReusedFormulaComponent) {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for relocate in [false, true] {
        for sharing in [FormulaComponentSharing::None, FormulaComponentSharing::WithinFormula,
            FormulaComponentSharing::WithinParagraph, FormulaComponentSharing::OtherTargetParagraph,
            FormulaComponentSharing::OtherUntargetedParagraph, FormulaComponentSharing::OtherTargetPage,
            FormulaComponentSharing::OtherUntargetedPage, FormulaComponentSharing::DirectSamePage,
            FormulaComponentSharing::DirectOtherPage] {
            let mut document = formula_component_document(relocate);
            let first_composition = usize::from(relocate);
            let destination = match sharing {
                FormulaComponentSharing::None | FormulaComponentSharing::DirectSamePage | FormulaComponentSharing::DirectOtherPage => None,
                FormulaComponentSharing::WithinFormula => Some((0, 0, first_composition)),
                FormulaComponentSharing::WithinParagraph => Some((0, 0, first_composition + 1)),
                FormulaComponentSharing::OtherTargetParagraph | FormulaComponentSharing::OtherUntargetedParagraph => Some((0, 1, first_composition)),
                FormulaComponentSharing::OtherTargetPage | FormulaComponentSharing::OtherUntargetedPage => Some((1, 0, first_composition)),
            };
            if let Some((page_index, paragraph_index, composition_index)) = destination {
                let mut view = document.view::<ir::TypesettingView>();
                let source = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
                    .pdf_paragraph_composition_at(&view, first_composition).unwrap().pdf_formula(&view).unwrap().unwrap();
                let destination = view.document().page_at(&view, page_index).unwrap().pdf_paragraph_at(&view, paragraph_index).unwrap()
                    .pdf_paragraph_composition_at(&view, composition_index).unwrap().pdf_formula(&view).unwrap().unwrap();
                match component {
                    ReusedFormulaComponent::Curve => {
                        let original = source.pdf_curve_at(&view, 0).unwrap().retain(&mut view).unwrap();
                        let mut values = vec![original.share(&mut view).unwrap()];
                        if matches!(sharing, FormulaComponentSharing::WithinFormula) { values.push(original.share(&mut view).unwrap()); }
                        destination.set_pdf_curve(&mut view, values).unwrap();
                    }
                    ReusedFormulaComponent::Form => {
                        let original = source.pdf_form_at(&view, 0).unwrap().retain(&mut view).unwrap();
                        let mut values = vec![original.share(&mut view).unwrap()];
                        if matches!(sharing, FormulaComponentSharing::WithinFormula) { values.push(original.share(&mut view).unwrap()); }
                        destination.set_pdf_form(&mut view, values).unwrap();
                    }
                }
            }
            if matches!(sharing, FormulaComponentSharing::DirectSamePage | FormulaComponentSharing::DirectOtherPage) {
                let mut view = document.view::<ir::TypesettingView>();
                let source = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
                    .pdf_paragraph_composition_at(&view, first_composition).unwrap().pdf_formula(&view).unwrap().unwrap();
                let page_index = usize::from(matches!(sharing, FormulaComponentSharing::DirectOtherPage));
                let page = view.document().page_at(&view, page_index).unwrap();
                match component {
                    ReusedFormulaComponent::Curve => {
                        let original = source.pdf_curve_at(&view, 0).unwrap().retain(&mut view).unwrap();
                        let shared = original.share(&mut view).unwrap();
                        page.set_pdf_curve(&mut view, vec![shared]).unwrap();
                    }
                    ReusedFormulaComponent::Form => {
                        let original = source.pdf_form_at(&view, 0).unwrap().retain(&mut view).unwrap();
                        let shared = original.share(&mut view).unwrap();
                        page.set_pdf_form(&mut view, vec![shared]).unwrap();
                    }
                }
            }
            let second_target = match sharing {
                FormulaComponentSharing::OtherTargetParagraph => (0, 1),
                FormulaComponentSharing::OtherTargetPage => (1, 0),
                _ => (1, 1),
            };
            let targets = [(0, 0), second_target, (0, 0)];
            let before = document.artifact().unwrap();
            let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
            let previews = stage.audit_fork(&document, &targets, &mut context).unwrap();
            let first_matches = matches!(sharing, FormulaComponentSharing::None);
            let second_matches = !matches!(sharing, FormulaComponentSharing::OtherTargetParagraph | FormulaComponentSharing::OtherTargetPage);
            for (preview, matches) in previews.iter().zip([first_matches, second_matches, first_matches]) {
                assert_eq!(preview.formula_identities_match, Some(matches), "{component:?} {sharing:?} relocate={relocate}: {preview:?}");
                assert_eq!(preview.rejections.contains(&PreviewRejection::FormulaIdentityMismatch), !matches);
                assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
                assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
            }
            assert_eq!(before, document.artifact().unwrap());
        }
    }
}

#[test]
fn preview_rejects_reused_curve_identity() {
    check_formula_component_sharing(ReusedFormulaComponent::Curve);
}

#[test]
fn preview_rejects_reused_form_identity() {
    check_formula_component_sharing(ReusedFormulaComponent::Form);
}

#[test]
fn preview_counts_formula_and_character_references_across_document() {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file());
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let stage = Typesetting::new("en", mapper, true, Default::default());
    for relocate in [false, true] {
        for kind in ["none", "formula", "formula_character", "direct", "line", "group", "page"] {
            for (page_index, paragraph_index) in [(0, 0), (0, 1), (1, 0)] {
                let mut document = formula_component_document(relocate);
                if kind != "none" {
                    let mut view = document.view::<ir::TypesettingView>();
                    let source = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
                        .pdf_paragraph_composition_at(&view, usize::from(relocate)).unwrap().pdf_formula(&view).unwrap().unwrap();
                    let formula = source.retain(&mut view).unwrap();
                    let character = source.pdf_character_at(&view, 0).unwrap().retain(&mut view).unwrap();
                    let page = view.document().page_at(&view, page_index).unwrap();
                    if kind == "page" {
                        let shared = character.share(&mut view).unwrap();
                        page.set_pdf_character(&mut view, vec![shared]).unwrap();
                    } else if kind == "formula_character" {
                        let paragraph = page.pdf_paragraph_at(&view, paragraph_index).unwrap();
                        let index = usize::from(relocate) + usize::from(page_index == 0 && paragraph_index == 0);
                        let destination = paragraph.pdf_paragraph_composition_at(&view, index).unwrap().pdf_formula(&view).unwrap().unwrap();
                        let shared = character.share(&mut view).unwrap();
                        destination.set_pdf_character(&mut view, vec![shared]).unwrap();
                    } else {
                        let paragraph = page.pdf_paragraph_at(&view, paragraph_index).unwrap();
                        let mut compositions = Vec::new();
                        for index in 0..paragraph.pdf_paragraph_composition_len(&view).unwrap() {
                            let original = paragraph.pdf_paragraph_composition_at(&view, index).unwrap().retain(&mut view).unwrap();
                            compositions.push(original.share(&mut view).unwrap());
                        }
                        let extra = match kind {
                            "formula" => ir::PdfParagraphCompositionInit { pdf_formula: Some(formula.share(&mut view).unwrap()), ..Default::default() },
                            "direct" => ir::PdfParagraphCompositionInit { pdf_character: Some(character.share(&mut view).unwrap()), ..Default::default() },
                            "line" => ir::PdfParagraphCompositionInit { pdf_line: Some(ir::PdfLineInit { pdf_character: vec![character.share(&mut view).unwrap()], ..Default::default() }.into_value()), ..Default::default() },
                            "group" => ir::PdfParagraphCompositionInit { pdf_same_style_characters: Some(ir::PdfSameStyleCharactersInit { pdf_character: vec![character.share(&mut view).unwrap()], ..Default::default() }.into_value()), ..Default::default() },
                            _ => unreachable!(),
                        };
                        compositions.push(extra.into_value());
                        paragraph.set_pdf_paragraph_composition(&mut view, compositions).unwrap();
                    }
                }
                let before = document.artifact().unwrap();
                let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
                let preview = stage.audit_fork(&document, &[(0, 0)], &mut context).unwrap().remove(0);
                if preview.rejections.contains(&PreviewRejection::TypesettingFailed) {
                    assert_eq!(kind, "direct", "unexpected typesetting failure: {preview:?}");
                    assert_ne!(preview.formula_identities_match, Some(true));
                } else {
                    assert_eq!(preview.formula_identities_match, Some(kind == "none"), "{kind} {page_index} {paragraph_index} relocate={relocate}: {preview:?}");
                    assert_eq!(preview.rejections.contains(&PreviewRejection::FormulaIdentityMismatch), kind != "none");
                }
                assert!(preview.rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
                assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
                assert_eq!(before, document.artifact().unwrap());
            }
        }
    }
}
