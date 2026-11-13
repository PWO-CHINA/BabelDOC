use babeldoc_ir_core as ir;
use babeldoc_parser_core::{Config, parse};
use babeldoc_writer_core::{BuildOptions, PageObjectMap, apply_page_output_with_page_objects, build_page_output};
use lopdf::{Document, Object, Stream, dictionary};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

fn fixture(content: &str, form_content: Option<&str>, uses: usize) -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("original-rotated-text-fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}-{}.pdf", std::process::id(), NEXT_FILE.fetch_add(1, Ordering::Relaxed)));
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let font = pdf.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let resources = dictionary! { "Font" => dictionary! { "F1" => font } };
    let mut page_resources = resources.clone();
    if let Some(form_content) = form_content {
        let form = pdf.add_object(Stream::new(dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            "Resources" => resources,
        }, form_content.as_bytes().to_vec()));
        page_resources.set("XObject", dictionary! { "Form" => form });
    }
    let body = format!("{} {}", content, "q 1 0 0 1 0 0 cm /Form Do Q ".repeat(uses));
    let stream = pdf.add_object(Stream::new(dictionary! {}, body.into_bytes()));
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
        "Resources" => page_resources, "Contents" => stream,
    });
    pdf.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    pdf.save(&path).unwrap();
    path
}

fn output(path: &PathBuf, options: BuildOptions) -> babeldoc_writer_core::PageOutput {
    let mut parsed = parse(path, Config::default()).unwrap();
    let view = parsed.document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    build_page_output(&view, &page, &options).unwrap()
}

fn write_parsed_pdf(path: &PathBuf, destination: &PathBuf, options: BuildOptions) {
    let mut parsed = parse(path, Config::default()).unwrap();
    let page_objects = PageObjectMap::new(parsed.page_objects.clone());
    let mut pdf = Document::load(path).unwrap();
    let view = parsed.document.view::<ir::ReadOnlyView>();
    for page in view.document().page_iter(&view).unwrap() {
        let output = build_page_output(&view, &page.unwrap(), &options).unwrap();
        let counts = apply_page_output_with_page_objects(&mut pdf, &output, &page_objects).unwrap();
        assert!(counts.form_failures.is_empty(), "{:?}", counts.form_failures);
    }
    pdf.save(destination).unwrap();
}

#[test]
fn mixed_text_keeps_original_show_and_keeps_normal_characters_once() {
    let path = fixture("q 2 0 0 2 10 20 cm 0 0 100 100 re W n BT /F1 12 Tf 1.25 Tc 2 Tw 80 Tz 3 Ts 0 Tr 0.70710678 0.70710678 -0.70710678 0.70710678 30 40 Tm [(A ) 100 <42FF>] TJ 1 0 0 1 100 100 Tm (H) Tj 0 1 -1 0 120 110 Tm (V) Tj ET Q", None, 0);
    let result = output(&path, BuildOptions { skip_curve_render: true, ..Default::default() });
    let stream = String::from_utf8(result.stream).unwrap();
    assert_eq!(stream.matches("[<4120> 100 <42FF>] TJ").count(), 1, "{stream}");
    assert!(stream.contains("1.25 Tc 2 Tw 80 Tz"), "{stream}");
    assert!(stream.contains("0.707107 0.707107 -0.707107 0.707107 30.000000 40.000000 Tm"), "{stream}");
    assert!(stream.contains("2.000000 0.000000 0.000000 2.000000 10.000000 20.000000 cm"), "{stream}");
    assert!(stream.contains("W n"), "{stream}");
    assert_eq!(stream.matches("<48> Tj").count(), 1, "{stream}");
    assert_eq!(stream.matches("<56> Tj").count(), 1, "{stream}");
}

#[test]
fn repeated_shows_keep_spacing_between_operations() {
    let path = fixture("BT /F1 10 Tf 2 Tc 3 Tw 50 Tz 1 1 -1 1 30 40 Tm (A ) Tj (B) Tj ET", None, 0);
    let result = output(&path, BuildOptions::default());
    let stream = String::from_utf8(result.stream).unwrap();
    assert_eq!(stream.matches("[<4120>] TJ").count(), 1, "{stream}");
    assert_eq!(stream.matches("[<42>] TJ").count(), 1, "{stream}");
    let parsed = lopdf::content::Content::decode(stream.as_bytes()).unwrap();
    let matrices: Vec<_> = parsed.operations.iter().filter(|operation| operation.operator == "Tm").collect();
    assert_eq!(matrices.len(), 2, "{stream}");
    let coordinate = |value: &Object| match value { Object::Real(value) => *value as f64, Object::Integer(value) => *value as f64, _ => panic!("not numeric") };
    let x = coordinate(&matrices[1].operands[4]);
    let y = coordinate(&matrices[1].operands[5]);
    assert!(x > 32. && x < 42. && (x - 30. - (y - 40.)).abs() < 1e-4, "{stream}");
}

#[test]
fn form_resources_and_scope_are_preserved() {
    let path = fixture("", Some("BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET"), 1);
    let result = output(&path, BuildOptions::default());
    assert_eq!(result.forms.len(), 1);
    assert!(!String::from_utf8(result.stream).unwrap().contains("<464F524D>"));
    let form = String::from_utf8(result.forms[0].stream.clone()).unwrap();
    assert_eq!(form.matches("[<464F524D>] TJ").count(), 1, "{form}");
    assert!(form.contains("/F1 12 Tf"), "{form}");
}

