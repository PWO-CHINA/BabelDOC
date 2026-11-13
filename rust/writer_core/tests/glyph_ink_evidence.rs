use babeldoc_font_core::glyph_bounds_evidence::{
    Bounds, Failure, GlyphBoundsEvidence, OutlineObservation,
};
use babeldoc_writer_core::{
    GlyphInkBox, GlyphInkInput, GlyphInkObservation, GlyphInkUncertain, InkStateAssertions,
    observe_glyph_ink,
};

fn state() -> InkStateAssertions {
    InkStateAssertions {
        horizontal_unit_text_matrix: true,
        default_horizontal_scale_and_rise: true,
        fill_only_no_stroke: true,
        no_other_transform: true,
        simple_static_unhinted_truetype: true,
        exact_half_unit_extraction: true,
    }
}
fn glyph() -> GlyphBoundsEvidence {
    GlyphBoundsEvidence {
        glyph_id: 1,
        units_per_em: 1000,
        observation: OutlineObservation::Bounds(Bounds {
            x_min: -101.0,
            y_min: -201.5,
            x_max: 503.0,
            y_max: 701.0,
        }),
    }
}
fn input() -> GlyphInkInput<'static> {
    GlyphInkInput {
        x: "-3.000001",
        y: "2.000000",
        font_size: "12.500000",
        crop_translation_x: "-1.000000",
        crop_translation_y: "0.000001",
        confirmed_units_per_em: 1000,
        glyph: Ok(glyph()),
        state: state(),
    }
}

#[test]
fn outward_rounding_is_signed_and_includes_translation() {
    assert_eq!(
        observe_glyph_ink(input()),
        Ok(GlyphInkObservation::UnclippedBounds(GlyphInkBox {
            x_min: -5_262_501,
            y_min: -518_749,
            x_max: 2_287_499,
            y_max: 10_762_501
        }))
    );
}

#[test]
fn fractional_edges_round_outward_and_overflow_fails_closed() {
    let mut value = input();
    value.x = "0.000000";
    value.y = "0.000000";
    value.crop_translation_x = "0.000000";
    value.crop_translation_y = "0.000000";
    value.font_size = "0.000001";
    value.confirmed_units_per_em = 16;
    value.glyph = Ok(GlyphBoundsEvidence {
        glyph_id: 1,
        units_per_em: 16,
        observation: OutlineObservation::Bounds(Bounds {
            x_min: -0.5, y_min: 0.5, x_max: -0.5, y_max: 0.5,
        }),
    });
    assert_eq!(observe_glyph_ink(value), Ok(GlyphInkObservation::UnclippedBounds(GlyphInkBox {
        x_min: -1, y_min: 0, x_max: 0, y_max: 1,
    })));
    let mut value = input();
    value.x = "170141183460469231731687303715884.105727";
    value.crop_translation_x = "0.000001";
    assert_eq!(observe_glyph_ink(value), Err(GlyphInkUncertain::Overflow));
    let mut value = input();
    value.x = "-170141183460469231731687303715884.105728";
    assert_eq!(observe_glyph_ink(value), Err(GlyphInkUncertain::Overflow));
}

#[test]
fn unknown_conditions_and_bad_values_fail_closed() {
    let mut value = input();
    value.state.fill_only_no_stroke = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.state.default_horizontal_scale_and_rise = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.state.no_other_transform = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.state.horizontal_unit_text_matrix = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.state.simple_static_unhinted_truetype = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.state.exact_half_unit_extraction = false;
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::UnsupportedOrUnknownState)
    );
    let mut value = input();
    value.x = "nan";
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::InvalidNumber)
    );
    let mut value = input();
    value.font_size = "12.5";
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::InvalidNumber)
    );
    let mut value = input();
    value.font_size = "170141183460469231731687303715884.000000";
    assert_eq!(observe_glyph_ink(value), Err(GlyphInkUncertain::Overflow));
    let mut value = input();
    value.glyph = Err(Failure::UnsupportedComposite);
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::Glyph(Failure::UnsupportedComposite))
    );
    let mut value = input();
    value.glyph = Err(Failure::MissingGlyph);
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::Glyph(Failure::MissingGlyph))
    );
    let mut value = input();
    let mut bad = glyph();
    bad.glyph_id = 0;
    value.glyph = Ok(bad);
    assert_eq!(
        observe_glyph_ink(value),
        Err(GlyphInkUncertain::MissingMapping)
    );
    let mut value = input();
    value.confirmed_units_per_em = 2048;
    assert_eq!(observe_glyph_ink(value), Err(GlyphInkUncertain::UnitsPerEm));
    for coordinate in [0.25, f32::NAN, 32768.5] {
        let mut value = input();
        let mut bad = glyph();
        if let OutlineObservation::Bounds(ref mut bounds) = bad.observation {
            bounds.x_min = coordinate;
        }
        value.glyph = Ok(bad);
        assert_eq!(
            observe_glyph_ink(value),
            Err(GlyphInkUncertain::InvalidBounds)
        );
    }
    let mut value = input();
    let mut empty = glyph();
    empty.observation = OutlineObservation::NoOutline;
    value.glyph = Ok(empty);
    assert_eq!(observe_glyph_ink(value), Ok(GlyphInkObservation::NoOutline));
}

