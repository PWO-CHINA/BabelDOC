use super::*;
use babeldoc_ir_core::{DataDocument, DataPage, DataPdfCharacter, DataPdfParagraph, DataPdfParagraphComposition, DataPdfSameStyleUnicodeCharacters, DataPdfStyle, History, Node, NodeData, Table};

fn document() -> Document {
    let mut table = Table::default();
    let mut pages = Vec::new();
    for text in ["left original", "right original"] {
        let style = table.attach(Node::new(NodeData::PdfStyle(DataPdfStyle {
            font_id: Some("source".into()), font_size: Some(10.0), ..Default::default()
        })));
        let characters = table.attach(Node::new(NodeData::PdfSameStyleUnicodeCharacters(DataPdfSameStyleUnicodeCharacters {
            pdf_style: Some(style.share()), unicode: Some(text.into()), ..Default::default()
        })));
        let composition = table.attach(Node::new(NodeData::PdfParagraphComposition(DataPdfParagraphComposition {
            pdf_same_style_unicode_characters: Some(characters), ..Default::default()
        })));
        let paragraph = table.attach(Node::new(NodeData::PdfParagraph(DataPdfParagraph {
            unicode: Some(text.into()), pdf_style: Some(style), pdf_paragraph_composition: vec![composition], ..Default::default()
        })));
        pages.push(table.attach(Node::new(NodeData::Page(DataPage {
            pdf_paragraph: vec![paragraph], ..Default::default()
        }))));
    }
    Document::from_parts(Node::new(NodeData::Document(DataDocument { page: pages, ..Default::default() })), table, History::default()).unwrap()
}

fn candidates() -> (Candidate, Candidate) {
    (Candidate { location: ParagraphLocation { page: 0, paragraph: 0 }, unicode: "left candidate".into() },
     Candidate { location: ParagraphLocation { page: 1, paragraph: 0 }, unicode: "right candidate".into() })
}

fn paragraph_text(document: &Document, page: usize) -> String {
    let view = document.read();
    view.document().page_at(&view, page).unwrap().pdf_paragraph_at(&view, 0).unwrap().unicode(&view).unwrap().clone().unwrap()
}

#[test]
fn completed_trial_rejects_and_keeps_original() {
    let original = document();
    let before = original.artifact().unwrap();
    let original_cursor = {
        let view = original.read();
        view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
    };
    let (left, right) = candidates();
    let outcome = trial(&original, left, right, |candidate| {
        let view = candidate.read();
        assert_eq!(original_cursor.unicode(&view).unwrap_err(), AccessError::StaleHandle);
        drop(view);
        assert_eq!(paragraph_text(candidate, 0), "left candidate");
        assert_eq!(paragraph_text(candidate, 1), "right candidate");
        candidate.compact();
        LayoutOutcome::Completed
    });
    assert_eq!(outcome, TrialOutcome::RejectedInsufficientEvidence);
    assert_eq!(original.artifact().unwrap(), before);
}

#[test]
fn cancellation_and_failure_leave_original_untouched() {
    let original = document();
    let before = original.artifact().unwrap();
    let (left, right) = candidates();
    assert_eq!(trial(&original, left, right, |_| LayoutOutcome::Cancelled), TrialOutcome::Cancelled);
    let (left, right) = candidates();
    assert_eq!(trial(&original, left, right, |_| LayoutOutcome::Failed), TrialOutcome::LayoutFailed);
    assert_eq!(original.artifact().unwrap(), before);
}

#[test]
fn invalid_or_missing_targets_never_run_layout() {
    let original = document();
    let before = original.artifact().unwrap();
    let (left, mut right) = candidates();
    right.location = left.location;
    assert_eq!(trial(&original, left, right, |_| -> LayoutOutcome { panic!("layout must not run") }), TrialOutcome::InvalidCandidate);
    let (left, mut right) = candidates();
    right.location.page = 99;
    assert!(matches!(trial(&original, left, right, |_| -> LayoutOutcome { panic!("layout must not run") }), TrialOutcome::ApplyFailed(_)));
    assert_eq!(original.artifact().unwrap(), before);
}

