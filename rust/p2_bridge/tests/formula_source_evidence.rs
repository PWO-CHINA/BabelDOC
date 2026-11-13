use _rust_il::formula_source_evidence::{capture_formula_source_evidence as capture, capture_formula_source_evidence_many};
use babeldoc_ir_core as ir;

#[path = "../src/formula_source_evidence.rs"]
mod snapshot_validation;

fn fixture() -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        ..Default::default()
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    let mut view = document.view::<ir::ParagraphFinderView>();
    let page = view.document().page_at(&view, 0).unwrap();
    page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
        xobj_id: Some(1),
        pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
            pdf_formula: Some(ir::PdfFormulaInit {
                x_offset: Some(1.0),
                pdf_character: vec![ir::PdfCharacterInit {
                    char_unicode: Some("x".into()), pdf_character_id: Some(10),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("font".into()), font_size: Some(10.0),
                        graphic_state: Some(ir::GraphicStateInit { passthrough_per_char_instruction: Some(ir::Instructions::Materialized("state".into())) }.into_value()),
                    }.into_value()), ..Default::default()
                }.into_value()],
                pdf_curve: vec![ir::PdfCurveInit { xobj_id: Some(1), pdf_path: vec![ir::PdfPathInit { x: Some(1.0), y: Some(2.0), op: Some("m".into()), ..Default::default() }.into_value()], ..Default::default() }.into_value()],
                pdf_form: vec![ir::PdfFormInit { xobj_id: Some(1), pdf_matrix: Some(ir::PdfMatrixInit { a: Some(1.0), ..Default::default() }.into_value()), ..Default::default() }.into_value()],
                ..Default::default()
            }.into_value()), ..Default::default()
        }.into_value()], ..Default::default()
    }.into_value()]).unwrap();
    drop(view);
    document
}

#[test]
fn fork_and_same_value_different_node() {
    let document = fixture();
    let expected = capture(&document, 0, 0).unwrap();
    let batched = capture_formula_source_evidence_many(&document, &[(0, 0), (0, 0)]).unwrap();
    assert!(batched.iter().all(|item| item == &expected));
    assert!(capture_formula_source_evidence_many(&document, &[]).unwrap().is_empty());
    assert!(expected == capture(&document.fork(), 0, 0).unwrap());
    let mut fork = document.fork();
    {
        let mut view = fork.view::<ir::TypesettingView>();
        let composition = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap();
        let formula = composition.pdf_formula(&view).unwrap().unwrap();
        let replacement = formula.copy_shallow(&mut view).unwrap().into_value();
        composition.set_pdf_formula(&mut view, Some(replacement)).unwrap();
    }
    assert!(expected != capture(&fork, 0, 0).unwrap());
}

#[test]
fn detects_mutations_of_existing_nodes() {
    for field in 0..10 {
        let source = fixture();
        let expected = capture(&source, 0, 0).unwrap();
        let mut fork = source.fork();
        {
            let mut view = fork.view::<ir::TypesettingView>();
            let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
            let formula = paragraph.pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap();
            match field {
                0 => formula.set_x_offset(&mut view, Some(2.0)).unwrap(),
                1 => formula.pdf_character_at(&view, 0).unwrap().set_char_unicode(&mut view, Some("y".into())).unwrap(),
                2 => formula.pdf_character_at(&view, 0).unwrap().set_pdf_character_id(&mut view, Some(11)).unwrap(),
                3 => formula.pdf_character_at(&view, 0).unwrap().pdf_style(&view).unwrap().unwrap().set_font_size(&mut view, Some(11.0)).unwrap(),
                4 => formula.pdf_character_at(&view, 0).unwrap().pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap().set_passthrough_per_char_instruction(&mut view, Some(ir::Instructions::Materialized("other".into()))).unwrap(),
                5 => formula.pdf_curve_at(&view, 0).unwrap().set_xobj_id(&mut view, Some(2)).unwrap(),
                6 => formula.pdf_form_at(&view, 0).unwrap().set_xobj_id(&mut view, Some(2)).unwrap(),
                7 => formula.pdf_curve_at(&view, 0).unwrap().set_pdf_path(&mut view, vec![ir::PdfPathInit { x: Some(3.0), y: Some(2.0), op: Some("m".into()), ..Default::default() }.into_value()]).unwrap(),
                8 => formula.pdf_form_at(&view, 0).unwrap().set_pdf_matrix(&mut view, Some(ir::PdfMatrixInit { a: Some(2.0), ..Default::default() }.into_value())).unwrap(),
                _ => paragraph.set_xobj_id(&mut view, Some(2)).unwrap(),
            }
        }
        assert!(expected != capture(&fork, 0, 0).unwrap(), "mutation {field}");
        assert!(expected == capture(&source, 0, 0).unwrap());
    }
}

