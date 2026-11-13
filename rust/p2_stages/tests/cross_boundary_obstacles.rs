use babeldoc_ir_core as ir;
use babeldoc_p2_stages::layout_audit::{audit, inspect_page_obstacles, AuditInput, Bounds, ExpectedPart, Glyph, ObstacleKind, ObstacleUncertainty, Rejection, TextPart};

fn node(data: ir::NodeData) -> ir::Edge { ir::Edge::Inline(Box::new(ir::Node::new(data))) }
fn bounds(left: f64, bottom: f64, right: f64, top: f64) -> ir::Edge {
    node(ir::NodeData::Box(ir::DataBox { x: Some(left), y: Some(bottom), x2: Some(right), y2: Some(top) }))
}
fn document(figure: Option<ir::Edge>) -> ir::Document {
    let page = ir::DataPage {
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds(0.0, 0.0, 100.0, 100.0)) }))),
        pdf_paragraph: vec![node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph { r#box: Some(bounds(10.0, 50.0, 90.0, 80.0)), ..Default::default() })),
            node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph { r#box: Some(bounds(10.0, 10.0, 90.0, 20.0)), ..Default::default() }))],
        pdf_figure: figure.into_iter().collect(), ..Default::default()
    };
    ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: vec![node(ir::NodeData::Page(page))], ..Default::default() })), Default::default(), Default::default()).unwrap()
}

#[test]
fn reports_original_target_and_figure_without_claiming_complete_evidence() {
    for figure_box in [Some(bounds(10.0, 25.0, 90.0, 40.0)), Some(bounds(10.0, 60.0, 90.0, 70.0)), None] {
        let figure = node(ir::NodeData::PdfFigure(ir::DataPdfFigure { r#box: figure_box }));
        let mut document = document(Some(figure));
        let before = document.artifact().unwrap();
        let report = inspect_page_obstacles(&mut document, 0, 0).unwrap();
        assert_eq!(report.target_box.unwrap().bottom, 50.0);
        assert_eq!(report.obstacles.len(), 2);
        assert_eq!(report.obstacles[1].kind, ObstacleKind::Figure);
        assert!(report.uncertainties.contains(&ObstacleUncertainty::GraphicStateAndClippingUnexamined));
        assert!(!report.neighbor_evidence().complete);
        let result = audit(&AuditInput { page_bounds: report.crop_box, expected_parts: Some(vec![ExpectedPart::Text("A".into())]),
            text_parts: Some(vec![TextPart::Glyph(Glyph { text: Some("A".into()), glyph_id: Some(1), font_size: Some(10.0), bounds: Some(Bounds { left: 20.0, bottom: 60.0, right: 25.0, top: 70.0 }) })]),
            neighbors: Some(report.neighbor_evidence()), minimum_font_size: 5.0 }).unwrap_err();
        assert!(result.contains(&Rejection::IncompleteNeighbors));
        if report.obstacles[1].bounds.is_some_and(|bounds| bounds.bottom == 60.0) {
            assert!(result.contains(&Rejection::NeighborOverlap { glyph: 0, neighbor: 1 }));
        }
        assert_eq!(before, document.artifact().unwrap());
        if report.obstacles[1].bounds.is_none() {
            assert!(report.uncertainties.contains(&ObstacleUncertainty::MissingOrInvalidObstacleBox { kind: ObstacleKind::Figure, index: 0 }));
        }
    }
}

#[test]
fn empty_collections_and_missing_target_never_prove_safety() {
    let mut document = document(None);
    let report = inspect_page_obstacles(&mut document, 0, 99).unwrap();
    assert!(report.uncertainties.contains(&ObstacleUncertainty::MissingTarget));
    assert!(report.uncertainties.contains(&ObstacleUncertainty::IrCollectionCompletenessUnproven));
    assert!(!report.neighbor_evidence().complete);
    assert!(!report.matches_fork_target(&report));
}

#[test]
fn original_evidence_follows_the_same_fork_target_and_scope() {
    let figure = node(ir::NodeData::PdfFigure(ir::DataPdfFigure { r#box: Some(bounds(10.0, 25.0, 90.0, 40.0)) }));
    let mut original = document(Some(figure));
    let source = inspect_page_obstacles(&mut original, 0, 0).unwrap();
    let mut fork = original.fork();
    {
        let mut view = fork.view::<ir::TranslationView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        paragraph.set_unicode(&mut view, Some("translated".into())).unwrap();
    }
    let candidate = inspect_page_obstacles(&mut fork, 0, 0).unwrap();
    assert!(source.matches_fork_target(&candidate));
    assert_eq!(source.obstacles[1].bounds.unwrap().bottom, 25.0);
    assert!(source.uncertainties.contains(&ObstacleUncertainty::GraphicStateAndClippingUnexamined));
    {
        let mut view = fork.view::<ir::ParagraphFinderView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        paragraph.set_xobj_id(&mut view, Some(7)).unwrap();
    }
    let changed_scope = inspect_page_obstacles(&mut fork, 0, 0).unwrap();
    assert!(!source.matches_fork_target(&changed_scope));
}

#[test]
fn original_evidence_rejects_a_replacement_at_the_same_location() {
    let mut original = document(None);
    let source = inspect_page_obstacles(&mut original, 0, 0).unwrap();
    let mut fork = original.fork();
    {
        let mut view = fork.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(ir::BoxInit { x: Some(10.0), y: Some(50.0), x2: Some(90.0), y2: Some(80.0) }.into_value()),
            ..Default::default()
        }.into_value()]).unwrap();
    }
    let replacement = inspect_page_obstacles(&mut fork, 0, 0).unwrap();
    assert_eq!(source.target_box, replacement.target_box);
    assert!(!source.matches_fork_target(&replacement));
}

#[test]
fn opaque_operations_and_formula_contents_do_not_prove_safety() {
    for operations in ["", "invalid-encoding"] {
        let formula = node(ir::NodeData::PdfFormula(ir::DataPdfFormula {
            pdf_form: vec![node(ir::NodeData::PdfForm(ir::DataPdfForm::default()))],
            ..Default::default()
        }));
        let composition = node(ir::NodeData::PdfParagraphComposition(ir::DataPdfParagraphComposition {
            pdf_formula: Some(formula), ..Default::default()
        }));
        let page = ir::DataPage {
            cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox { r#box: Some(bounds(0.0, 0.0, 100.0, 100.0)) }))),
            pdf_paragraph: vec![node(ir::NodeData::PdfParagraph(ir::DataPdfParagraph {
                r#box: Some(bounds(10.0, 50.0, 90.0, 80.0)),
                pdf_paragraph_composition: vec![composition], ..Default::default()
            }))],
            base_operations: Some(node(ir::NodeData::BaseOperations(ir::DataBaseOperations { value: operations.into() }))),
            pdf_xobject: vec![node(ir::NodeData::PdfXobject(ir::DataPdfXobject {
                xobj_id: Some(1), base_operations: Some(node(ir::NodeData::BaseOperations(ir::DataBaseOperations { value: operations.into() }))),
                ..Default::default()
            }))],
            ..Default::default()
        };
        let mut document = ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(page))], ..Default::default()
        })), Default::default(), Default::default()).unwrap();
        let report = inspect_page_obstacles(&mut document, 0, 0).unwrap();
        assert!(report.uncertainties.contains(&ObstacleUncertainty::PageAndXobjectOperationsUnexamined));
        assert!(report.uncertainties.contains(&ObstacleUncertainty::IrCollectionCompletenessUnproven));
        assert!(!report.neighbor_evidence().complete);
    }
}
