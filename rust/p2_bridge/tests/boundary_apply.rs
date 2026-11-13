use _rust_il::font_mapper::{self, FontMapperHandle};
use babeldoc_il_types::RuntimeBool;
use babeldoc_ir_core as ir;
use babeldoc_p2_stages::translation::cross_boundary::{
    Location, OutputPart, ResolvedParagraph, Resource,
};
use sha2::{Digest, Sha256};
use std::path::Path;

#[path = "../src/boundary_apply.rs"]
mod boundary_apply;

fn apply(
    document: &mut ir::Document,
    outputs: &[ResolvedParagraph],
    mapper: &FontMapperHandle,
) -> pyo3::PyResult<()> {
    boundary_apply::apply_with_evidence(document, outputs, mapper, None)
}

fn node(data: ir::NodeData) -> ir::Edge {
    ir::Edge::Inline(Box::new(ir::Node::new(data)))
}

fn style(font_id: &str, size: f64, state: &str) -> ir::Edge {
    node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
        font_id: Some(font_id.into()),
        font_size: Some(size),
        graphic_state: Some(node(ir::NodeData::GraphicState(ir::DataGraphicState {
            passthrough_per_char_instruction: Some(ir::Instructions::Materialized(state.into())),
        }))),
    }))
}

fn font(id: &str, bold: bool) -> ir::Edge {
    node(ir::NodeData::PdfFont(ir::DataPdfFont {
        font_id: Some(id.into()),
        bold: Some(RuntimeBool::Bool(bold)),
        italic: Some(RuntimeBool::Bool(false)),
        monospace: Some(RuntimeBool::Bool(false)),
        serif: Some(RuntimeBool::Bool(false)),
        ..Default::default()
    }))
}

fn rich(id: &str, size: f64, state: &str) -> ir::Edge {
    node(ir::NodeData::PdfParagraphComposition(
        ir::DataPdfParagraphComposition {
            pdf_same_style_characters: Some(node(ir::NodeData::PdfSameStyleCharacters(
                ir::DataPdfSameStyleCharacters {
                    pdf_style: Some(style(id, size, state)),
                    ..Default::default()
                },
            ))),
            ..Default::default()
        },
    ))
}

fn formula(text: &str) -> ir::Edge {
    node(ir::NodeData::PdfParagraphComposition(
        ir::DataPdfParagraphComposition {
            pdf_formula: Some(node(ir::NodeData::PdfFormula(ir::DataPdfFormula {
                pdf_character: vec![node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
                    char_unicode: Some(text.into()),
                    ..Default::default()
                }))],
                ..Default::default()
            }))),
            ..Default::default()
        },
    ))
}

fn paragraph(scope: Option<i64>, parts: Vec<ir::Edge>) -> ir::Edge {
    node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph {
        xobj_id: scope,
        unicode: Some("source".into()),
        pdf_style: Some(style("F1", 10.0, "target")),
        pdf_paragraph_composition: parts,
        ..Default::default()
    }))
}

fn fixture() -> ir::Document {
    let first = ir::DataPage {
        pdf_font: vec![font("F1", false), font("F2", true)],
        pdf_xobject: vec![node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
            xobj_id: Some(7), xref_id: Some(87),
            pdf_font: vec![font("F1", true)],
            ..Default::default()
        }))],
        pdf_paragraph: vec![
            paragraph(None, vec![rich("F1", 11.0, "0 g"), formula("x")]),
            paragraph(None, vec![rich("F2", 15.0, "0.1 g"), formula("y")]),
            paragraph(Some(7), vec![rich("/F1", 17.0, "0.2 g"), formula("z")]),
            paragraph(None, vec![rich("F1", 10.0, "0.3 g")]),
            paragraph(Some(7), vec![rich("F1", 12.0, "0.4 g"), formula("q")]),
        ],
        page_number: Some(0),
        ..Default::default()
    };
    let second = ir::DataPage {
        page_number: Some(1),
        pdf_font: vec![font("F1", true)],
        pdf_paragraph: vec![paragraph(
            None,
            vec![rich("F1", 19.0, "0.5 g"), formula("w")],
        )],
        ..Default::default()
    };
    ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![
                node(ir::NodeData::Page(first)),
                node(ir::NodeData::Page(second)),
            ],
            ..Default::default()
        })),
        Default::default(),
        Default::default(),
    )
    .unwrap()
}

