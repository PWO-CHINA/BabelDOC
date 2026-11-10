use babeldoc_ir_core::{AccessError, DataBox, DataCropbox, DataDocument, DataGraphicState, DataPage, DataPdfParagraph, DataPdfStyle, Document, GeometryView, History, HistoryArg, HistoryNode, InstructionView, Instructions, Node, NodeData, ReadOnlyView, Table, TranslationView};

fn document() -> Document {
    let mut table = Table::default();
    let paragraph = table.attach(Node::new(NodeData::PdfParagraph(DataPdfParagraph {
        unicode: Some("original".into()),
        ..Default::default()
    })));
    let page = table.attach(Node::new(NodeData::Page(DataPage {
        pdf_paragraph: vec![paragraph.share(), paragraph],
        ..Default::default()
    })));
    Document::from_parts(Node::new(NodeData::Document(DataDocument {
        page: vec![page],
        ..Default::default()
    })), table, History::default()).unwrap()
}

#[test]
fn fork_preserves_aliases_and_isolates_mutations_and_handles() {
    let mut original = document();
    let mut fork = original.fork();
    assert_eq!(original.artifact().unwrap(), fork.artifact().unwrap());
    let old = {
        let view = original.view::<ReadOnlyView>();
        view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
    };
    let fork_handle = {
        let mut view = fork.view::<TranslationView>();
        assert_eq!(old.unicode(&view).unwrap_err(), AccessError::StaleHandle);
        let page = view.document().page_at(&view, 0).unwrap();
        let first = page.pdf_paragraph_at(&view, 0).unwrap();
        let second = page.pdf_paragraph_at(&view, 1).unwrap();
        first.set_unicode(&mut view, Some("fork".into())).unwrap();
        assert_eq!(second.unicode(&view).unwrap().as_deref(), Some("fork"));
        first
    };
    let foreign_value = {
        let mut view = fork.view::<TranslationView>();
        fork_handle.share(&mut view).unwrap()
    };
    {
        let mut view = original.view::<TranslationView>();
        assert_eq!(fork_handle.unicode(&view).unwrap_err(), AccessError::StaleHandle);
        let page = view.document().page_at(&view, 0).unwrap();
        assert_eq!(page.push_pdf_paragraph(&mut view, foreign_value).unwrap_err(), AccessError::StaleHandle);
        assert_eq!(old.unicode(&view).unwrap().as_deref(), Some("original"));
        old.set_unicode(&mut view, Some("changed original".into())).unwrap();
    }
    fork.compact();
    original.compact();
    let view = fork.read();
    let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 1).unwrap();
    assert_eq!(paragraph.unicode(&view).unwrap().as_deref(), Some("fork"));
    drop(view);
    let view = original.read();
    let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 1).unwrap();
    assert_eq!(paragraph.unicode(&view).unwrap().as_deref(), Some("changed original"));
}

#[test]
fn fork_history_is_independent() {
    let mut original = document();
    let first = original.history.push(HistoryNode {
        parent: None, op: "first".into(), arg: HistoryArg::Text("one".into()),
        replace_key: None, version: 1, length: 1,
    });
    let mut fork = original.fork();
    assert_eq!(fork.history.get(first).unwrap().op, "first");
    let next = fork.history.push(HistoryNode {
        parent: Some(first), op: "second".into(), arg: HistoryArg::Text("two".into()),
        replace_key: None, version: 2, length: 2,
    });
    assert_eq!(fork.history.parent_key(fork.history.get(next).unwrap()), Some(first));
    assert!(original.history.get(next).is_err());
    fork.compact();
    assert_eq!(original.history.get(first).unwrap().op, "first");
}

fn active_instructions(document: &Document) -> InstructionView {
    let view = document.read();
    let state = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
        .pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap();
    let Some(Instructions::View(instructions)) = state.passthrough_per_char_instruction(&view).unwrap() else {
        panic!("expected a history-backed instruction view");
    };
    instructions.clone()
}

fn rendered_instructions(document: &Document) -> String {
    let instructions = active_instructions(document);
    document.read().render_instructions(&Some(Instructions::View(instructions))).unwrap().unwrap()
}

