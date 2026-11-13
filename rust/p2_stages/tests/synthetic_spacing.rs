use babeldoc_ir_core as ir;
use babeldoc_p2_stages::{
    NoopCancellation, NoopProgress, StageContext,
    font_mapper::font_mapper_from_asset_paths,
    stages::typesetting::{PreviewRejection, PreviewUnit, Typesetting},
};

fn bounds(x: f64, x2: f64) -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit { x: Some(x), y: Some(20.0), x2: Some(x2), y2: Some(30.0) }.into_value()
}

fn style() -> ir::FieldValue<ir::types::PdfStyle> {
    ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()
}

fn character(text: &str, id: Option<i64>, marker: Option<bool>, x: f64, width: f64) -> ir::FieldValue<ir::types::PdfCharacter> {
    ir::PdfCharacterInit {
        char_unicode: Some(text.into()), pdf_character_id: id, synthetic_spacing: marker,
        r#box: Some(bounds(x, x + width)),
        visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(x, x + width)) }.into_value()),
        pdf_style: Some(style()), advance: Some(width), ..Default::default()
    }.into_value()
}

fn document(formula: bool, relocate: bool, width: f64, invalid: Option<(&str, Option<bool>)>) -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        ..Default::default()
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let characters = if let Some((text, marker)) = invalid {
            vec![character(text, None, marker, 10.0, 2.0)]
        } else {
            vec![
                character("A", Some(65), None, 10.0, 5.0),
                character(" ", Some(32), None, 15.0, 2.5),
                character(" ", None, Some(true), 17.5, width),
                character(" ", Some(32), None, 17.5 + width, 2.5),
                character("B", Some(66), None, 20.0 + width, 5.0),
            ]
        };
        let source = if formula {
            ir::PdfParagraphCompositionInit {
                pdf_formula: Some(ir::PdfFormulaInit {
                    r#box: Some(bounds(10.0, 25.0 + width)), x_offset: Some(0.0), y_offset: Some(0.0),
                    x_advance: Some(15.0 + width), pdf_character: characters, ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()
        } else {
            ir::PdfParagraphCompositionInit {
                pdf_same_style_characters: Some(ir::PdfSameStyleCharactersInit {
                    pdf_character: characters, pdf_style: Some(style()), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()
        };
        let mut compositions = vec![];
        if relocate {
            compositions.push(ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some("Q".into()), pdf_style: Some(style()), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value());
        }
        compositions.push(source);
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(0.0), x2: Some(200.0), y2: Some(100.0) }.into_value()),
            unicode: Some("QA   B".into()), pdf_paragraph_composition: compositions, ..Default::default()
        }.into_value()]).unwrap();
    }
    document
}

fn stage(progress: bool) -> Typesetting {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file(), "system test font unavailable");
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    Typesetting::new("en", mapper, progress, Default::default())
}

fn process(document: &mut ir::Document) -> Result<(), babeldoc_p2_stages::StageError> {
    stage(false).process(document, &mut StageContext::new(&NoopCancellation, &NoopProgress))
}

fn output_characters(document: &ir::Document) -> Vec<serde_json::Value> {
    let artifact = document.artifact().unwrap();
    let compositions = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"].as_array().unwrap();
    compositions.iter().flat_map(|part| {
        if let Some(character) = part.get("pdf_character").filter(|value| !value.is_null()) {
            vec![character.clone()]
        } else if let Some(formula) = part.get("pdf_formula").filter(|value| !value.is_null()) {
            formula["pdf_character"].as_array().unwrap().clone()
        } else {
            vec![]
        }
    }).collect()
}

#[test]
fn synthetic_spacing_survives_passthrough_and_relocation_with_width() {
    for (formula, relocate) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut document = document(formula, relocate, 11.91216, None);
        process(&mut document).unwrap();
        let characters = output_characters(&document);
        let spacing = characters.iter().find(|character| character["synthetic_spacing"] == true).unwrap();
        assert!(spacing["pdf_character_id"].is_null());
        assert_eq!(spacing["char_unicode"], " ");
        let x = spacing["box"]["x"].as_f64().unwrap();
        let x2 = spacing["box"]["x2"].as_f64().unwrap();
        assert!((x2 - x - 11.91216).abs() < 1e-8, "{formula} {relocate} {spacing}");
        let real_spaces: Vec<_> = characters.iter().filter(|character| character["pdf_character_id"] == 32).collect();
        assert_eq!(real_spaces.len(), 2);
        assert!(real_spaces.iter().all(|character| character["synthetic_spacing"] != true));
        assert!((real_spaces[1]["box"]["x"].as_f64().unwrap() - x2).abs() < 1e-8);
    }
}

#[test]
fn missing_real_glyph_id_fails_even_if_layout_would_drop_a_space() {
    for (text, marker) in [(" ", None), (" ", Some(false)), ("A", None), ("A", Some(true))] {
        let mut document = document(false, true, 0.0, Some((text, marker)));
        let error = process(&mut document).unwrap_err();
        assert!(error.to_string().contains("typesetting.missing_character_id"), "{error}");
    }
}

#[test]
fn zero_width_synthetic_spacing_is_preserved_in_formula_passthrough() {
    let mut document = document(true, false, 0.0, None);
    process(&mut document).unwrap();
    let characters = output_characters(&document);
    let spacing = characters.iter().find(|character| character["synthetic_spacing"] == true).unwrap();
    assert_eq!(spacing["box"]["x"], spacing["box"]["x2"]);
}

#[test]
fn preview_classifies_spacing_without_claiming_a_missing_glyph() {
    for (formula, width) in [(false, 11.91216), (false, 0.0), (true, 11.91216), (true, 0.0)] {
        let document = document(formula, true, width, None);
        let (previews, fork) = stage(true).audit_fork_with_document(
            &document, &[(0, 0)], &mut StageContext::new(&NoopCancellation, &NoopProgress)
        ).unwrap();
        let preview = &previews[0];
        assert!(fork.is_some(), "{preview:?}");
        assert!(!preview.rejections.contains(&PreviewRejection::MissingGlyph), "{preview:?}");
        assert!(!preview.rejections.contains(&PreviewRejection::InvalidSpacingEvidence), "{preview:?}");
        assert!(!preview.rejections.contains(&PreviewRejection::InvalidFormulaEvidence), "{preview:?}");
        if formula {
            assert!(preview.units.iter().any(|unit| matches!(unit, PreviewUnit::Formula { .. })));
            assert!(preview.formulas[0].character_bounds.iter().all(Option::is_some));
        } else {
            assert!(preview.units.iter().any(|unit| matches!(unit, PreviewUnit::Spacing { bounds: Some(bounds) } if (bounds.right - bounds.left - width).abs() < 1e-8)));
        }
    }
}