fn zhang_source() -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.crossboundary-acceptance-20261112/02-Zhang/work");
    std::fs::read_dir(root).unwrap().map(|entry| entry.unwrap().path().join("input.pdf"))
        .find(|path| path.is_file()).unwrap()
}

fn zhang_evidence() -> boundary_apply::boundary_graphics::Evidence {
    let path = zhang_source();
    let bytes = std::fs::read(&path).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let pdf = babeldoc_writer_core::PdfDocument::load_mem(&bytes).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    boundary_apply::boundary_graphics::Evidence::load(&path, &digest, &pages).unwrap()
}

fn set_source_state(document: &mut ir::Document, instruction: &str) {
    let mut view = document.view::<ir::TranslationView>();
    let state = view.document().page_at(&view, 1).unwrap()
        .pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap()
        .pdf_same_style_characters(&view).unwrap().unwrap().pdf_style(&view).unwrap().unwrap()
        .graphic_state(&view).unwrap().unwrap();
    state.set_passthrough_per_char_instruction(
        &mut view, Some(ir::Instructions::Materialized(instruction.into())),
    ).unwrap();
}

#[test]
fn zhang_matching_resources_are_transferred_without_dropping_style() {
    let mut document = fixture();
    let mapper = mapper();
    let evidence = zhang_evidence();
    set_source_state(&mut document, "/CS0 cs 0 0 0 scn /GS1 gs");
    let identity = graphic_identity(&document, 1, 0, 0, true);
    let outputs = [output(loc(0, 0), vec![text("A", Some(resource(1, 0, 0))),
        OutputPart::Formula(resource(0, 0, 1))])];
    boundary_apply::apply_with_evidence(&mut document, &outputs, &mapper, Some(&evidence)).unwrap();
    assert_eq!(graphic_identity(&document, 0, 0, 0, false), identity);
}

#[test]
fn zhang_resource_evidence_rejects_missing_and_unsupported_dependencies() {
    let evidence = zhang_evidence();
    let left = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: None };
    let right = boundary_apply::boundary_graphics::Scope { page_number: 1, form_xref: None };
    for instruction in [
        "/CS0 cs 0 0 0 scn /GS1 gs", "/C#530 cs 0 0 0 scn /G#531 gs",
    ] {
        assert!(evidence.transferable(instruction, left, right), "{instruction}");
    }
    for instruction in [
        "/missing cs 0 0 0 scn", "/missing gs", "/CS0 cs /pattern scn",
        "/CS0 cs 0 0 scn", "0 0 0 scn", "/GS1 gs 7 Tr", "/GS#XX gs",
        "/GS1 gs 1 0 0 1 0 0 cm", "/CS0 cs 0 0 0 scn /GS1 gs 9",
    ] {
        assert!(!evidence.transferable(instruction, left, right), "{instruction}");
    }
    let invalid_form = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: Some(38) };
    assert!(!evidence.transferable("/GS1 gs", left, invalid_form));
    let form = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: Some(87) };
    assert!(evidence.transferable("/CS0 cs 0 0 0 scn", left, form));
    assert!(!evidence.transferable("/GS1 gs", left, form));
    let mut document = fixture();
    set_source_state(&mut document, "/CS0 cs 0 0 0 scn");
    let outputs = [output(loc(0, 2), vec![text("A", Some(resource(1, 0, 0))),
        OutputPart::Formula(resource(0, 2, 1))])];
    boundary_apply::apply_with_evidence(&mut document, &outputs, &mapper(), Some(&evidence)).unwrap();
    let mut document = fixture();
    set_source_state(&mut document, "/GS1 gs");
    let before = document.artifact().unwrap();
    assert!(boundary_apply::apply_with_evidence(&mut document, &outputs, &mapper(), Some(&evidence)).is_err());
    assert_eq!(before, document.artifact().unwrap());
    let path = zhang_source();
    let pdf = babeldoc_writer_core::PdfDocument::load(&path).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    assert!(boundary_apply::boundary_graphics::Evidence::load(&path, &[0; 32], &pages).is_none());
}