#[test]
fn detects_changes_in_history_backed_graphic_state() {
    let mut document = fixture();
    let first = document.history.push(ir::HistoryNode { parent: None, op: "first".into(), arg: ir::HistoryArg::Text("one".into()), replace_key: None, version: 1, length: 1 });
    let instructions = ir::InstructionView { tail: Some(first), latest_versions: vec![], include_clipping: true, suffix_parts: vec![] };
    {
        let mut view = document.view::<ir::TypesettingView>();
        let state = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap().pdf_character_at(&view, 0).unwrap().pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap();
        state.set_passthrough_per_char_instruction(&mut view, Some(ir::Instructions::View(instructions.clone()))).unwrap();
    }
    let expected = capture(&document, 0, 0).unwrap();
    let mut fork = document.fork();
    assert!(expected == capture(&fork, 0, 0).unwrap());
    let second = fork.history.push(ir::HistoryNode { parent: Some(first), op: "second".into(), arg: ir::HistoryArg::Text("two".into()), replace_key: None, version: 2, length: 2 });
    {
        let mut view = fork.view::<ir::TypesettingView>();
        let state = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap().pdf_character_at(&view, 0).unwrap().pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap();
        state.set_passthrough_per_char_instruction(&mut view, Some(ir::Instructions::View(ir::InstructionView { tail: Some(second), ..instructions }))).unwrap();
    }
    assert!(expected != capture(&fork, 0, 0).unwrap());
}

#[test]
fn rejects_nonfinite_and_ambiguous_floats_on_existing_formula() {
    for value in [f64::from_bits(0x7ff8_0000_0000_0001), f64::from_bits(0x7ff8_0000_0000_0002), f64::INFINITY, f64::NEG_INFINITY, -0.0] {
        let source = fixture();
        let expected = capture(&source, 0, 0).unwrap();
        let mut fork = source.fork();
        {
            let mut view = fork.view::<ir::TypesettingView>();
            let formula = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap();
            formula.set_x_offset(&mut view, Some(value)).unwrap();
        }
        assert!(capture(&fork, 0, 0).is_err());
        assert!(capture_formula_source_evidence_many(&fork, &[(0, 0)]).is_err());
        assert!(expected == capture(&source, 0, 0).unwrap());
    }
}

#[test]
fn rejects_nonfinite_float_inside_history_argument() {
    let mut document = fixture();
    let node = document.history.push(ir::HistoryNode {
        parent: None, op: "cm".into(),
        arg: ir::HistoryArg::CtmAware { value: "matrix".into(), ctm: ir::PythonValue::Float(f64::INFINITY) },
        replace_key: None, version: 1, length: 1,
    });
    {
        let mut view = document.view::<ir::TypesettingView>();
        let state = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap().pdf_character_at(&view, 0).unwrap().pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap();
        state.set_passthrough_per_char_instruction(&mut view, Some(ir::Instructions::View(ir::InstructionView { tail: Some(node), latest_versions: vec![], include_clipping: true, suffix_parts: vec![] }))).unwrap();
    }
    assert!(capture(&document, 0, 0).is_err());
    assert!(capture(&document.fork(), 0, 0).is_err());
}

#[test]
fn rejects_nonfinite_original_path_arguments_on_existing_curve() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut document = fixture();
        {
            let mut view = document.view::<ir::TypesettingView>();
            let curve = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap().pdf_paragraph_composition_at(&view, 0).unwrap().pdf_formula(&view).unwrap().unwrap().pdf_curve_at(&view, 0).unwrap();
            curve.set_pdf_original_path_primitive(&mut view, Some(ir::PdfOriginalPathPrimitiveInit { op: Some("m".into()), args: vec![value] }.into_value())).unwrap();
        }
        let artifact = document.artifact().unwrap();
        assert!(artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]["pdf_formula"]["pdf_curve"][0]["pdf_original_path_primitive"]["args"][0].is_null());
        assert!(capture(&document, 0, 0).is_err());
        assert!(capture(&document.fork(), 0, 0).is_err());
    }
}