#[test]
fn shared_form_and_text_clipping_fail_explicitly() {
    let path = fixture("", Some("BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET"), 2);
    let mut parsed = parse(&path, Config::default()).unwrap();
    let view = parsed.document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let error = build_page_output(&view, &page, &BuildOptions::default()).unwrap_err();
    assert!(error.to_string().contains("shared Form"), "{error}");
    let path = fixture("BT /F1 12 Tf 7 Tr 1 1 -1 1 30 40 Tm (CLIP) Tj ET", None, 0);
    let error = match parse(&path, Config::default()) { Ok(_) => panic!("expected clipping error"), Err(error) => error };
    assert!(error.to_string().contains("text clipping"), "{error}");
}

#[test]
fn rotated_form_shared_with_another_page_is_rejected_before_changes() {
    let path = fixture("", Some("BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET"), 1);
    let mut pdf = Document::load(&path).unwrap();
    let first_id = *pdf.get_pages().get(&1).unwrap();
    let first_page = pdf.get_dictionary(first_id).unwrap().clone();
    let first_contents = first_page.get(b"Contents").unwrap().clone();
    let second_id = pdf.add_object(first_page);
    let pages_id = pdf.get_dictionary(first_id).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
    let pages = pdf.get_dictionary_mut(pages_id).unwrap();
    pages.set("Kids", vec![Object::Reference(first_id), Object::Reference(second_id)]);
    pages.set("Count", 2);
    pdf.save(&path).unwrap();

    let mut parsed = parse(&path, Config { pages: Some(vec![0]), ..Default::default() }).unwrap();
    let page_objects = PageObjectMap::new(parsed.page_objects.clone());
    let view = parsed.document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let output = build_page_output(&view, &page, &BuildOptions::default()).unwrap();
    let error = apply_page_output_with_page_objects(&mut pdf, &output, &page_objects).unwrap_err();
    assert!(error.to_string().contains("shared across pages"), "{error}");
    assert_eq!(pdf.get_dictionary(first_id).unwrap().get(b"Contents").unwrap(), &first_contents);
}

#[test]
fn unused_cross_page_form_resource_does_not_block_output() {
    let path = fixture("", Some("BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET"), 1);
    let mut pdf = Document::load(&path).unwrap();
    let first_id = *pdf.get_pages().get(&1).unwrap();
    let mut second_page = pdf.get_dictionary(first_id).unwrap().clone();
    let blank = pdf.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    second_page.set("Contents", blank);
    let second_id = pdf.add_object(second_page);
    let pages_id = pdf.get_dictionary(first_id).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
    let pages = pdf.get_dictionary_mut(pages_id).unwrap();
    pages.set("Kids", vec![Object::Reference(first_id), Object::Reference(second_id)]);
    pages.set("Count", 2);
    pdf.save(&path).unwrap();

    let mut parsed = parse(&path, Config { pages: Some(vec![0]), ..Default::default() }).unwrap();
    let page_objects = PageObjectMap::new(parsed.page_objects.clone());
    let view = parsed.document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let output = build_page_output(&view, &page, &BuildOptions::default()).unwrap();
    let counts = apply_page_output_with_page_objects(&mut pdf, &output, &page_objects).unwrap();
    assert!(counts.form_failures.is_empty(), "{:?}", counts.form_failures);
}

#[test]
fn form_failure_cannot_silently_drop_original_rotated_text() {
    let path = fixture("", Some("BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET"), 1);
    let mut parsed = parse(&path, Config::default()).unwrap();
    let page_objects = PageObjectMap::new(parsed.page_objects.clone());
    let view = parsed.document.view::<ir::ReadOnlyView>();
    let page = view.document().page_at(&view, 0).unwrap();
    let mut output = build_page_output(&view, &page, &BuildOptions::default()).unwrap();
    output.forms[0].xref_id = 999_999;
    let mut pdf = Document::load(&path).unwrap();
    assert!(apply_page_output_with_page_objects(&mut pdf, &output, &page_objects).is_err());
}

#[test]
fn visual_fixture_roundtrip_when_requested() {
    let Some(directory) = std::env::var_os("BABELDOC_ROTATED_FIXTURE_DIR") else { return };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = fixture("q 0.94 g 0 0 300 300 re f 0.75 0.1 0.1 rg 25 130 180 90 re f Q q 1.2 0 0 1.2 20 10 cm 15 75 125 65 re W n BT /F1 18 Tf 1 Tc 2 Tw 85 Tz 0 Tr 0.70710678 0.70710678 -0.70710678 0.70710678 30 90 Tm (ROTATE) Tj [(T) 80 (J)] TJ ET Q BT /F1 14 Tf 1 0 0 1 35 230 Tm (HORIZONTAL) Tj 0 1 -1 0 260 40 Tm (VERTICAL) Tj ET q 0 0 1 rg 95 115 18 30 re f Q", None, 0);
    std::fs::copy(&path, directory.join("synthetic-source.pdf")).unwrap();
    write_parsed_pdf(&path, &directory.join("synthetic-output.pdf"), BuildOptions::default());
    write_parsed_pdf(&path, &directory.join("synthetic-no-curves.pdf"), BuildOptions { skip_curve_render: true, ..Default::default() });
    let form = fixture("q 1.3 0 0 1.3 45 25 cm /Form Do Q", Some("q 0 0 100 100 re W n BT /F1 12 Tf 1 1 -1 1 30 40 Tm (FORM) Tj ET Q"), 0);
    std::fs::copy(&form, directory.join("form-source.pdf")).unwrap();
    write_parsed_pdf(&form, &directory.join("form-output.pdf"), BuildOptions::default());
}

#[test]
fn offline_real_pdf_roundtrip_when_requested() {
    let Some(input) = std::env::var_os("BABELDOC_ROTATED_REAL_INPUT") else { return };
    let Some(output) = std::env::var_os("BABELDOC_ROTATED_REAL_OUTPUT") else { return };
    let input = PathBuf::from(input);
    write_parsed_pdf(&input, &PathBuf::from(output), BuildOptions::default());
}
