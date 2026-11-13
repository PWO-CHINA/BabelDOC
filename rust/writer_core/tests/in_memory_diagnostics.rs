include!("in_memory_writer_evidence.rs");

use babeldoc_writer_core::{FontBindingIssue, FontBindingInventoryLimit, InMemoryFontBindingOutcome, embed_font};

fn inventory_fixture(id: &str, formula: bool, pages: usize) -> ir::Document {
    let character = || node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: Some(36), char_unicode: Some("A".into()), render_order: Some(1),
        sub_render_order: Some(0), r#box: Some(bounds()),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
            font_id: Some(id.into()), font_size: Some(12.0), ..Default::default()
        }))), ..Default::default()
    }));
    let make_page = || {
        let mut page = ir::DataPage { page_number: Some(0), cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds()) }))), ..Default::default() };
        if formula {
            page.pdf_paragraph.push(node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph {
                pdf_paragraph_composition: vec![node(ir::NodeData::PdfParagraphComposition(ir::DataPdfParagraphComposition {
                    pdf_formula: Some(node(ir::NodeData::PdfFormula(ir::DataPdfFormula { pdf_character: vec![character()], ..Default::default() }))), ..Default::default()
                }))], ..Default::default()
            })));
        } else { page.pdf_character.push(character()); }
        node(ir::NodeData::Page(page))
    };
    ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: (0..pages).map(|_| make_page()).collect(), ..Default::default()
    })), Default::default(), Default::default()).unwrap()
}

#[test]
fn formula_exclusion_is_explicit_and_duplicate_page_indices_are_distinct() {
    let (pdf, map) = pdf();
    let mut formula = inventory_fixture("FORMULA", true, 1);
    let used = babeldoc_writer_core::used_font_ids(&formula.view::<ir::WriterView>()).unwrap();
    assert!(!used.contains("FORMULA"));
    let report = diagnose_in_memory_writer(&formula, &pdf, &map, &[], &BuildOptions::default(), || false);
    assert!(report.font_binding_inventory_complete && report.font_bindings.is_empty());
    assert!(report.unverified.iter().any(|item| item.contains("formula-only")));
    let duplicates = inventory_fixture("F1", false, 2);
    let report = diagnose_in_memory_writer(&duplicates, &pdf, &map, &[], &BuildOptions::default(), || false);
    assert_eq!(report.font_bindings.len(), 2);
    assert_eq!(report.font_bindings[0].page_number, report.font_bindings[1].page_number);
    assert_eq!(report.font_bindings[0].page_index, 0);
    assert_eq!(report.font_bindings[1].page_index, 1);
}

#[test]
fn slash_id_is_unsupported_even_when_stripped_resource_matches() {
    let face = FontFace::load("public", std::env::var_os("BABELDOC_TEST_FONT").unwrap()).unwrap();
    let metadata = FontMetadata { font_name: "PublicFont".into(), ascent: 0.9, descent: -0.2, encoding_length: 2 };
    let assets = [FontAsset { id: "/F1", face: &face, metadata: &metadata }];
    let ir = inventory_fixture("/F1", false, 1);
    let (mut pdf, map) = pdf();
    let font = embed_font(&mut pdf, &assets[0]).unwrap();
    let page = babeldoc_writer_core::page_object_with_map(&pdf, &map, 0).unwrap();
    pdf.get_dictionary_mut(page).unwrap().get_mut(b"Resources").unwrap().as_dict_mut().unwrap().get_mut(b"Font").unwrap().as_dict_mut().unwrap().set("F1", Object::Reference(font));
    let report = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || false);
    assert!(matches!(&report.font_bindings[0].outcome, InMemoryFontBindingOutcome::UnsupportedResourceName));
}

#[test]
fn comparison_budget_stops_inventory() {
    let face = FontFace::load("public", std::env::var_os("BABELDOC_TEST_FONT").unwrap()).unwrap();
    let metadata = FontMetadata { font_name: "PublicFont".into(), ascent: 0.9, descent: -0.2, encoding_length: 2 };
    let assets = [FontAsset { id: "F1", face: &face, metadata: &metadata }];
    let page_count = (8 * 1024 * 1024 / face.pdf_embedding().unwrap().data.len()) + 2;
    assert!(page_count < 10_000);
    let ir = inventory_fixture("F1", false, page_count);
    let (pdf, map) = pdf();
    let before_ir = ir.artifact().unwrap();
    let before_pdf = format!("{:?}", pdf.objects);
    let report = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || false);
    assert!(!report.font_binding_inventory_complete);
    assert_eq!(report.font_binding_inventory_limit, Some(FontBindingInventoryLimit::ComparisonBytes));
    assert!(matches!(report.font_bindings.last().unwrap().outcome, InMemoryFontBindingOutcome::ComparisonBudgetExceeded));
    assert!(report.font_bindings.len() < page_count);
    let mut registration_pdf = pdf.clone();
    let mut registration_ir = ir.fork();
    let used = babeldoc_writer_core::used_font_ids(&registration_ir.view::<ir::WriterView>()).unwrap();
    let mut registration_checks = 0;
    babeldoc_writer_core::add_fonts_with_used_ids_from_page_objects(
        &mut registration_pdf, &mut registration_ir.view::<ir::WriterView>(), &assets, &used, &map,
        |_| { registration_checks += 1; Ok(()) },
    ).unwrap();
    let cancel_at = 1 + registration_checks + 2 * report.font_bindings.len() + 1;
    let mut checks = 0;
    let cancelled = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || {
        checks += 1;
        checks == cancel_at
    });
    assert!(cancelled.cancelled && !cancelled.font_binding_inventory_complete);
    assert_eq!(cancelled.font_binding_inventory_limit, Some(FontBindingInventoryLimit::ComparisonBytes));
    assert!(matches!(cancelled.font_bindings.last().unwrap().outcome, InMemoryFontBindingOutcome::ComparisonBudgetExceeded));
    assert_eq!(before_ir, ir.artifact().unwrap());
    assert_eq!(before_pdf, format!("{:?}", pdf.objects));
}