#[test]
fn writer_tokens_and_public_synthetic_font() {
    use babeldoc_ir_core as ir;
    use babeldoc_writer_core::{BuildOptions, FontFace, build_page_output};
    fn node(data: ir::NodeData) -> ir::Edge {
        ir::Edge::Inline(Box::new(ir::Node::new(data)))
    }
    fn bounds(x: f64, y: f64) -> ir::Edge {
        node(ir::NodeData::Box(ir::DataBox {
            x: Some(x),
            y: Some(y),
            x2: Some(x + 10.0),
            y2: Some(y + 10.0),
        }))
    }
    let character = node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: Some(1),
        char_unicode: Some("A".into()),
        render_order: Some(1),
        sub_render_order: Some(0),
        r#box: Some(bounds(-3.000001, 2.0)),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
            font_id: Some("F1".into()),
            font_size: Some(12.5),
            ..Default::default()
        }))),
        ..Default::default()
    }));
    let page = node(ir::NodeData::Page(ir::DataPage {
        page_number: Some(0),
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox {
            r#box: Some(bounds(1.0, -0.000001)),
        }))),
        pdf_character: vec![character],
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some("F1".into()),
            encoding_length: Some(2),
            ..Default::default()
        }))],
        ..Default::default()
    }));
    let mut document = ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![page],
            ..Default::default()
        })),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let view = document.view::<ir::ReadOnlyView>();
    let output = build_page_output(
        &view,
        &view.document().page_at(&view, 0).unwrap(),
        &BuildOptions::default(),
    )
    .unwrap();
    let stream = String::from_utf8(output.stream).unwrap();
    let tokens: Vec<_> = stream.split_whitespace().collect();
    let cm = tokens.iter().position(|token| *token == "cm").unwrap();
    let tf = tokens.iter().position(|token| *token == "Tf").unwrap();
    let tm = tokens.iter().position(|token| *token == "Tm").unwrap();
    assert_eq!(
        &tokens[cm - 6..cm - 2],
        &["1.000000", "0.000000", "0.000000", "1.000000"]
    );
    assert_eq!(&tokens[tm - 6..tm - 2], &["1", "0", "0", "1"]);
    let path = std::env::var_os("GLYPH_INK_TEST_FONT").expect("public synthetic fixture required");
    let face = FontFace::load("synthetic", path).unwrap();
    let actual = face.glyph_bounds_evidence(1).unwrap();
    assert_eq!(
        face.glyph_bounds_evidence(3),
        Err(Failure::UnsupportedComposite)
    );
    assert_eq!(face.glyph_bounds_evidence(4), Err(Failure::MissingGlyph));
    assert_eq!(
        face.glyph_bounds_evidence(2).unwrap().observation,
        OutlineObservation::NoOutline
    );
    assert_eq!(
        actual.observation,
        OutlineObservation::Bounds(Bounds {
            x_min: -101.0,
            y_min: -201.0,
            x_max: 503.0,
            y_max: 701.0
        })
    );
    let result = observe_glyph_ink(GlyphInkInput {
        x: tokens[tm - 2],
        y: tokens[tm - 1],
        font_size: tokens[tf - 1],
        crop_translation_x: tokens[cm - 2],
        crop_translation_y: tokens[cm - 1],
        confirmed_units_per_em: 1000,
        glyph: Ok(actual),
        state: state(),
    })
    .unwrap();
    assert_eq!(
        result,
        GlyphInkObservation::UnclippedBounds(GlyphInkBox {
            x_min: -5_262_501,
            y_min: -512_499,
            x_max: 2_287_499,
            y_max: 10_762_501
        })
    );
}