fn changed_zhang_evidence(kind: &[u8], name: &[u8], remove: bool) -> boundary_apply::boundary_graphics::Evidence {
    let mut pdf = babeldoc_writer_core::PdfDocument::load(zhang_source()).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    let page = babeldoc_writer_core::page_object_with_map(&pdf, &pages, 1).unwrap();
    let resources = pdf.get_object(page).unwrap().as_dict().unwrap()
        .get(b"Resources").unwrap().as_reference().unwrap();
    let object = pdf.get_object(resources).unwrap().as_dict().unwrap().get(kind).unwrap()
        .as_dict().unwrap().get(name).unwrap().as_reference().unwrap();
    let replacement = pdf.add_object(pdf.get_object(object).unwrap().clone());
    let category = pdf.get_object_mut(resources).unwrap().as_dict_mut().unwrap()
        .get_mut(kind).unwrap().as_dict_mut().unwrap();
    if remove { category.remove(name); } else { category.set(name, replacement); }
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    boundary_apply::boundary_graphics::Evidence::from_bytes(&bytes, &digest, &pages).unwrap()
}

#[test]
fn rejects_same_name_different_object_or_missing_resource() {
    let left = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: None };
    let right = boundary_apply::boundary_graphics::Scope { page_number: 1, form_xref: None };
    for (kind, name, remove) in [
        (b"ColorSpace".as_slice(), b"CS0".as_slice(), false),
        (b"ColorSpace".as_slice(), b"CS0".as_slice(), true),
        (b"ExtGState".as_slice(), b"GS1".as_slice(), false),
        (b"ExtGState".as_slice(), b"GS1".as_slice(), true),
    ] {
        let evidence = changed_zhang_evidence(kind, name, remove);
        assert!(!evidence.transferable("/CS0 cs 0 0 0 scn /GS1 gs", left, right));
        let mut document = fixture();
        set_source_state(&mut document, "/CS0 cs 0 0 0 scn /GS1 gs");
        let before = document.artifact().unwrap();
        let outputs = [output(loc(0, 0), vec![text("A", Some(resource(1, 0, 0))),
            OutputPart::Formula(resource(0, 0, 1))])];
        assert!(boundary_apply::apply_with_evidence(&mut document, &outputs, &mapper(), Some(&evidence)).is_err());
        assert_eq!(before, document.artifact().unwrap());
    }
}

#[test]
fn rejects_unsupported_icc_alternate_and_soft_mask_even_when_shared() {
    let mut pdf = babeldoc_writer_core::PdfDocument::load(zhang_source()).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    let left = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: None };
    let right = boundary_apply::boundary_graphics::Scope { page_number: 1, form_xref: None };
    pdf.get_object_mut((39, 0)).unwrap().as_stream_mut().unwrap().dict.set("Alternate", "Pattern");
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let evidence = boundary_apply::boundary_graphics::Evidence::from_bytes(&bytes, &digest, &pages).unwrap();
    assert!(!evidence.transferable("/CS0 cs 0 0 0 scn", left, right));
    pdf.get_object_mut((41, 0)).unwrap().as_dict_mut().unwrap().set("SMask", "CustomMask");
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let evidence = boundary_apply::boundary_graphics::Evidence::from_bytes(&bytes, &digest, &pages).unwrap();
    assert!(!evidence.transferable("/GS1 gs", left, right));
}

#[test]
fn rejects_alpha_and_conflicting_default_color_space() {
    let mut pdf = babeldoc_writer_core::PdfDocument::load(zhang_source()).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    let left = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: None };
    let right = boundary_apply::boundary_graphics::Scope { page_number: 1, form_xref: None };
    pdf.get_object_mut((41, 0)).unwrap().as_dict_mut().unwrap().set("ca", 0);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let evidence = boundary_apply::boundary_graphics::Evidence::from_bytes(&bytes, &digest, &pages).unwrap();
    assert!(!evidence.transferable("/GS1 gs", left, right));
    let second_page = babeldoc_writer_core::page_object_with_map(&pdf, &pages, 1).unwrap();
    let resources = pdf.get_object(second_page).unwrap().as_dict().unwrap().get(b"Resources").unwrap().as_reference().unwrap();
    pdf.get_object_mut(resources).unwrap().as_dict_mut().unwrap().get_mut(b"ColorSpace").unwrap()
        .as_dict_mut().unwrap().set("DefaultRGB", (38, 0));
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let evidence = boundary_apply::boundary_graphics::Evidence::from_bytes(&bytes, &digest, &pages).unwrap();
    assert!(!evidence.transferable("/CS0 cs 0 0 0 scn", left, right));
}

