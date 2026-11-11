use std::sync::Arc;

use babeldoc_ir_core as ir;
use babeldoc_p2_stages::shared::formula_helper::FontCoverage;
use babeldoc_p2_stages::stages::{
    styles_and_formulas, styles_and_formulas_config::StylesAndFormulasConfig,
};
use babeldoc_p2_stages::{NoopCancellation, NoopProgress, StageContext};

struct CompleteCoverage;

impl FontCoverage for CompleteCoverage {
    fn has_char(&self, _: &str) -> bool {
        true
    }
}

fn bounds() -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit {
        x: Some(0.0),
        y: Some(0.0),
        x2: Some(20.0),
        y2: Some(10.0),
    }
    .into_value()
}

fn characters(text: &str) -> Vec<ir::FieldValue<ir::types::PdfCharacter>> {
    text.chars()
        .map(|character| {
            ir::PdfCharacterInit {
                char_unicode: Some(character.to_string()),
                r#box: Some(bounds()),
                visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds()) }.into_value()),
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
        })
        .collect()
}

fn formula(text: &str) -> ir::FieldValue<ir::types::PdfParagraphComposition> {
    ir::PdfParagraphCompositionInit {
        pdf_formula: Some(
            ir::PdfFormulaInit {
                pdf_character: characters(text),
                r#box: Some(bounds()),
                line_id: Some(0),
                ..Default::default()
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value()
}

fn text(value: &str) -> ir::FieldValue<ir::types::PdfParagraphComposition> {
    ir::PdfParagraphCompositionInit {
        pdf_line: Some(
            ir::PdfLineInit {
                pdf_character: characters(value),
                r#box: Some(bounds()),
                ..Default::default()
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value()
}

fn process(compositions: Vec<ir::FieldValue<ir::types::PdfParagraphComposition>>) -> String {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(
            Default::default(),
        ))))],
        total_pages: Some(1),
    }));
    let mut document =
        ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_pdf_paragraph(
            &mut view,
            vec![ir::PdfParagraphInit {
                pdf_paragraph_composition: compositions,
                unicode: Some("synthetic reference identifier".into()),
                ..Default::default()
            }
            .into_value()],
        )
        .unwrap();
    }
    let config = StylesAndFormulasConfig {
        formula_matcher: None,
        font_mapper: Arc::new(CompleteCoverage),
        character_pattern_hits: None,
        font_pattern_hits: None,
        skip_formula_offset_calculation: true,
        ocr_workaround: false,
        remove_non_formula_lines: false,
        figure_table_protection_threshold: None,
        non_formula_line_iou_threshold: None,
    };
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    styles_and_formulas::process(&config, &mut document, &mut context).unwrap();
    let artifact = document.artifact().unwrap();
    let mut output = String::new();
    collect_text(
        &artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"],
        &mut output,
    );
    output
}

fn collect_text(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Object(values) => {
            if let Some(character) = values.get("char_unicode").and_then(serde_json::Value::as_str)
            {
                output.push_str(character);
            } else {
                for child in values.values() {
                    collect_text(child, output);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_text(child, output);
            }
        }
        _ => {}
    }
}

#[test]
fn overlapping_formula_groups_do_not_move_across_text() {
    assert_eq!(
        process(vec![
            formula("10"),
            text("."),
            formula("1234"),
            text("/example."),
            formula("5678"),
        ]),
        "10.1234/example.5678"
    );
}

#[test]
fn adjacent_formula_groups_still_preserve_order() {
    assert_eq!(process(vec![formula("12"), formula("34")]), "1234");
}
