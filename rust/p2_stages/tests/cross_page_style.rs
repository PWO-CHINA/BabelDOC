use babeldoc_p2_stages::translation::cross_page_style::{composition_styles_equivalent as boundary_styles_equivalent, whole_sentence_boundary_styles_equivalent, CompositionLocation};
use babeldoc_p2_stages::translation::ordinary_translation::{InputPlaceholder, RichInput, TranslateInput};
use serde_json::{json, Value};

fn location(page: usize) -> CompositionLocation {
    CompositionLocation { page, paragraph: 0, composition: 0 }
}

fn fixture() -> Value {
    let page = |id: &str, xref: i64| {
        let style = json!({"font_id": id, "font_size": 10.0, "graphic_state": null});
        json!({
            "pdf_font": [{"font_id": id, "xref_id": xref}],
            "pdf_xobject": [],
            "pdf_paragraph": [{
                "xobj_id": 0,
                "pdf_paragraph_composition": [{"pdf_same_style_characters": {
                    "pdf_style": style,
                    "pdf_character": [{"char_unicode": "a", "pdf_style": style, "xobj_id": 0, "vertical": false,
                        "scale": null, "visual_roundtrip_text_render_mode": null, "visual_roundtrip_text_scale_x": null, "visual_roundtrip_text_scale_y": null}]
                }}]
            }]
        })
    };
    json!({"document": {"page": [page("first", 42), page("second", 42)]}})
}

fn rich(composition_index: usize) -> InputPlaceholder {
    InputPlaceholder::Rich(RichInput {
        id: 1, composition_index, left: "<style>".into(), right: "</style>".into(),
        left_pattern: "<style>".into(), right_pattern: "</style>".into(), source_text: "a".into(),
    })
}

#[test]
fn accepts_only_matching_proven_font_and_size() {
    let mut document = fixture();
    assert!(boundary_styles_equivalent(&document, location(0), location(1)));
    document["document"]["page"][1]["pdf_font"][0]["xref_id"] = json!(43);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document = fixture();
    document["document"]["page"][1]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_style"]["font_size"] = json!(9.9);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    assert!(!boundary_styles_equivalent(&document, location(0), location(0)));
}

#[test]
fn derives_boundary_compositions_from_translation_placeholders() {
    let document = fixture();
    let input = TranslateInput { text: "<style>a</style>".into(), placeholders: vec![rich(0)], original_tokens: vec![] };
    assert!(whole_sentence_boundary_styles_equivalent(&document, 0, 0, &input, 1, 0, &input));
    let mut wrong = input.clone();
    wrong.placeholders = vec![rich(99)];
    assert!(!whole_sentence_boundary_styles_equivalent(&document, 0, 0, &wrong, 1, 0, &input));
    let mut wrong = input.clone();
    let InputPlaceholder::Rich(placeholder) = &mut wrong.placeholders[0] else { unreachable!() };
    placeholder.source_text = "different".into();
    assert!(!whole_sentence_boundary_styles_equivalent(&document, 0, 0, &wrong, 1, 0, &input));
}

#[test]
fn rejects_graphic_state_and_unknown_character_transforms() {
    let mut document = fixture();
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_style"]["graphic_state"] = json!({"passthrough_per_char_instruction": "/GS1 gs"});
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_style"]["graphic_state"] = json!({});
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    for (field, value) in [("vertical", json!(true)), ("scale", json!(0.5)), ("visual_roundtrip_text_render_mode", json!(3)), ("xobj_id", json!(1))] {
        let mut document = fixture();
        document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_character"][0][field] = value;
        assert!(!boundary_styles_equivalent(&document, location(0), location(1)), "{field}");
    }
}

#[test]
fn rejects_missing_ambiguous_or_unrecognized_evidence() {
    let mut document = fixture();
    document["document"]["page"][1]["pdf_font"][0]["xref_id"] = Value::Null;
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document = fixture();
    let font = document["document"]["page"][1]["pdf_font"][0].clone();
    document["document"]["page"][1]["pdf_font"].as_array_mut().unwrap().push(font);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document = fixture();
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_character"] = json!([]);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document = fixture();
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_character"][0].as_object_mut().unwrap().remove("scale");
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    document = fixture();
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_formula"] = json!({});
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    assert!(!boundary_styles_equivalent(&document, location(0), location(9)));
}

#[test]
fn rejects_unproven_xobject_scope_even_when_font_resource_matches() {
    let mut document = fixture();
    document["document"]["page"][0]["pdf_xobject"] = json!([{"xobj_id": 7, "pdf_font": [{"font_id": "first", "xref_id": 42}]}]);
    document["document"]["page"][0]["pdf_paragraph"][0]["xobj_id"] = json!(7);
    document["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_same_style_characters"]["pdf_character"][0]["xobj_id"] = json!(7);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
    let mut document = fixture();
    document["document"]["page"][0]["pdf_xobject"] = json!([{"xobj_id": 0, "pdf_font": []}]);
    assert!(!boundary_styles_equivalent(&document, location(0), location(1)));
}
