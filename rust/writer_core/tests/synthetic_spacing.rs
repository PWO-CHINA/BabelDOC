use babeldoc_ir_core as ir;
use babeldoc_writer_core::{AvailableFonts, BuildOptions, WriterDiagnosticKind, WriterError, build_page_output, build_page_output_with_diagnostics};

fn node(data: ir::NodeData) -> ir::Edge {
    ir::Edge::Inline(Box::new(ir::Node::new(data)))
}

fn bounds(x: f64, x2: f64) -> ir::Edge {
    node(ir::NodeData::Box(ir::DataBox { x: Some(x), y: Some(10.0), x2: Some(x2), y2: Some(20.0) }))
}

fn character(text: &str, id: Option<i64>, marked: Option<bool>, x: f64) -> ir::Edge {
    node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        char_unicode: Some(text.into()), pdf_character_id: id, synthetic_spacing: marked,
        r#box: Some(bounds(x, x + 2.0)),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
            font_id: Some("F1".into()), font_size: Some(10.0), ..Default::default()
        }))),
        ..Default::default()
    }))
}

fn document(characters: Vec<ir::Edge>, formula: bool) -> ir::Document {
    let mut page = ir::DataPage {
        page_number: Some(0),
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds(0.0, 200.0)) }))),
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some("F1".into()), encoding_length: Some(1), ..Default::default()
        }))],
        ..Default::default()
    };
    if formula {
        page.pdf_paragraph = vec![node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph {
            pdf_paragraph_composition: vec![node(ir::NodeData::PdfParagraphComposition(ir::DataPdfParagraphComposition {
                pdf_formula: Some(node(ir::NodeData::PdfFormula(ir::DataPdfFormula { pdf_character: characters, ..Default::default() }))),
                ..Default::default()
            }))],
            ..Default::default()
        }))];
    } else {
        page.pdf_character = characters;
    }
    ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![node(ir::NodeData::Page(page))], ..Default::default()
    })), Default::default(), Default::default()).unwrap()
}

#[test]
fn marked_spacing_is_not_drawn_between_real_spaces_or_in_formula() {
    for formula in [false, true] {
        let mut document = document(vec![
            character(" ", Some(32), None, 10.0),
            character(" ", None, Some(true), 12.5),
            character(" ", Some(32), None, 24.41216),
            character("A", Some(65), None, 26.91216),
            character(" ", None, Some(true), 27.0),
        ], formula);
        let view = document.view::<ir::ReadOnlyView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let output = build_page_output(&view, &page, &BuildOptions::default()).unwrap();
        let stream = String::from_utf8(output.stream).unwrap();
        assert_eq!(stream.matches("<20> Tj").count(), 2, "{stream}");
        assert_eq!(stream.matches("<41> Tj").count(), 1, "{stream}");
        assert!(stream.contains("24.412160 10.000000 Tm <20> Tj"), "{stream}");
        assert!(stream.contains("26.912160 10.000000 Tm <41> Tj"), "{stream}");
        let mut available = AvailableFonts::default();
        available.page.insert("F1".into());
        let (_, issues) = build_page_output_with_diagnostics(&view, &page, &BuildOptions::default(), &available).unwrap();
        assert!(issues.is_empty(), "{issues:?}");
    }
}

#[test]
fn missing_real_glyphs_are_not_hidden_by_text_or_invalid_marker() {
    for (text, marked) in [(" ", None), (" ", Some(false)), ("A", None), ("\n", None), ("A", Some(true))] {
        let mut document = document(vec![character(text, None, marked, 10.0)], false);
        let view = document.view::<ir::ReadOnlyView>();
        let page = view.document().page_at(&view, 0).unwrap();
        assert!(matches!(build_page_output(&view, &page, &BuildOptions::default()),
            Err(WriterError::Invalid(message)) if message.starts_with("writer.missing_character_id")));
        let (_, issues) = build_page_output_with_diagnostics(&view, &page, &BuildOptions::default(), &AvailableFonts::default()).unwrap();
        assert!(issues.iter().any(|issue| issue.kind == WriterDiagnosticKind::MissingCharacterId));
    }
    let mut document = document(vec![character(" ", Some(32), Some(true), 10.0)], false);
    let view = document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let output = build_page_output(&view, &page, &BuildOptions::default()).unwrap();
    assert!(String::from_utf8(output.stream).unwrap().contains("<20> Tj"));
}