#[test]
fn writer_keeps_zhang_graphics_and_indirect_resources() {
    let source = zhang_source();
    let mut pdf = babeldoc_writer_core::PdfDocument::load(&source).unwrap();
    let pages = babeldoc_writer_core::legacy_page_objects(&pdf);
    let before_page = babeldoc_writer_core::page_object_with_map(&pdf, &pages, 0).unwrap();
    let before = pdf.get_object(before_page).unwrap().as_dict().unwrap().get(b"Resources").unwrap().clone();
    let bounds = || node(ir::NodeData::Box(ir::DataBox {
        x: Some(10.0), y: Some(10.0), x2: Some(20.0), y2: Some(20.0),
    }));
    let character = node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: Some(65), char_unicode: Some("A".into()),
        r#box: Some(bounds()), pdf_style: Some(style("F1", 10.0, "/CS0 cs 0 0 0 scn /GS1 gs")),
        ..Default::default()
    }));
    let ir_page = ir::DataPage {
        page_number: Some(0), cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds()) }))),
        pdf_character: vec![character],
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some("F1".into()), encoding_length: Some(1), ..Default::default()
        }))],
        ..Default::default()
    };
    let document = ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(ir_page))], ..Default::default()
        })), Default::default(), Default::default(),
    ).unwrap();
    let view = document.read();
    let ir_page = view.document().page_at(&view, 0).unwrap();
    let page = babeldoc_writer_core::build_page_output(&view, &ir_page, &Default::default()).unwrap();
    babeldoc_writer_core::apply_page_output_with_page_objects(&mut pdf, &page, &pages).unwrap();
    let page_dictionary = pdf.get_object(before_page).unwrap().as_dict().unwrap();
    assert_eq!(page_dictionary.get(b"Resources").unwrap(), &before);
    let contents = pdf.get_object(page_dictionary.get(b"Contents").unwrap().as_reference().unwrap()).unwrap().as_stream().unwrap();
    let stream = contents.get_plain_content().unwrap();
    assert!(stream.windows(b"/CS0 cs 0 0 0 scn /GS1 gs".len())
        .any(|window| window == b"/CS0 cs 0 0 0 scn /GS1 gs"));
    let mut output = Vec::new();
    pdf.save_to(&mut output).unwrap();
    let digest: [u8; 32] = Sha256::digest(&output).into();
    let evidence = boundary_apply::boundary_graphics::Evidence::from_bytes(&output, &digest, &pages).unwrap();
    let left = boundary_apply::boundary_graphics::Scope { page_number: 0, form_xref: None };
    let right = boundary_apply::boundary_graphics::Scope { page_number: 1, form_xref: None };
    assert!(evidence.transferable("/CS0 cs 0 0 0 scn /GS1 gs", left, right));
}

fn mapper() -> FontMapperHandle {
    let candidates = if cfg!(windows) {
        ["C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/arialbd.ttf"]
    } else if cfg!(target_os = "macos") {
        [
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        ]
    } else {
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ]
    };
    assert!(
        candidates
            .iter()
            .all(|path| std::path::Path::new(path).is_file())
    );
    let family = serde_json::json!({
        "normal": ["normalface", "boldface"], "script": [],
        "fallback": ["normalface", "boldface"], "base": ["normalface"]
    });
    let paths = serde_json::json!({"normalface": candidates[0], "boldface": candidates[1]});
    FontMapperHandle::from_shared(
        babeldoc_p2_stages::font_mapper::font_mapper_from_asset_paths(&family, &paths, None)
            .unwrap(),
    )
}

fn loc(page: usize, paragraph: usize) -> Location {
    Location { page, paragraph }
}

fn resource(page: usize, paragraph: usize, composition: usize) -> Resource {
    Resource {
        location: loc(page, paragraph),
        composition,
    }
}

fn text(value: &str, style: Option<Resource>) -> OutputPart {
    OutputPart::Text {
        text: value.into(),
        style,
    }
}

fn output(location: Location, parts: Vec<OutputPart>) -> ResolvedParagraph {
    ResolvedParagraph { location, parts }
}

