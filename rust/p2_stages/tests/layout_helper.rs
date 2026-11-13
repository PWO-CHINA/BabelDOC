use babeldoc_p2_stages::shared::layout_helper::{
    SpacingUnit, UnicodePart, get_char_unicode_string, inferred_word_spaces, normalize_source_text,
};
use babeldoc_il_types::schema::{Box as PdfBox, PdfCharacter, PdfStyle};

fn character(text: &str, left: f64, right: f64, bottom: f64, top: f64, font: &str) -> PdfCharacter {
    PdfCharacter {
        char_unicode: Some(text.into()),
        r#box: Some(PdfBox { x: Some(left), x2: Some(right), y: Some(bottom), y2: Some(top) }.into()),
        pdf_style: Some(PdfStyle { font_id: Some(font.into()), font_size: Some(9.0), ..Default::default() }.into()),
        ..Default::default()
    }
}

#[test]
fn local_space_width_recovers_asp_word_and_ignores_kerning() {
    let chars = [
        character("s", 232.75289792, 236.52775232, 63.84269, 72.80909, "body"),
        character("a", 238.0628, 242.1604448, 63.84269, 72.80909, "body"),
        character("n", 242.15775488, 247.36723328, 63.84269, 72.80909, "body"),
        character("d", 247.36633664, 252.13646144, 63.84269, 72.80909, "body"),
        character(" ", 252.13646144, 253.67240576, 63.84269, 72.80909, "body"),
        character("s", 253.67240576, 257.447, 63.84269, 72.80909, "body"),
        character("t", 257.47, 260.1, 63.84269, 72.80909, "body"),
    ];
    let parts: Vec<_> = chars.iter().map(UnicodePart::Character).collect();
    assert_eq!(get_char_unicode_string(&parts).unwrap(), "s and st");
}

#[test]
fn font_and_line_scoping_prevents_unrelated_space_widths() {
    let units = [
        SpacingUnit { text: "a", bounds: Some((0.0, 0.0, 4.0, 10.0)), font_id: Some("body"), font_size: Some(10.0) },
        SpacingUnit { text: "b", bounds: Some((4.5, 0.0, 8.0, 10.0)), font_id: Some("body"), font_size: Some(10.0) },
        SpacingUnit { text: " ", bounds: Some((8.0, 0.0, 8.2, 10.0)), font_id: Some("other"), font_size: Some(10.0) },
        SpacingUnit { text: " ", bounds: Some((0.0, -20.0, 0.2, -10.0)), font_id: Some("body"), font_size: Some(10.0) },
    ];
    assert!(!inferred_word_spaces(&units)[1]);
}

#[test]
fn source_normalization_expands_ligatures_without_flattening_scripts() {
    assert_eq!(normalize_source_text("  eﬀect\u{a0}ﬁnd 10⁻³ H₂O  "), " effect find 10⁻³ H₂O ");
}

#[test]
fn stored_asp_pre_ir_recovers_missing_space() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.crossboundary-rootcause-20261112/selection/06-Asp-pre/pretranslation-artifact.json");
    if !path.exists() { return; }
    let document: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let paragraphs = document["document"]["page"][0]["pdf_paragraph"].as_array().unwrap();
    let composition = &paragraphs[6]["pdf_paragraph_composition"][0];
    let characters = composition["pdf_same_style_characters"]["pdf_character"].as_array().unwrap();
    let characters: Vec<PdfCharacter> = characters.iter().map(|value| {
        let bounds = &value["box"];
        let style = &value["pdf_style"];
        PdfCharacter {
            char_unicode: value["char_unicode"].as_str().map(str::to_owned),
            r#box: Some(PdfBox { x: bounds["x"].as_f64(), y: bounds["y"].as_f64(), x2: bounds["x2"].as_f64(), y2: bounds["y2"].as_f64() }.into()),
            pdf_style: Some(PdfStyle { font_id: style["font_id"].as_str().map(str::to_owned), font_size: style["font_size"].as_f64(), ..Default::default() }.into()),
            ..Default::default()
        }
    }).collect();
    let parts: Vec<_> = characters.iter().map(UnicodePart::Character).collect();
    let result = get_char_unicode_string(&parts).unwrap();
    assert!(result.contains("tools and"), "{result}");
    assert!(!result.contains("toolsand"));
}