fn append_instruction(document: &mut Document, op: &str, arg: &str) {
    let mut instructions = active_instructions(document);
    instructions.tail = Some(document.history.push(HistoryNode {
        parent: instructions.tail, op: op.into(), arg: HistoryArg::Text(arg.into()),
        replace_key: None, version: 3, length: 3,
    }));
    let mut view = document.view::<TranslationView>();
    let state = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap()
        .pdf_style(&view).unwrap().unwrap().graphic_state(&view).unwrap().unwrap();
    state.set_passthrough_per_char_instruction(&mut view, Some(Instructions::View(instructions))).unwrap();
}

#[test]
fn fork_preserves_and_isolates_reachable_history_through_compaction() {
    let mut history = History::default();
    let first = history.push(HistoryNode {
        parent: None, op: "first".into(), arg: HistoryArg::Text("one".into()),
        replace_key: None, version: 1, length: 1,
    });
    let second = history.push(HistoryNode {
        parent: Some(first), op: "second".into(), arg: HistoryArg::Text("two".into()),
        replace_key: None, version: 2, length: 2,
    });
    let mut table = Table::default();
    let state = table.attach(Node::new(NodeData::GraphicState(DataGraphicState {
        passthrough_per_char_instruction: Some(Instructions::View(InstructionView {
            tail: Some(second), latest_versions: vec![], include_clipping: true, suffix_parts: vec![],
        })),
    })));
    let style = table.attach(Node::new(NodeData::PdfStyle(DataPdfStyle { graphic_state: Some(state), ..Default::default() })));
    let paragraph = table.attach(Node::new(NodeData::PdfParagraph(DataPdfParagraph { pdf_style: Some(style), ..Default::default() })));
    let page = table.attach(Node::new(NodeData::Page(DataPage { pdf_paragraph: vec![paragraph], ..Default::default() })));
    let mut original = Document::from_parts(Node::new(NodeData::Document(DataDocument { page: vec![page], ..Default::default() })), table, history).unwrap();
    assert_eq!(rendered_instructions(&original), "one first two second");
    let mut fork = original.fork();
    assert_eq!(rendered_instructions(&fork), "one first two second");

    append_instruction(&mut fork, "fork", "three");
    assert_eq!(rendered_instructions(&fork), "one first two second three fork");
    assert_eq!(rendered_instructions(&original), "one first two second");
    append_instruction(&mut original, "original", "four");
    assert_eq!(rendered_instructions(&original), "one first two second four original");
    assert_eq!(rendered_instructions(&fork), "one first two second three fork");

    fork.compact();
    original.compact();
    assert_eq!(rendered_instructions(&fork), "one first two second three fork");
    assert_eq!(rendered_instructions(&original), "one first two second four original");
}

#[test]
fn immutable_subtree_references_the_correct_document_table() {
    let mut table = Table::default();
    let bounds = table.attach(Node::new(NodeData::Box(DataBox { x: Some(10.0), ..Default::default() })));
    let crop = table.attach(Node::new(NodeData::Cropbox(DataCropbox { r#box: Some(bounds) })));
    let page = table.attach(Node::new(NodeData::Page(DataPage { cropbox: Some(crop), ..Default::default() })));
    let original = Document::from_parts(Node::new(NodeData::Document(DataDocument { page: vec![page], ..Default::default() })), table, History::default()).unwrap();
    let mut fork = original.fork();
    {
        let mut view = fork.view::<GeometryView>();
        let bounds = view.document().page_at(&view, 0).unwrap().cropbox(&view).unwrap().unwrap().r#box(&view).unwrap().unwrap();
        bounds.set_x(&mut view, Some(20.0)).unwrap();
    }
    let view = original.read();
    let bounds = view.document().page_at(&view, 0).unwrap().cropbox(&view).unwrap().unwrap().r#box(&view).unwrap().unwrap();
    assert_eq!(bounds.x(&view).unwrap(), Some(10.0));
    drop(view);
    fork.compact();
    let view = fork.read();
    let bounds = view.document().page_at(&view, 0).unwrap().cropbox(&view).unwrap().unwrap().r#box(&view).unwrap().unwrap();
    assert_eq!(bounds.x(&view).unwrap(), Some(20.0));
}
