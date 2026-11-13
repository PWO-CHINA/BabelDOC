use babeldoc_ir_core as ir;
use babeldoc_writer_core::{
    BuildOptions, FontAsset, FontFace, FontMetadata, PageObjectMap, PdfDocument,
    WriterDiagnosticKind, diagnose_in_memory_writer,
};
use lopdf::{Dictionary, Object, Stream, dictionary};

fn node(data: ir::NodeData) -> ir::Edge {
    ir::Edge::Inline(Box::new(ir::Node::new(data)))
}
fn bounds() -> ir::Edge {
    node(ir::NodeData::Box(ir::DataBox {
        x: Some(0.0),
        y: Some(0.0),
        x2: Some(10.0),
        y2: Some(10.0),
    }))
}

fn document(id: Option<i64>, encoding: Option<i64>, form: bool) -> ir::Document {
    document_with_form_scopes(id, encoding, if form { &[7] } else { &[] })
}

fn document_with_form_scopes(id: Option<i64>, encoding: Option<i64>, scopes: &[i64]) -> ir::Document {
    let character = node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: id,
        char_unicode: Some("A".into()),
        render_order: Some(1),
        sub_render_order: Some(0),
        r#box: Some(bounds()),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
            font_id: Some("F1".into()),
            font_size: Some(12.0),
            ..Default::default()
        }))),
        ..Default::default()
    }));
    let page = ir::DataPage {
        page_number: Some(0),
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox {
            r#box: Some(bounds()),
        }))),
        pdf_character: vec![character],
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some("F1".into()),
            encoding_length: encoding,
            ..Default::default()
        }))],
        pdf_xobject: scopes.iter().map(|scope| {
            node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
                xobj_id: Some(*scope),
                xref_id: Some(99999),
                base_operations: Some(node(ir::NodeData::BaseOperations(ir::DataBaseOperations {
                    value: "D77#BAOHaX00".into(),
                }))),
                ..Default::default()
            }))
        }).collect(),
        ..Default::default()
    };
    ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(page))],
            ..Default::default()
        })),
        Default::default(),
        Default::default(),
    )
    .unwrap()
}

#[test]
fn repeated_form_xref_is_reported_even_when_scope_differs() {
    let ir = document_with_form_scopes(Some(36), Some(2), &[7, 8]);
    let (pdf, map) = pdf();
    let report = diagnose_in_memory_writer(&ir, &pdf, &map, &[], &BuildOptions::default(), || false);
    assert!(report.errors.iter().any(|item| item.stage == "shared_form_xref"
        && item.page_number == Some(0) && item.scope == Some(8) && item.xref_id == Some(99999)), "{:?}", report.errors);
    assert!(!report.unverified.is_empty());
}

fn pdf() -> (PdfDocument, PageObjectMap) {
    let mut pdf = PdfDocument::with_version("1.7");
    let pages = pdf.new_object_id();
    let content = pdf.add_object(Stream::new(Dictionary::new(), Vec::new()));
    let page = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => Object::Reference(pages), "MediaBox" => vec![0.into(),0.into(),100.into(),100.into()], "Resources" => dictionary! { "Font" => Dictionary::new() }, "Contents" => Object::Reference(content) });
    pdf.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }));
    let catalog =
        pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages) });
    pdf.trailer.set("Root", Object::Reference(catalog));
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    (
        PdfDocument::load_mem(&bytes).unwrap(),
        PageObjectMap::new(vec![page]),
    )
}

fn font_count(doc: &mut ir::Document) -> usize {
    let view = doc.view::<ir::ReadOnlyView>();
    view.document()
        .page_at(&view, 0)
        .unwrap()
        .pdf_font_len(&view)
        .unwrap()
}

#[test]
fn real_font_registration_on_copies_and_cancellation() {
    let font_path = std::env::var_os("BABELDOC_TEST_FONT")
        .expect("set BABELDOC_TEST_FONT to a production-profiled public font asset");
    let face = FontFace::load("test", font_path).unwrap();
    let metadata = FontMetadata {
        font_name: "Arial".into(),
        ascent: 0.9,
        descent: -0.2,
        encoding_length: 2,
    };
    let assets = [FontAsset {
        id: "F1",
        face: &face,
        metadata: &metadata,
    }];
    let mut ir = document(Some(36), Some(2), false);
    let (pdf, map) = pdf();
    let before = format!("{:?}", pdf.objects);
    let original_ir = ir.artifact().unwrap();
    let fonts = font_count(&mut ir);
    let mut proof_pdf = pdf.clone();
    let mut proof_ir = ir.fork();
    let used = babeldoc_writer_core::used_font_ids(&proof_ir.view::<ir::WriterView>()).unwrap();
    let installed = babeldoc_writer_core::add_fonts_with_used_ids_from_page_objects(
        &mut proof_pdf,
        &mut proof_ir.view::<ir::WriterView>(),
        &assets,
        &used,
        &map,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(installed.installed.len(), 1);
    assert!(proof_pdf.objects.values().any(|object| {
        object
            .as_dict()
            .ok()
            .is_some_and(|dict| dict.has(b"FontFile2"))
    }));
    let report =
        diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || false);
    assert!(
        !report.cancelled && report.errors.is_empty(),
        "{:?}",
        report.errors
    );
    assert_eq!(report.pages_applied, 1);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(fonts, font_count(&mut ir));
    assert_eq!(original_ir, ir.artifact().unwrap());
    assert_eq!(before, format!("{:?}", pdf.objects));
    assert!(!report.unverified.is_empty());
    let mut checks = 0;
    let cancelled =
        diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || {
            checks += 1;
            checks >= 3
        });
    assert!(cancelled.cancelled);
    assert_eq!(fonts, font_count(&mut ir));
    assert_eq!(original_ir, ir.artifact().unwrap());
    assert_eq!(before, format!("{:?}", pdf.objects));
}

#[test]
fn missing_glyph_data_and_form_failure_are_reported() {
    let ir = document(Some(36), None, true);
    let (pdf, map) = pdf();
    let report =
        diagnose_in_memory_writer(&ir, &pdf, &map, &[], &BuildOptions::default(), || false);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.kind == WriterDiagnosticKind::FontUnavailable)
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.kind == WriterDiagnosticKind::MissingEncodingLength)
    );
    assert!(
        report.errors.iter().any(|item| item.stage == "apply_form"
            && item.page_number == Some(0)
            && item.scope == Some(7)
            && item.xref_id == Some(99999)),
        "{:?}",
        report.errors
    );
    let missing_id = document(None, None, false);
    let report = diagnose_in_memory_writer(
        &missing_id,
        &pdf,
        &map,
        &[],
        &BuildOptions::default(),
        || false,
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.kind == WriterDiagnosticKind::MissingCharacterId)
    );
}
