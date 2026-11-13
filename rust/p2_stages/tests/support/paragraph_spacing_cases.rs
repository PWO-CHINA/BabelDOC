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

#[test]
fn source_line_end_space_survives_before_following_text() {
    source_line_end_space_case(false);
}

#[test]
fn source_line_end_space_survives_intermediate_whitespace_only_line() {
    source_line_end_space_case(true);
}

fn source_line_end_space_case(intermediate_blank_line: bool) {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let mut separator = ir::PdfCharacterInit {
            char_unicode: Some(" ".into()), pdf_character_id: Some(32), render_order: Some(7),
            r#box: Some(bounds(55.0, 57.5, 90.0)),
            visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(55.0, 57.5, 90.0)) }.into_value()),
            pdf_style: Some(ir::PdfStyleInit { font_id: Some("regular".into()), font_size: Some(10.0), ..Default::default() }.into_value()),
            advance: Some(2.5), ..Default::default()
        };
        let mut compositions = vec![ir::PdfParagraphCompositionInit {
            pdf_line: Some(ir::PdfLineInit { pdf_character: vec![character(")", 50.0, 90.0), separator.into_value()], ..Default::default() }.into_value()),
            ..Default::default()
        }.into_value()];
        if intermediate_blank_line {
            compositions.push(ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit {
                    pdf_character: vec![character(" ", 12.0, 84.0)], ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value());
        }
        separator = ir::PdfCharacterInit {
            char_unicode: Some(" ".into()), pdf_character_id: Some(32),
            r#box: Some(bounds(15.0, 17.5, 80.0)),
            visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(15.0, 17.5, 80.0)) }.into_value()),
            pdf_style: Some(ir::PdfStyleInit { font_id: Some("regular".into()), font_size: Some(10.0), ..Default::default() }.into_value()),
            ..Default::default()
        };
        compositions.push(ir::PdfParagraphCompositionInit {
            pdf_line: Some(ir::PdfLineInit { pdf_character: vec![character("f", 10.0, 80.0), separator.into_value()], ..Default::default() }.into_value()),
            ..Default::default()
        }.into_value());
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            pdf_paragraph_composition: compositions, ..Default::default()
        }.into_value()]).unwrap();
        let paragraph = page.pdf_paragraph_at(&view, 0).unwrap();
        super::spacing(&paragraph, &mut view).unwrap();
    }
    let artifact = document.artifact().unwrap();
    let parts = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "intermediate whitespace-only lines must be removed");
    let first = parts[0]["pdf_line"]["pdf_character"].as_array().unwrap();
    assert_eq!(first.len(), 2, "the real word separator must survive line-end normalization");
    assert_eq!(first[1]["char_unicode"], " ");
    assert_eq!(first[1]["pdf_character_id"], 32);
    assert_eq!(first[1]["render_order"], 7);
    assert_eq!(first[1]["box"]["x"], 55.0);
    assert_eq!(first[1]["box"]["x2"], 57.5);
    assert_eq!(parts[1]["pdf_line"]["pdf_character"].as_array().unwrap().len(), 1,
        "the paragraph's final trailing whitespace is still trimmed");
    assert_eq!(parts[1]["pdf_line"]["pdf_character"][0]["char_unicode"], "f");
}
