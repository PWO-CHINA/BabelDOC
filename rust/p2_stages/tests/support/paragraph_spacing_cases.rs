use super::add_spaces;
use babeldoc_ir_core as ir;

fn bounds(x: f64, x2: f64, y: f64) -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit {
        x: Some(x),
        y: Some(y),
        x2: Some(x2),
        y2: Some(y + 10.0),
    }
    .into_value()
}

fn character(value: &str, x: f64, y: f64) -> ir::FieldValue<ir::types::PdfCharacter> {
    ir::PdfCharacterInit {
        char_unicode: Some(value.into()),
        pdf_character_id: Some(1),
        r#box: Some(bounds(x, x + 5.0, y)),
        visual_bbox: Some(
            ir::VisualBboxInit {
                r#box: Some(bounds(x, x + 5.0, y)),
            }
            .into_value(),
        ),
        pdf_style: Some(
            ir::PdfStyleInit {
                font_id: Some("regular".into()),
                font_size: Some(10.0),
                ..Default::default()
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value()
}

fn paragraph(
    characters: Vec<ir::FieldValue<ir::types::PdfCharacter>>,
) -> (Vec<(String, f64)>, f64) {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(
            ir::NodeData::Page(Default::default()),
        )))],
        total_pages: Some(1),
    }));
    let mut document =
        ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_pdf_paragraph(
            &mut view,
            vec![
                ir::PdfParagraphInit {
                    pdf_paragraph_composition: vec![
                        ir::PdfParagraphCompositionInit {
                            pdf_line: Some(
                                ir::PdfLineInit {
                                    pdf_character: characters,
                                    ..Default::default()
                                }
                                .into_value(),
                            ),
                            ..Default::default()
                        }
                        .into_value(),
                    ],
                    ..Default::default()
                }
                .into_value(),
            ],
        )
        .unwrap();
        let paragraph = page.pdf_paragraph_at(&view, 0).unwrap();
        add_spaces(&paragraph, &mut view).unwrap();
    }
    let artifact = document.artifact().unwrap();
    let characters = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_line"]["pdf_character"].as_array().unwrap();
    let values: Vec<_> = characters
        .iter()
        .map(|item| {
            (
                item["char_unicode"].as_str().unwrap().to_owned(),
                item["box"]["x2"].as_f64().unwrap(),
            )
        })
        .collect();
    let max_x = values
        .iter()
        .map(|(_, x)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    (values, max_x)
}

#[test]
fn line_wrap_spacing_does_not_expand_beyond_page() {
    let (characters, max_x) = paragraph(vec![
        character("A", 20.0, 90.0),
        character("B", 220.0, 90.0),
        character("C", 600.0, 90.0),
        character("D", 20.0, 90.0),
    ]);
    assert!(characters.iter().any(|(value, _)| value == " "));
    assert_eq!(max_x, 605.0, "spacing extends to {max_x}");
}

#[test]
fn ordinary_word_gap_still_inserts_space() {
    let (characters, max_x) =
        paragraph(vec![character("A", 20.0, 90.0), character("B", 32.0, 90.0)]);
    assert_eq!(
        characters
            .iter()
            .map(|(value, _)| value.as_str())
            .collect::<String>(),
        "A B"
    );
    assert_eq!(max_x, 37.0);
}

#[test]
fn adjacent_letters_remain_adjacent() {
    let (characters, _) =
        paragraph(vec![character("A", 20.0, 90.0), character("B", 25.0, 90.0)]);
    assert_eq!(characters, [("A".into(), 25.0), ("B".into(), 30.0)]);
}

#[test]
fn newline_with_forward_offset_has_zero_width_space() {
    let (characters, _) = paragraph(vec![
        character("A", 20.0, 90.0),
        character("B", 150.0, 70.0),
    ]);
    assert_eq!(
        characters,
        [("A".into(), 25.0), (" ".into(), 25.0), ("B".into(), 155.0)]
    );
}

#[test]
fn small_same_line_overlap_does_not_add_space() {
    let (characters, _) = paragraph(vec![character("A", 20.0, 90.0), character("B", 23.0, 90.0)]);
    assert_eq!(characters, [("A".into(), 25.0), ("B".into(), 28.0)]);
}