fn formula_identity(document: &ir::Document, page: usize, paragraph: usize, index: usize) -> u64 {
    let view = document.read();
    view.document()
        .page_at(&view, page)
        .unwrap()
        .pdf_paragraph_at(&view, paragraph)
        .unwrap()
        .pdf_paragraph_composition_at(&view, index)
        .unwrap()
        .pdf_formula(&view)
        .unwrap()
        .unwrap()
        .identity()
}

fn graphic_identity(
    document: &ir::Document,
    page: usize,
    paragraph: usize,
    index: usize,
    rich: bool,
) -> u64 {
    let view = document.read();
    let composition = view
        .document()
        .page_at(&view, page)
        .unwrap()
        .pdf_paragraph_at(&view, paragraph)
        .unwrap()
        .pdf_paragraph_composition_at(&view, index)
        .unwrap();
    let style = if rich {
        composition
            .pdf_same_style_characters(&view)
            .unwrap()
            .unwrap()
            .pdf_style(&view)
            .unwrap()
            .unwrap()
    } else {
        composition
            .pdf_same_style_unicode_characters(&view)
            .unwrap()
            .unwrap()
            .pdf_style(&view)
            .unwrap()
            .unwrap()
    };
    style.graphic_state(&view).unwrap().unwrap().identity()
}

fn assert_rejected_unchanged(
    document: &mut ir::Document,
    outputs: &[ResolvedParagraph],
    mapper: &FontMapperHandle,
) {
    let before = document.artifact().unwrap();
    assert!(apply(document, outputs, mapper).is_err());
    assert_eq!(before, document.artifact().unwrap());
}

#[test]
fn swaps_formulas_and_styles_after_capturing_all_sources() {
    let mut document = fixture();
    let mapper = mapper();
    let first_formula = formula_identity(&document, 0, 0, 1);
    let second_formula = formula_identity(&document, 0, 1, 1);
    let first_graphic = graphic_identity(&document, 0, 0, 0, true);
    let second_graphic = graphic_identity(&document, 0, 1, 0, true);
    let outputs = vec![
        output(
            loc(0, 0),
            vec![
                text("A", Some(resource(0, 1, 0))),
                OutputPart::Formula(resource(0, 1, 1)),
            ],
        ),
        output(
            loc(0, 1),
            vec![
                OutputPart::Formula(resource(0, 0, 1)),
                text("B", Some(resource(0, 0, 0))),
                text("C", None),
            ],
        ),
    ];
    apply(&mut document, &outputs, &mapper).unwrap();
    assert_eq!(formula_identity(&document, 0, 0, 1), second_formula);
    assert_eq!(formula_identity(&document, 0, 1, 0), first_formula);
    assert_eq!(graphic_identity(&document, 0, 0, 0, false), second_graphic);
    assert_eq!(graphic_identity(&document, 0, 1, 1, false), first_graphic);
    let view = document.read();
    let page = view.document().page_at(&view, 0).unwrap();
    let first = page.pdf_paragraph_at(&view, 0).unwrap();
    let second = page.pdf_paragraph_at(&view, 1).unwrap();
    assert_eq!(first.unicode(&view).unwrap().as_deref(), Some("Ay"));
    assert_eq!(second.unicode(&view).unwrap().as_deref(), Some("xBC"));
    let styled = first
        .pdf_paragraph_composition_at(&view, 0)
        .unwrap()
        .pdf_same_style_unicode_characters(&view)
        .unwrap()
        .unwrap()
        .pdf_style(&view)
        .unwrap()
        .unwrap();
    assert_eq!(styled.font_id(&view).unwrap().as_deref(), Some("boldface"));
    assert_eq!(styled.font_size(&view).unwrap(), Some(15.0));
    let plain = second
        .pdf_paragraph_composition_at(&view, 2)
        .unwrap()
        .pdf_same_style_unicode_characters(&view)
        .unwrap()
        .unwrap()
        .pdf_style(&view)
        .unwrap()
        .unwrap();
    assert_eq!(
        plain.identity(),
        second.pdf_style(&view).unwrap().unwrap().identity()
    );
}

