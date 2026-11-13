use babeldoc_ir_core as ir;
use babeldoc_writer_core::{AvailableFonts, BuildOptions, WriterDiagnosticKind, WriterError, build_page_output, build_page_output_with_diagnostics};

fn node(data: ir::NodeData) -> ir::Edge {
    ir::Edge::Inline(Box::new(ir::Node::new(data)))
}

fn bounds() -> ir::Edge {
    node(ir::NodeData::Box(ir::DataBox { x: Some(0.0), y: Some(0.0), x2: Some(10.0), y2: Some(10.0) }))
}

fn document(id: Option<i64>, text: &str, encoding: Option<i64>, scope: Option<i64>, other_form_encoding: Option<i64>) -> ir::Document {
    let character = node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: id, char_unicode: Some(text.into()), xobj_id: scope,
        render_order: Some(7), sub_render_order: Some(3), r#box: Some(bounds()),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle { font_id: Some("F1".into()), font_size: Some(12.0), ..Default::default() }))),
        ..Default::default()
    }));
    let mut page = ir::DataPage {
        page_number: Some(2), cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds()) }))),
        pdf_character: vec![character],
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont { font_id: Some("F1".into()), encoding_length: encoding, ..Default::default() }))],
        pdf_xobject: if scope == Some(1) { vec![node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
            xobj_id: scope, xref_id: Some(42), base_operations: Some(node(ir::NodeData::BaseOperations(ir::DataBaseOperations { value: "D77#BAOHaX00".into() }))), ..Default::default()
        }))] } else { vec![] },
        ..Default::default()
    };
    if let Some(encoding) = other_form_encoding {
        page.pdf_xobject.push(node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
            xobj_id: Some(2), xref_id: Some(43),
            base_operations: Some(node(ir::NodeData::BaseOperations(ir::DataBaseOperations { value: "D77#BAOHaX00".into() }))),
            pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont { font_id: Some("F1".into()), encoding_length: Some(encoding), ..Default::default() }))],
            ..Default::default()
        })));
    }
    ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: vec![node(ir::NodeData::Page(page))], ..Default::default() })), Default::default(), Default::default()).unwrap()
}

fn run(id: Option<i64>, text: &str, encoding: Option<i64>, scope: Option<i64>, options: BuildOptions, available: AvailableFonts) -> (Vec<u8>, Vec<babeldoc_writer_core::WriterDiagnostic>) {
    let doc = document(id, text, encoding, scope, None);
    run_document(doc, options, available)
}

fn run_document(mut doc: ir::Document, options: BuildOptions, available: AvailableFonts) -> (Vec<u8>, Vec<babeldoc_writer_core::WriterDiagnostic>) {
    let view = doc.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let normal = build_page_output(&view, &page, &options);
    let (observed, diagnostics) = build_page_output_with_diagnostics(&view, &page, &options, &available).unwrap();
    if let Ok(normal) = normal {
        assert_eq!(normal.stream, observed.stream);
        assert_eq!(normal.forms.len(), observed.forms.len());
        for (ordinary, diagnostic) in normal.forms.iter().zip(&observed.forms) {
            assert_eq!((ordinary.xobj_id, ordinary.xref_id, &ordinary.stream), (diagnostic.xobj_id, diagnostic.xref_id, &diagnostic.stream));
        }
        assert_eq!(normal.counts.units, observed.counts.units);
    } else {
        assert!(diagnostics.iter().any(|issue| matches!(issue.kind,
            WriterDiagnosticKind::MissingCharacterId | WriterDiagnosticKind::MissingEncodingLength | WriterDiagnosticKind::FontRejectedByGate)));
    }
    (observed.stream, diagnostics)
}

#[test]
fn observes_actual_output_scope_and_form_resources() {
    let mut fonts = available();
    fonts.forms.insert(Some(1), Default::default());
    let (_, diagnostics) = run(Some(65), "A", Some(1), Some(1), BuildOptions::default(), fonts);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::FontUnavailable);
    let mut fonts = AvailableFonts::default();
    fonts.forms.insert(Some(99), ["F1".to_string()].into_iter().collect());
    let (_, diagnostics) = run(Some(65), "A", Some(1), Some(99), BuildOptions::default(), fonts);
    assert!(diagnostics.iter().any(|d| d.kind == WriterDiagnosticKind::MissingFormScope));
    assert!(diagnostics.iter().any(|d| d.kind == WriterDiagnosticKind::FontUnavailable));
}

#[test]
fn missing_form_observation_is_not_page_observation() {
    let (_, diagnostics) = run(Some(65), "A", Some(1), Some(1), BuildOptions::default(), available());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::FormFontObservationUnavailable);
    assert_eq!((diagnostics[0].scope, diagnostics[0].output_scope, diagnostics[0].output_is_form), (Some(1), Some(1), true));
    let mut fonts = available();
    fonts.forms.insert(Some(1), ["F1".to_string()].into_iter().collect());
    let (_, diagnostics) = run(Some(65), "A", Some(1), Some(1), BuildOptions::default(), fonts);
    assert!(diagnostics.is_empty());
}

#[test]
fn foreign_form_encoding_is_reported_but_local_encoding_is_not() {
    let doc = document(Some(65), "A", None, Some(1), Some(2));
    let mut fonts = available();
    fonts.forms.insert(Some(1), ["F1".to_string()].into_iter().collect());
    let (_, diagnostics) = run_document(doc, BuildOptions::default(), fonts.clone());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::EncodingFromOtherScope);
    let doc = document(Some(65), "A", Some(1), Some(1), Some(2));
    let (_, diagnostics) = run_document(doc, BuildOptions::default(), fonts);
    assert!(diagnostics.is_empty());
}

