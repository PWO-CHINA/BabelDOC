use babeldoc_ir_core as ir;
use babeldoc_p2_stages::{
    NoopCancellation, NoopProgress, StageContext, StageError,
    font_mapper::font_mapper_from_asset_paths,
    stages::typesetting::{Typesetting, TypesettingError, TypesettingFailure, typesetting_failure},
};

fn stage(progress: bool) -> Typesetting {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    Typesetting::new("en", mapper, progress, Default::default())
}

fn document(text: &str, style: Option<ir::FieldValue<ir::types::PdfStyle>>, width: f64, height: f64) -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let compositions = if text.is_empty() { vec![] } else {
            vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some(text.into()), pdf_style: style, ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()]
        };
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(10.0 + width), y2: Some(10.0 + height) }.into_value()),
            unicode: Some(text.into()), pdf_paragraph_composition: compositions, ..Default::default()
        }.into_value()]).unwrap();
    }
    document
}

fn style(id: Option<&str>) -> ir::FieldValue<ir::types::PdfStyle> {
    ir::PdfStyleInit { font_id: id.map(str::to_owned), font_size: Some(12.0), ..Default::default() }.into_value()
}

fn process(stage: &Typesetting, document: &mut ir::Document) -> Result<(), StageError> {
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    stage.process(document, &mut context)
}

