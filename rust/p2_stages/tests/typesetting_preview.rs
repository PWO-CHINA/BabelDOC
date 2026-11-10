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
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let previews = stage.audit_fork(&document, &[(0, 0), (0, 0), (0, 1)], &mut context).unwrap();
    assert_eq!(previews.len(), 3);
    assert_eq!(previews[1].glyphs.len(), 1);
    assert!(previews[2].rejections.contains(&PreviewRejection::TargetMissing));
    let preview = &previews[0];
    assert_eq!(preview.glyphs.len(), 1);
    assert_eq!(preview.glyphs[0].text.as_deref(), Some("A"));
    assert!(preview.rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
    assert_eq!(before, document.artifact().unwrap());
}