#[test]
fn missing_style_and_unknown_composition_are_rejected() {
    for case in 0..4 {
        let mut table = Table::default();
        let style = if case == 0 { None } else {
            Some(table.attach(Node::new(NodeData::PdfStyle(DataPdfStyle {
                font_id: Some("source".into()), font_size: Some(if case == 1 { f64::NAN } else { 10.0 }), ..Default::default()
            }))))
        };
        let compositions = if case == 2 {
            let character = table.attach(Node::new(NodeData::PdfCharacter(DataPdfCharacter::default())));
            vec![table.attach(Node::new(NodeData::PdfParagraphComposition(DataPdfParagraphComposition {
                pdf_character: Some(character), ..Default::default()
            })))]
        } else { vec![] };
        let paragraph = table.attach(Node::new(NodeData::PdfParagraph(DataPdfParagraph {
            pdf_style: style, pdf_paragraph_composition: compositions, ..Default::default()
        })));
        let page = table.attach(Node::new(NodeData::Page(DataPage { pdf_paragraph: vec![paragraph], ..Default::default() })));
        let original = Document::from_parts(Node::new(NodeData::Document(DataDocument { page: vec![page], ..Default::default() })), table, History::default()).unwrap();
        let (left, mut right) = candidates();
        right.location.page = 1;
        assert_eq!(trial(&original, left, right, |_| -> LayoutOutcome { panic!("layout must not run") }), TrialOutcome::UnsupportedComposition);
    }
}

fn set_paragraph_text(document: &mut Document, page: usize, text: &str) {
    let mut view = document.view::<TranslationView>();
    let paragraph = view.document().page_at(&view, page).unwrap().pdf_paragraph_at(&view, 0).unwrap();
    paragraph.set_unicode(&mut view, Some(text.into())).unwrap();
}

#[test]
fn stage_transaction_commits_all_outputs_or_none() {
    let mut original = document();
    let before = original.artifact().unwrap();
    for stop_after in 1..=2 {
        let mut stage = StageTransaction::new(&mut original, true);
        set_paragraph_text(stage.document(), 0, "first output");
        if stop_after == 2 {
            set_paragraph_text(stage.document(), 1, "second output");
        }
        drop(stage);
        assert_eq!(original.artifact().unwrap(), before);
    }
    let mut stage = StageTransaction::new(&mut original, true);
    set_paragraph_text(stage.document(), 0, "first output");
    set_paragraph_text(stage.document(), 1, "second output");
    stage.commit();
    assert_eq!(paragraph_text(&original, 0), "first output");
    assert_eq!(paragraph_text(&original, 1), "second output");
}

#[test]
fn stage_transaction_preserves_direct_writes_when_disabled() {
    let mut original = document();
    let mut stage = StageTransaction::new(&mut original, false);
    set_paragraph_text(stage.document(), 0, "direct output");
    drop(stage);
    assert_eq!(paragraph_text(&original, 0), "direct output");
}

#[test]
fn prepared_tracking_replaces_existing_file_and_commits_document() {
    let directory = std::env::temp_dir().join(format!("babeldoc-tracking-test-{}-{}", std::process::id(), TRACKING_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("tracking.json");
    std::fs::write(&path, "old").unwrap();
    let mut original = document();
    let mut stage = StageTransaction::new(&mut original, true);
    set_paragraph_text(stage.document(), 0, "new output");
    let tracking = PreparedTracking::prepare(&path, "new").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    drop(tracking);
    drop(stage);
    assert_eq!(paragraph_text(&original, 0), "left original");
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    let mut stage = StageTransaction::new(&mut original, true);
    set_paragraph_text(stage.document(), 0, "new output");
    let tracking = PreparedTracking::prepare(&path, "new").unwrap();
    stage.commit_with_tracking(Some(tracking)).unwrap();
    assert_eq!(paragraph_text(&original, 0), "new output");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn tracking_publish_failure_does_not_commit_document() {
    let directory = std::env::temp_dir().join(format!("babeldoc-tracking-publish-failure-{}-{}", std::process::id(), TRACKING_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("tracking.json");
    std::fs::create_dir(&path).unwrap();
    let mut original = document();
    let before = original.artifact().unwrap();
    let mut stage = StageTransaction::new(&mut original, true);
    set_paragraph_text(stage.document(), 0, "new output");
    let tracking = PreparedTracking::prepare(&path, "new").unwrap();
    assert!(stage.commit_with_tracking(Some(tracking)).is_err());
    assert_eq!(original.artifact().unwrap(), before);
    assert!(path.is_dir());
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    std::fs::remove_dir(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn tracking_prepare_failure_leaves_document_unchanged() {
    let directory = std::env::temp_dir().join(format!("babeldoc-tracking-failure-{}-{}", std::process::id(), TRACKING_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&directory, "not a directory").unwrap();
    let mut original = document();
    let mut stage = StageTransaction::new(&mut original, true);
    set_paragraph_text(stage.document(), 0, "new output");
    assert!(PreparedTracking::prepare(&directory.join("tracking.json"), "new").is_err());
    drop(stage);
    assert_eq!(paragraph_text(&original, 0), "left original");
    std::fs::remove_file(directory).unwrap();
}