#[test]
fn resolves_cross_page_and_xobject_fonts_in_the_source_scope() {
    let mapper = mapper();
    for (source, expected_size) in [(resource(1, 0, 0), 19.0), (resource(0, 2, 0), 17.0)] {
        let mut document = fixture();
        let graphic = graphic_identity(
            &document,
            source.location.page,
            source.location.paragraph,
            source.composition,
            true,
        );
        let outputs = vec![output(
            loc(0, 0),
            vec![
                text("A", Some(source)),
                OutputPart::Formula(resource(0, 0, 1)),
            ],
        )];
        apply(&mut document, &outputs, &mapper).unwrap();
        assert_eq!(graphic_identity(&document, 0, 0, 0, false), graphic);
        let view = document.read();
        let style = view
            .document()
            .page_at(&view, 0)
            .unwrap()
            .pdf_paragraph_at(&view, 0)
            .unwrap()
            .pdf_paragraph_composition_at(&view, 0)
            .unwrap()
            .pdf_same_style_unicode_characters(&view)
            .unwrap()
            .unwrap()
            .pdf_style(&view)
            .unwrap()
            .unwrap();
        assert_eq!(style.font_id(&view).unwrap().as_deref(), Some("boldface"));
        assert_eq!(style.font_size(&view).unwrap(), Some(expected_size));
    }
}

#[test]
fn writes_mapped_style_into_an_xobject_target() {
    let mut document = fixture();
    let mapper = mapper();
    let formula = formula_identity(&document, 0, 2, 1);
    let outputs = vec![output(
        loc(0, 2),
        vec![
            text("A", Some(resource(0, 1, 0))),
            OutputPart::Formula(resource(0, 2, 1)),
        ],
    )];
    apply(&mut document, &outputs, &mapper).unwrap();
    assert_eq!(formula_identity(&document, 0, 2, 1), formula);
    let view = document.read();
    let style = view
        .document()
        .page_at(&view, 0)
        .unwrap()
        .pdf_paragraph_at(&view, 2)
        .unwrap()
        .pdf_paragraph_composition_at(&view, 0)
        .unwrap()
        .pdf_same_style_unicode_characters(&view)
        .unwrap()
        .unwrap()
        .pdf_style(&view)
        .unwrap()
        .unwrap();
    assert_eq!(style.font_id(&view).unwrap().as_deref(), Some("boldface"));
}

#[test]
fn rejects_page_scoped_graphics_when_moving_rich_text() {
    let mapper = mapper();
    for instruction in ["/GS1 gs", "/Pattern cs /P1 scn", "1 0 0 1 40 0 cm", "7 Tr"] {
        let mut document = fixture();
        {
            let mut view = document.view::<ir::TranslationView>();
            let source = view
                .document()
                .page_at(&view, 1)
                .unwrap()
                .pdf_paragraph_at(&view, 0)
                .unwrap()
                .pdf_paragraph_composition_at(&view, 0)
                .unwrap()
                .pdf_same_style_characters(&view)
                .unwrap()
                .unwrap()
                .pdf_style(&view)
                .unwrap()
                .unwrap()
                .graphic_state(&view)
                .unwrap()
                .unwrap();
            source
                .set_passthrough_per_char_instruction(
                    &mut view,
                    Some(ir::Instructions::Materialized(instruction.into())),
                )
                .unwrap();
        }
        assert_rejected_unchanged(
            &mut document,
            &[output(
                loc(0, 0),
                vec![
                    text("A", Some(resource(1, 0, 0))),
                    OutputPart::Formula(resource(0, 0, 1)),
                ],
            )],
            &mapper,
        );
    }
}

#[test]
fn rejects_formula_loss_duplication_and_unmodified_sources_atomically() {
    let mapper = mapper();
    for parts in [
        vec![text("A", None)],
        vec![
            OutputPart::Formula(resource(0, 0, 1)),
            OutputPart::Formula(resource(0, 0, 1)),
        ],
        vec![OutputPart::Formula(resource(0, 1, 1))],
    ] {
        let mut document = fixture();
        assert_rejected_unchanged(&mut document, &[output(loc(0, 0), parts)], &mapper);
    }
}

#[test]
fn rejects_cross_page_and_cross_scope_formula_moves_atomically() {
    let mapper = mapper();
    let mut document = fixture();
    let cross_page = vec![
        output(loc(0, 0), vec![OutputPart::Formula(resource(1, 0, 1))]),
        output(loc(1, 0), vec![OutputPart::Formula(resource(0, 0, 1))]),
    ];
    assert_rejected_unchanged(&mut document, &cross_page, &mapper);
    let cross_scope = vec![
        output(loc(0, 0), vec![OutputPart::Formula(resource(0, 2, 1))]),
        output(loc(0, 2), vec![OutputPart::Formula(resource(0, 0, 1))]),
    ];
    assert_rejected_unchanged(&mut document, &cross_scope, &mapper);
}

