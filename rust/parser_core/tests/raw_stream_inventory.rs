use babeldoc_parser_core::cross_boundary_evidence::{inventory_page, Identity};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

fn pdf(contents: Vec<Object>, resources: Object, extra: Vec<(ObjectId, Object)>) -> (Document, ObjectId) {
    let mut doc = Document::with_version("1.7");
    let pages = doc.new_object_id();
    let page = doc.new_object_id();
    for (id, object) in extra { doc.objects.insert(id, object); }
    doc.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1, "Resources" => resources }.into());
    doc.objects.insert(page, dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()], "Contents" => contents }.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    (Document::load_mem(&bytes).unwrap(), page)
}

fn stream(data: &[u8]) -> Object { Stream::new(dictionary! {}, data.to_vec()).into() }
fn form(data: &[u8], resources: Option<Object>) -> Object {
    let mut dict = dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()] };
    if let Some(resources) = resources { dict.set("Resources", resources); }
    Stream::new(dict, data.to_vec()).into()
}

#[test]
fn records_multistream_and_repeated_nested_forms_with_resource_sources() {
    let first = (10, 0); let second = (11, 0); let outer = (12, 0); let inner = (13, 0);
    let nested = dictionary! { "XObject" => dictionary! { "Inner" => inner } };
    let resources = dictionary! { "XObject" => dictionary! { "Outer" => outer } };
    let (doc, page) = pdf(vec![first.into(), second.into()], resources.into(), vec![
        (first, stream(b"/Ou")), (second, stream(b"ter Do /Outer Do")),
        (outer, form(b"/Inner Do", Some(nested.into()))), (inner, form(b"q Q", Some(dictionary!{}.into()))),
    ]);
    let report = inventory_page(&doc, page);
    assert_eq!(report.contents.iter().map(|entry| entry.identity.clone()).collect::<Vec<_>>(), vec![Identity::Indirect(first), Identity::Indirect(second)]);
    assert_eq!(report.form_calls.len(), 4);
    assert_eq!(report.form_calls[1].path.len(), 2);
    assert_eq!(report.form_calls[1].path[1].target, Identity::Indirect(inner));
    assert_eq!(report.form_calls[1].path[1].resource_owner, Identity::Indirect(outer));
    assert_ne!(report.form_calls[0].path[0].operation_index, report.form_calls[2].path[0].operation_index);
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("non-strict")));
}

#[test]
fn records_missing_contents_and_never_interprets_partial_sequence() {
    let existing = (10, 0); let missing = (99, 0);
    let (doc, page) = pdf(vec![existing.into(), missing.into()], dictionary!{}.into(), vec![(existing, stream(b"/X Do"))]);
    let report = inventory_page(&doc, page);
    assert_eq!(report.contents.len(), 2);
    assert_eq!(report.contents[1].identity, Identity::Indirect(missing));
    assert!(report.form_calls.is_empty());
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("observation skipped")));
}

#[test]
fn records_form_cycle_and_missing_resource_without_allowing_empty_report() {
    let content = (10, 0); let cycle = (11, 0);
    let resources = dictionary! { "XObject" => dictionary! { "Loop" => cycle } };
    let (doc, page) = pdf(vec![content.into()], resources.into(), vec![
        (content, stream(b"/Loop Do /Absent Do")),
        (cycle, form(b"/Loop Do", None)),
    ]);
    let report = inventory_page(&doc, page);
    assert_eq!(report.form_calls.len(), 2);
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("cycle") && issue.path.len() == 2));
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("resource missing") && issue.path.last().unwrap().name == b"Absent"));
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("semantics")));
}

#[test]
fn records_unknown_invocation_paths_and_escaped_names() {
    let content = (10, 0); let known = (11, 0); let invalid = (12, 0); let image = (13, 0);
    let resources = dictionary! { "XObject" => dictionary! { "A B" => known, "Bad" => invalid, "Pic" => image } };
    let (doc, page) = pdf(vec![content.into()], resources.into(), vec![
        (content, stream(b"/A#20B Do /Bad Do /Pic Do /Unknown Do")),
        (known, form(b"q Q", Some(dictionary!{}.into()))),
        (invalid, Object::Integer(42)),
        (image, Stream::new(dictionary! { "Subtype" => "Image" }, vec![]).into()),
    ]);
    let report = inventory_page(&doc, page);
    assert_eq!(report.form_calls[0].path[0].name, b"A B");
    for (name, detail) in [(b"Bad".as_slice(), "not a stream"), (b"Pic", "image XObject"), (b"Unknown", "resource missing")] {
        assert!(report.uncertainties.iter().any(|issue| issue.detail.contains(detail) && issue.path.last().is_some_and(|call| call.name == name)));
    }
}

#[test]
fn records_form_resource_fallback_and_unresolved_form_resources() {
    let content = (10, 0); let outer = (11, 0); let inner = (12, 0); let broken = (13, 0);
    let resources = dictionary! { "XObject" => dictionary! { "Outer" => outer, "Inner" => inner, "Broken" => broken } };
    let (doc, page) = pdf(vec![content.into()], resources.into(), vec![
        (content, stream(b"/Outer Do /Broken Do")),
        (outer, form(b"/Inner Do", None)),
        (inner, form(b"", Some(dictionary!{}.into()))),
        (broken, form(b"/Lost Do", Some(Object::Reference((99, 0))))),
    ]);
    let report = inventory_page(&doc, page);
    let nested = report.form_calls.iter().find(|call| call.path.len() == 2).unwrap();
    assert_eq!(nested.path[1].target, Identity::Indirect(inner));
    assert_ne!(nested.path[1].resource_owner, Identity::Indirect(outer));
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("fallback")));
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("invalid") && issue.path.last().is_some_and(|call| call.name == b"Broken")));
    assert!(report.uncertainties.iter().any(|issue| issue.detail.contains("no resolvable resources") && issue.path.last().is_some_and(|call| call.name == b"Lost")));
}
