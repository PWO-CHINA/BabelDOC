use super::add_spaces;
use babeldoc_ir_core as ir;

fn bounds(x: f64, x2: f64, y: f64) -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit { x: Some(x), y: Some(y), x2: Some(x2), y2: Some(y + 10.0) }.into_value()
}

fn character(x: f64, y: f64) -> ir::FieldValue<ir::types::PdfCharacter> {
    ir::PdfCharacterInit {
        char_unicode: Some(" ".into()), pdf_character_id: Some(32),
        r#box: Some(bounds(x, x + 2.5, y)),
        visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(x, x + 2.5, y)) }.into_value()),
        pdf_style: Some(ir::PdfStyleInit { font_id: Some("F1".into()), font_size: Some(10.0), ..Default::default() }.into_value()),
        ..Default::default()
    }.into_value()
}

fn inserted_spacing(second_x: f64, second_y: f64) -> serde_json::Value {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        ..Default::default()
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit {
                    pdf_character: vec![character(10.0, 90.0), character(second_x, second_y)],
                    ..Default::default()
                }.into_value()),
                ..Default::default()
            }.into_value()],
            ..Default::default()
        }.into_value()]).unwrap();
        let paragraph = page.pdf_paragraph_at(&view, 0).unwrap();
        add_spaces(&paragraph, &mut view).unwrap();
    }
    document.artifact().unwrap()["document"]["page"][0]["pdf_paragraph"][0]
        ["pdf_paragraph_composition"][0]["pdf_line"]["pdf_character"].clone()
}

#[test]
fn synthetic_spacing_is_marked_between_real_spaces_and_on_line_wrap() {
    for (second_x, second_y, expected_width) in [(24.41216, 90.0, 11.91216), (150.0, 70.0, 0.0)] {
        let characters = inserted_spacing(second_x, second_y);
        let characters = characters.as_array().unwrap();
        assert_eq!(characters.len(), 3);
        for index in [0, 2] {
            assert_eq!(characters[index]["pdf_character_id"], 32);
            assert_ne!(characters[index]["synthetic_spacing"], true);
        }
        let spacing = &characters[1];
        assert_eq!(spacing["synthetic_spacing"], true);
        assert!(spacing["pdf_character_id"].is_null());
        assert_eq!(spacing["char_unicode"], " ");
        let width = spacing["box"]["x2"].as_f64().unwrap() - spacing["box"]["x"].as_f64().unwrap();
        assert!((width - expected_width).abs() < 1e-8);
        assert!((spacing["advance"].as_f64().unwrap() - expected_width).abs() < 1e-8);
    }
}