#[test]
fn swaps_formulas_between_paragraphs_in_the_same_xobject() {
    let mut document = fixture();
    let mapper = mapper();
    let first = formula_identity(&document, 0, 2, 1);
    let second = formula_identity(&document, 0, 4, 1);
    let outputs = vec![
        output(loc(0, 2), vec![OutputPart::Formula(resource(0, 4, 1))]),
        output(loc(0, 4), vec![OutputPart::Formula(resource(0, 2, 1))]),
    ];
    apply(&mut document, &outputs, &mapper).unwrap();
    assert_eq!(formula_identity(&document, 0, 2, 0), second);
    assert_eq!(formula_identity(&document, 0, 4, 0), first);
}

#[test]
fn preserves_the_number_of_existing_formula_aliases() {
    let mut document = fixture();
    let mapper = mapper();
    {
        let mut view = document.view::<ir::TypesettingView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let first = page
            .pdf_paragraph_at(&view, 0)
            .unwrap()
            .pdf_paragraph_composition_at(&view, 1)
            .unwrap()
            .pdf_formula(&view)
            .unwrap()
            .unwrap();
        let second = page
            .pdf_paragraph_at(&view, 1)
            .unwrap()
            .pdf_paragraph_composition_at(&view, 1)
            .unwrap();
        let shared = first.share(&mut view).unwrap();
        second.set_pdf_formula(&mut view, Some(shared)).unwrap();
    }
    let identity = formula_identity(&document, 0, 0, 1);
    assert_eq!(formula_identity(&document, 0, 1, 1), identity);
    let outputs = vec![
        output(
            loc(0, 0),
            vec![
                OutputPart::Formula(resource(0, 0, 1)),
                OutputPart::Formula(resource(0, 1, 1)),
            ],
        ),
        output(loc(0, 1), vec![]),
    ];
    apply(&mut document, &outputs, &mapper).unwrap();
    assert_eq!(formula_identity(&document, 0, 0, 0), identity);
    assert_eq!(formula_identity(&document, 0, 0, 1), identity);
}

#[test]
fn rejects_invalid_resources_duplicate_targets_and_missing_glyphs_atomically() {
    let mapper = mapper();
    let mut document = fixture();
    let valid = output(loc(0, 0), vec![OutputPart::Formula(resource(0, 0, 1))]);
    assert_rejected_unchanged(&mut document, &[valid.clone(), valid], &mapper);
    assert_rejected_unchanged(
        &mut document,
        &[output(
            loc(0, 0),
            vec![
                text("A", Some(resource(0, 0, 99))),
                OutputPart::Formula(resource(0, 0, 1)),
            ],
        )],
        &mapper,
    );
    assert_rejected_unchanged(
        &mut document,
        &[output(
            loc(0, 0),
            vec![
                text("\u{10ffff}", Some(resource(0, 1, 0))),
                OutputPart::Formula(resource(0, 0, 1)),
            ],
        )],
        &mapper,
    );
    assert_rejected_unchanged(
        &mut document,
        &[
            output(
                loc(0, 0),
                vec![text("A", None), OutputPart::Formula(resource(0, 0, 1))],
            ),
            output(
                loc(0, 1),
                vec![
                    text("B", Some(resource(0, 1, 99))),
                    OutputPart::Formula(resource(0, 1, 1)),
                ],
            ),
        ],
        &mapper,
    );
}

#[test]
fn permits_empty_output_and_preserves_unaffected_formula() {
    let mut document = fixture();
    let mapper = mapper();
    let identity = formula_identity(&document, 0, 0, 1);
    apply(&mut document, &[output(loc(0, 3), vec![])], &mapper).unwrap();
    let view = document.read();
    let target = view
        .document()
        .page_at(&view, 0)
        .unwrap()
        .pdf_paragraph_at(&view, 3)
        .unwrap();
    assert_eq!(target.unicode(&view).unwrap().as_deref(), Some(""));
    assert_eq!(target.pdf_paragraph_composition_len(&view).unwrap(), 0);
    drop(view);
    assert_eq!(formula_identity(&document, 0, 0, 1), identity);
}