#[test]
fn binding_observes_registration_and_missing_asset_without_mutation() {
    let face = FontFace::load("public", std::env::var_os("BABELDOC_TEST_FONT").unwrap()).unwrap();
    let metadata = FontMetadata { font_name: "PublicFont".into(), ascent: 0.9, descent: -0.2, encoding_length: 2 };
    let assets = [FontAsset { id: "F1", face: &face, metadata: &metadata }];
    let ir = document(Some(36), Some(2), false);
    let (pdf, map) = pdf();
    let before_ir = ir.artifact().unwrap();
    let before_pdf = format!("{:?}", pdf.objects);
    let mut checks = 0;
    let report = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || { checks += 1; false });
    assert!(report.font_binding_inventory_complete);
    assert_eq!(report.font_bindings.len(), 1);
    assert!(matches!(&report.font_bindings[0].outcome, InMemoryFontBindingOutcome::Observed(result) if result.outcome.is_ok()));
    let missing = diagnose_in_memory_writer(&ir, &pdf, &map, &[], &BuildOptions::default(), || false);
    assert!(matches!(missing.font_bindings[0].outcome, InMemoryFontBindingOutcome::NoAsset));
    assert!(missing.font_binding_inventory_complete);
    let mut cancellation_checks = 0;
    let cancelled = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || {
        cancellation_checks += 1;
        cancellation_checks >= checks - 1
    });
    assert!(cancelled.cancelled && !cancelled.font_binding_inventory_complete);
    assert!(!cancelled.font_bindings.is_empty());
    assert_eq!(before_ir, ir.artifact().unwrap());
    assert_eq!(before_pdf, format!("{:?}", pdf.objects));
}

#[test]
fn binding_reports_existing_same_name_different_bytes() {
    let face = FontFace::load("public", std::env::var_os("BABELDOC_TEST_FONT").unwrap()).unwrap();
    let metadata = FontMetadata { font_name: "PublicFont".into(), ascent: 0.9, descent: -0.2, encoding_length: 2 };
    let assets = [FontAsset { id: "F1", face: &face, metadata: &metadata }];
    let ir = document(Some(36), Some(2), false);
    let (mut pdf, map) = pdf();
    let font = embed_font(&mut pdf, &assets[0]).unwrap();
    let descendant = pdf.get_dictionary(font).unwrap().get(b"DescendantFonts").unwrap().as_array().unwrap()[0].as_reference().unwrap();
    let descriptor = pdf.get_dictionary(descendant).unwrap().get(b"FontDescriptor").unwrap().as_reference().unwrap();
    let file = pdf.get_dictionary(descriptor).unwrap().get(b"FontFile2").unwrap().as_reference().unwrap();
    pdf.get_object_mut(file).unwrap().as_stream_mut().unwrap().content[0] ^= 1;
    let page = babeldoc_writer_core::page_object_with_map(&pdf, &map, 0).unwrap();
    pdf.get_dictionary_mut(page).unwrap().get_mut(b"Resources").unwrap().as_dict_mut().unwrap().get_mut(b"Font").unwrap().as_dict_mut().unwrap().set("F1", Object::Reference(font));
    let before_ir = ir.artifact().unwrap();
    let before_pdf = format!("{:?}", pdf.objects);
    let report = diagnose_in_memory_writer(&ir, &pdf, &map, &assets, &BuildOptions::default(), || false);
    assert!(report.font_binding_inventory_complete);
    assert!(matches!(&report.font_bindings[0].outcome, InMemoryFontBindingOutcome::Observed(result) if result.outcome == Err(FontBindingIssue::FontBytesDiffer)), "{:?}", report.font_bindings);
    assert_eq!(before_ir, ir.artifact().unwrap());
    assert_eq!(before_pdf, format!("{:?}", pdf.objects));
}