#[test]
fn invalid_translation_resources_return_typed_failures_without_losing_content() {
    for progress in [false, true] {
        for (text, style, expected) in [
            ("A", None, TypesettingFailure::MissingStyle),
            ("A", Some(style(None)), TypesettingFailure::MissingFontId),
            ("A", Some(style(Some("unknown"))), TypesettingFailure::MissingSourceFont),
            ("A\u{10ffff}", Some(style(Some("test"))), TypesettingFailure::MissingGlyph),
        ] {
            let mut document = document(text, style, 100.0, 30.0);
            let before = document.artifact().unwrap();
            let error = process(&stage(progress), &mut document).unwrap_err();
            assert_eq!(typesetting_failure(&error), Some(expected), "{error}");
            let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
            assert!(matches!(stage(progress).process_typed(&mut document, &mut context), Err(TypesettingError::Failure { kind, .. }) if kind == expected));
            let after = document.artifact().unwrap();
            assert_eq!(before["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"], after["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]);
        }
    }
}

#[test]
fn failed_layout_leaves_paragraph_and_page_objects_intact() {
    for progress in [false, true] {
        let mut document = document("Wide text", Some(style(Some("test"))), 0.0001, 0.0001);
        let before = document.artifact().unwrap();
        let error = process(&stage(progress), &mut document).unwrap_err();
        assert_eq!(typesetting_failure(&error), Some(TypesettingFailure::LayoutExhausted), "{error}");
        let after = document.artifact().unwrap();
        assert_eq!(before["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"], after["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]);
        assert_eq!(before["document"]["page"][0]["pdf_paragraph"][0]["box"], after["document"]["page"][0]["pdf_paragraph"][0]["box"]);
        assert_eq!(before["document"]["page"][0]["pdf_curve"], after["document"]["page"][0]["pdf_curve"]);
        assert_eq!(before["document"]["page"][0]["pdf_form"], after["document"]["page"][0]["pdf_form"]);
    }
}

#[test]
fn empty_slice_and_newline_only_run_are_preserved_but_missing_content_fails() {
    for text in ["", "\n"] {
        let mut document = document(text, None, 100.0, 30.0);
        process(&stage(false), &mut document).unwrap();
    }
    let mut empty_document = document("", None, 100.0, 30.0);
    {
        let mut view = empty_document.view::<ir::TypesettingView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        paragraph.set_unicode(&mut view, Some("unrendered".into())).unwrap();
    }
    let error = process(&stage(false), &mut empty_document).unwrap_err();
    assert_eq!(typesetting_failure(&error), Some(TypesettingFailure::MissingContent));

    let mut document = document("A", Some(style(Some("test"))), 100.0, 30.0);
    {
        let mut view = document.view::<ir::TypesettingView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        let unicode = paragraph.pdf_paragraph_composition_at(&view, 0).unwrap().pdf_same_style_unicode_characters(&view).unwrap().unwrap();
        unicode.set_unicode(&mut view, None).unwrap();
    }
    let error = process(&stage(false), &mut document).unwrap_err();
    assert_eq!(typesetting_failure(&error), Some(TypesettingFailure::MissingContent));
}

#[test]
fn invalid_initial_scale_cannot_cause_an_unbounded_retry() {
    let mut document = document("A", Some(style(Some("test"))), 100.0, 30.0);
    {
        let mut view = document.view::<ir::TypesettingView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        paragraph.set_optimal_scale(&mut view, Some(f64::INFINITY)).unwrap();
    }
    let error = process(&stage(false), &mut document).unwrap_err();
    assert_eq!(typesetting_failure(&error), Some(TypesettingFailure::LayoutExhausted));
}

#[test]
fn preview_accepts_a_genuinely_empty_slice_without_claiming_global_proof() {
    use babeldoc_p2_stages::stages::typesetting::PreviewRejection;
    let document = document("", None, 100.0, 30.0);
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let (previews, fork) = stage(true).audit_fork_with_document(&document, &[(0, 0)], &mut context).unwrap();
    assert!(!previews[0].rejections.contains(&PreviewRejection::InvalidGlyphEvidence));
    assert!(!previews[0].rejections.contains(&PreviewRejection::CaptureFailed));
    assert!(previews[0].rejections.contains(&PreviewRejection::FormulaIdentityUnavailable));
    assert!(previews[0].rejections.contains(&PreviewRejection::ObstacleAndClipEvidenceUnavailable));
    assert!(fork.is_some());
}

#[test]
fn ordinary_text_still_renders() {
    let mut document = document("ABC", Some(style(Some("test"))), 100.0, 30.0);
    process(&stage(false), &mut document).unwrap();
    let after = document.artifact().unwrap();
    let compositions = after["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"].as_array().unwrap();
    let text: String = compositions.iter().map(|part| part["pdf_character"]["char_unicode"].as_str().unwrap()).collect();
    assert_eq!(text, "ABC");
}

#[test]
fn mapped_asset_in_real_xobject_uses_asset_styles_and_detects_ambiguity() {
    use babeldoc_il_types::RuntimeBool;
    use babeldoc_p2_stages::font_mapper::SourceFontStyle;
    let (regular, bold) = if cfg!(windows) {
        ("C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/arialbd.ttf")
    } else {
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf")
    };
    let family = serde_json::json!({"normal":["test", "bold"],"script":[],"fallback":["test", "bold"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":regular, "bold":bold}), None).unwrap();
    let make_document = |source_id: Option<&str>, target_id: &str| {
        let node = |data| ir::Edge::Inline(Box::new(ir::Node::new(data)));
        let source_font = source_id.map(|id| node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some(id.into()), bold: Some(RuntimeBool::Bool(true)),
            italic: Some(RuntimeBool::Bool(false)), monospace: Some(RuntimeBool::Bool(false)),
            serif: Some(RuntimeBool::Bool(false)), ..Default::default()
        }))).into_iter().collect();
        let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(ir::DataPage {
                pdf_xobject: vec![node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
                    xobj_id: Some(7), pdf_font: source_font, ..Default::default()
                }))], ..Default::default()
            }))], ..Default::default()
        }));
        let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
        {
            let mut view = document.view::<ir::ParagraphFinderView>();
            let page = view.document().page_at(&view, 0).unwrap();
            page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
                xobj_id: Some(7), unicode: Some("A".into()),
                r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(100.0), y2: Some(40.0) }.into_value()),
                pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                    pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                        unicode: Some("A".into()), pdf_style: Some(style(Some(target_id))), ..Default::default()
                    }.into_value()), ..Default::default()
                }.into_value()], ..Default::default()
            }.into_value()]).unwrap();
        }
        document
    };
    for (source_id, target_id, verified_style, expected) in [
        (None, "test", false, "test"),
        (Some("test"), "test", true, "test"),
        (Some("source-bold"), "source-bold", false, "bold"),
    ] {
        let styles = if verified_style { [("test".into(), SourceFontStyle::default())].into_iter().collect() } else { Default::default() };
        let stage = Typesetting::new("en", mapper.clone(), false, styles);
        let mut document = make_document(source_id, target_id);
        process(&stage, &mut document).unwrap();
        let artifact = document.artifact().unwrap();
        let character = &artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_character"];
        assert_eq!(character["pdf_style"]["font_id"], expected);
        assert_eq!(character["xobj_id"], 7);
    }
    let mut document = make_document(Some("test"), "test");
    let error = process(&Typesetting::new("en", mapper, false, Default::default()), &mut document).unwrap_err();
    assert_eq!(typesetting_failure(&error), Some(TypesettingFailure::AmbiguousFontId));
}