#[test]
fn diagnostics_identify_objects_and_actual_output() {
    let (_, diagnostics) = run(Some(65), "A", None, Some(99), BuildOptions::default(), available());
    assert!(diagnostics.len() >= 2);
    for issue in &diagnostics {
        assert_eq!((issue.scope, issue.output_scope, issue.output_is_form, issue.object_kind), (Some(99), None, false, "character"));
        assert_eq!(issue.object_id, diagnostics[0].object_id);
    }
    let (_, diagnostics) = run(None, "\n", None, Some(99), BuildOptions::default(), available());
    assert!(diagnostics.iter().any(|issue| issue.kind == WriterDiagnosticKind::MissingCharacterId));
}

#[test]
fn same_order_characters_have_distinct_object_identities() {
    let character = || node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        char_unicode: Some("A".into()), render_order: Some(7), sub_render_order: Some(3),
        ..Default::default()
    }));
    let page = ir::DataPage {
        page_number: Some(2), cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds()) }))),
        pdf_character: vec![character(), character()], ..Default::default()
    };
    let doc = ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![node(ir::NodeData::Page(page))], ..Default::default()
    })), Default::default(), Default::default()).unwrap();
    let (_, diagnostics) = run_document(doc, BuildOptions::default(), available());
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::MissingCharacterId);
    assert_eq!(diagnostics[1].kind, WriterDiagnosticKind::MissingCharacterId);
    assert_ne!(diagnostics[0].object_id, diagnostics[1].object_id);
}

fn available() -> AvailableFonts {
    let mut fonts = AvailableFonts::default();
    fonts.page.insert("F1".into());
    fonts
}

#[test]
fn detects_missing_id_encoding_and_unknown_scope() {
    let (_, missing) = run(None, "A", Some(1), None, BuildOptions::default(), available());
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].kind, WriterDiagnosticKind::MissingCharacterId);
    assert_eq!((missing[0].page_number, missing[0].render_order, missing[0].sub_render_order), (2, 7, 3));
    let (stream, missing) = run(Some(65), "A", None, Some(99), BuildOptions::default(), available());
    assert!(missing.iter().any(|d| d.kind == WriterDiagnosticKind::MissingEncodingLength && d.font_id.as_deref() == Some("F1")));
    assert!(missing.iter().any(|d| d.kind == WriterDiagnosticKind::MissingFormScope && d.scope == Some(99)));
    assert!(!stream.windows(2).any(|part| part == b"Tj"));
}

#[test]
fn detects_observed_font_and_gate_rejection_independently() {
    let (stream, diagnostics) = run(Some(65), "A", Some(1), None, BuildOptions::default(), AvailableFonts::default());
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::FontUnavailable);
    assert!(stream.windows(2).any(|part| part == b"Tj"));
    let options = BuildOptions { check_font_exists: true, available: Some(AvailableFonts::default()), ..Default::default() };
    let (stream, diagnostics) = run(Some(65), "A", Some(1), None, options, available());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::FontRejectedByGate);
    assert!(!stream.windows(2).any(|part| part == b"Tj"));
}

#[test]
fn missing_newline_id_is_reported_and_explicit_skip_is_not() {
    let (stream, diagnostics) = run(Some(65), "A", Some(1), None, BuildOptions::default(), available());
    assert!(diagnostics.is_empty());
    assert!(stream.windows(2).any(|part| part == b"Tj"));
    let (_, diagnostics) = run(None, "\n", None, None, BuildOptions::default(), AvailableFonts::default());
    assert_eq!(diagnostics[0].kind, WriterDiagnosticKind::MissingCharacterId);
    let options = BuildOptions { skip_char: true, ..Default::default() };
    let (_, diagnostics) = run(None, "A", None, None, options, AvailableFonts::default());
    assert!(diagnostics.is_empty());
}

#[test]
fn production_fails_for_character_losses_and_preserves_intentional_skips() {
    let cases = [
        (document(None, "A", Some(1), None, None), BuildOptions::default(), "writer.missing_character_id"),
        (document(None, " ", Some(1), None, None), BuildOptions::default(), "writer.missing_character_id"),
        (document(None, "\n", Some(1), None, None), BuildOptions::default(), "writer.missing_character_id"),
        (document(Some(65), "A", None, None, None), BuildOptions::default(), "writer.missing_encoding_length"),
        (document(Some(65), "A", Some(1), None, None), BuildOptions { check_font_exists: true, available: Some(AvailableFonts::default()), ..Default::default() }, "writer.font_rejected_by_gate"),
        (document(Some(65), "A", None, Some(1), None), BuildOptions::default(), "writer.missing_encoding_length"),
    ];
    for (mut document, options, code) in cases {
        let view = document.view::<ir::ReadOnlyView>();
        let page = view.document().page_at(&view, 0).unwrap();
        assert!(matches!(build_page_output(&view, &page, &options), Err(WriterError::Invalid(message)) if message.starts_with(code)));
    }
    for (text, options) in [("A", BuildOptions { skip_char: true, ..Default::default() })] {
        let mut document = document(None, text, None, None, None);
        let view = document.view::<ir::ReadOnlyView>();
        let page = view.document().page_at(&view, 0).unwrap();
        assert!(build_page_output(&view, &page, &options).is_ok());
    }
}

#[test]
fn font_gate_checks_the_actual_output_scope() {
    let mut document = document(Some(65), "A", Some(1), Some(99), None);
    let mut fonts = AvailableFonts::default();
    fonts.forms.insert(Some(99), ["F1".to_string()].into_iter().collect());
    let options = BuildOptions { check_font_exists: true, available: Some(fonts), ..Default::default() };
    let view = document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    assert!(matches!(build_page_output(&view, &page, &options), Err(WriterError::Invalid(message)) if message.starts_with("writer.font_rejected_by_gate")));
}
