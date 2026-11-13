use babeldoc_parser_core::strict_clip_evidence::Rect;
use babeldoc_parser_core::strict_page_paint_evidence::{observe_page, MAX_CONTENT_STREAMS};
use lopdf::{dictionary, Document, EncryptionState, EncryptionVersion, LoadOptions, Object, ObjectId, Permissions, Stream};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

fn fixture(first: &[u8], second: &[u8]) -> (Document, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let page = document.new_object_id();
    let first = document.add_object(Stream::new(dictionary! {}, first.to_vec()));
    let second = document.add_object(Stream::new(dictionary! {}, second.to_vec()));
    document.objects.insert(pages, dictionary! {
        "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "CropBox" => vec![10.into(), 10.into(), 120.into(), 80.into()],
    }.into());
    document.objects.insert(page, dictionary! {
        "Type" => "Page", "Parent" => pages, "Contents" => vec![Object::Reference(first), Object::Reference(second)],
    }.into());
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    (Document::load_mem(&bytes).unwrap(), pages, page, first, second)
}

fn rect(x_min: i128, y_min: i128, x_max: i128, y_max: i128) -> Rect {
    Rect { x_min, y_min, x_max, y_max }
}

fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn replace_stream(document: &mut Document, id: ObjectId, bytes: Vec<u8>) {
    document.objects.insert(id, Stream::new(dictionary! { "Filter" => "FlateDecode" }, bytes).into());
}

fn reopen(mut document: Document) -> Document {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    Document::load_mem(&bytes).unwrap()
}

fn boundary_fixture(mut document: Document, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    if let Some(directory) = std::env::var_os("STRICT_PAGE_BOUNDARY_FIXTURES") {
        std::fs::write(std::path::Path::new(&directory).join(name), &bytes).unwrap();
    }
    bytes
}

#[test]
fn boundary_exact_limit_and_next_compressed_stream() {
    let (mut document, _, page, first, second) = fixture(b"", b"");
    let mut content = vec![b' '; 16 * 1024 * 1024];
    let paint = b"10 10 20 20 re f ";
    content[..paint.len()].copy_from_slice(paint);
    replace_stream(&mut document, first, compress(&content));
    replace_stream(&mut document, second, compress(b""));
    let bytes = boundary_fixture(document.clone(), "limit-empty.pdf");
    let evidence = observe_page(&Document::load_mem(&bytes).unwrap(), page).unwrap();
    assert_eq!(evidence.contents.paints[0].bounds, Some(rect(10, 10, 30, 30)));

    replace_stream(&mut document, second, compress(b" "));
    let bytes = boundary_fixture(document, "limit-plus-one.pdf");
    assert_eq!(observe_page(&Document::load_mem(&bytes).unwrap(), page).unwrap_err().reason, "content size limit exceeded");
}

#[test]
fn boundary_full_output_still_checks_checksum_and_trailing_data() {
    let full = compress(&vec![b' '; 16 * 1024 * 1024]);
    let empty = compress(b"");
    for (name, corrupt_full, corrupt_checksum) in [
        ("full-bad-checksum.pdf", true, true),
        ("full-trailing.pdf", true, false),
        ("empty-bad-checksum-after-full.pdf", false, true),
        ("empty-trailing-after-full.pdf", false, false),
    ] {
        let (mut document, _, page, first, second) = fixture(b"", b"");
        let mut bad = if corrupt_full { full.clone() } else { empty.clone() };
        if corrupt_checksum { *bad.last_mut().unwrap() ^= 1; } else { bad.push(0); }
        replace_stream(&mut document, first, if corrupt_full { bad.clone() } else { full.clone() });
        replace_stream(&mut document, second, if corrupt_full { empty.clone() } else { bad });
        let bytes = boundary_fixture(document, name);
        assert_eq!(observe_page(&Document::load_mem(&bytes).unwrap(), page).unwrap_err().reason, "content decode failed", "{name}");
    }
}

#[test]
fn boundary_mixed_streams_preserve_split_numbers_and_comments() {
    for (name, first, second, compress_first) in [
        ("plain-flate-number.pdf", b"1".as_slice(), b"0 10 20 20 re f".as_slice(), false),
        ("flate-plain-number.pdf", b"1".as_slice(), b"0 10 20 20 re f".as_slice(), true),
        ("plain-flate-comment.pdf", b"% comment contin".as_slice(), b"ues\n10 10 20 20 re f".as_slice(), false),
        ("flate-plain-comment.pdf", b"% comment contin".as_slice(), b"ues\n10 10 20 20 re f".as_slice(), true),
    ] {
        let (mut document, _, page, first_id, second_id) = fixture(first, second);
        if compress_first { replace_stream(&mut document, first_id, compress(first)); }
        else { replace_stream(&mut document, second_id, compress(second)); }
        let bytes = boundary_fixture(document, name);
        let evidence = observe_page(&Document::load_mem(&bytes).unwrap(), page).unwrap();
        assert_eq!(evidence.contents.paints[0].bounds, Some(rect(10, 10, 30, 30)), "{name}");
    }
}

#[test]
fn boundary_rejects_previously_encrypted_document_after_password_load() {
    let (mut document, _, page, first, _) = fixture(b"", b"");
    replace_stream(&mut document, first, compress(b"10 10 20 20 re f"));
    document.trailer.set("ID", vec![Object::string_literal(vec![1; 16]), Object::string_literal(vec![2; 16])]);
    let state = EncryptionState::try_from(EncryptionVersion::V1 {
        document: &document, owner_password: "owner", user_password: "user", permissions: Permissions::all(),
    }).unwrap();
    document.encrypt(&state).unwrap();
    let bytes = boundary_fixture(document, "encrypted.pdf");
    let document = Document::load_mem_with_options(&bytes, LoadOptions::with_password("user")).unwrap();
    assert!(document.was_encrypted());
    assert!(!document.trailer.has(b"Encrypt"));
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "encrypted page content unsupported");
}

#[test]
fn observes_reopened_flate_multistream_with_split_operators() {
    let (mut document, _, page, first, second) = fixture(b"", b"");
    replace_stream(&mut document, first, compress(b"q 0 0 30 30 r"));
    replace_stream(&mut document, second, compress(b"e f Q 90 70 30 30 re f*"));
    let document = reopen(document);
    assert_eq!(document.get_object(first).unwrap().as_stream().unwrap().dict.get(b"Filter").unwrap().as_name().unwrap(), b"FlateDecode");
    let evidence = observe_page(&document, page).unwrap();
    assert_eq!(evidence.stream_count, 2);
    assert_eq!(evidence.contents.paints.iter().map(|event| event.bounds).collect::<Vec<_>>(), vec![Some(rect(10, 10, 30, 30)), Some(rect(90, 70, 100, 80))]);
}

#[test]
fn rejects_corrupt_truncated_trailing_and_concatenated_zlib() {
    let valid = compress(b"0 0 10 10 re f ");
    let mut corrupt = valid.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut concatenated = valid.clone();
    concatenated.extend_from_slice(&compress(b"q Q"));
    for data in [corrupt, valid[..valid.len() - 1].to_vec(), trailing, concatenated] {
        let (mut document, _, page, first, _) = fixture(b"", b"");
        replace_stream(&mut document, first, data);
        assert_eq!(observe_page(&reopen(document), page).unwrap_err().reason, "content decode failed");
    }
}

#[test]
fn rejects_unsupported_filter_parameters_and_external_streams() {
    let (document, _, page, first, _) = fixture(b"", b"");
    for value in [Object::Integer(42), Object::Name(b"Fl".to_vec()), Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]), Object::Null] {
        let mut candidate = document.clone();
        replace_stream(&mut candidate, first, compress(b"q Q"));
        candidate.get_object_mut(first).unwrap().as_stream_mut().unwrap().dict.set("Filter", value);
        assert_eq!(observe_page(&reopen(candidate), page).unwrap_err().reason, "filtered or external page content unsupported");
    }
    for key in ["DecodeParms", "F", "FFilter", "FDecodeParms"] {
        let mut candidate = document.clone();
        replace_stream(&mut candidate, first, compress(b"q Q"));
        candidate.get_object_mut(first).unwrap().as_stream_mut().unwrap().dict.set(key, Object::Null);
        assert_eq!(observe_page(&reopen(candidate), page).unwrap_err().reason, "filtered or external page content unsupported");
    }
}

#[test]
fn rejects_flate_output_and_cumulative_input_limits() {
    let (mut document, _, page, first, second) = fixture(b"", b"");
    replace_stream(&mut document, first, compress(&vec![b' '; 16 * 1024 * 1024]));
    replace_stream(&mut document, second, compress(b" "));
    assert_eq!(observe_page(&reopen(document), page).unwrap_err().reason, "content size limit exceeded");
    let (mut document, _, page, first, second) = fixture(b"", b"");
    document.objects.insert(first, Stream::new(dictionary! {}, vec![b' '; 16 * 1024 * 1024]).into());
    document.objects.insert(second, Stream::new(dictionary! {}, vec![b' ']).into());
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "content size limit exceeded");
}

#[test]
fn rejects_late_text_after_successful_flate_and_declared_encryption() {
    let (mut document, _, page, first, second) = fixture(b"", b"");
    replace_stream(&mut document, first, compress(b"0 0 10 10 re f "));
    replace_stream(&mut document, second, compress(b"BT ET"));
    let mut document = reopen(document);
    let error = observe_page(&document, page).unwrap_err();
    assert_eq!(error.reason, "unsupported or invalid operator");
    assert!(error.content_offset.is_some());
    document.trailer.set("Encrypt", Object::Null);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "encrypted page content unsupported");
}

#[test]
fn observes_reopened_multistream_page_and_inherited_clip() {
    let (document, _, page, _, _) = fixture(b"q 0 0 30 30 r", b"e f Q 90 70 30 30 re f*");
    let evidence = observe_page(&document, page).unwrap();
    assert_eq!(evidence.page, page);
    assert_eq!(evidence.media_box, rect(0, 0, 100, 100));
    assert_eq!(evidence.crop_box, rect(10, 10, 100, 80));
    assert_eq!(evidence.stream_count, 2);
    assert_eq!(evidence.contents.paints.iter().map(|event| event.bounds).collect::<Vec<_>>(), vec![
        Some(rect(10, 10, 30, 30)), Some(rect(90, 70, 100, 80)),
    ]);
}

#[test]
fn unsupported_late_operator_invalidates_all_paint_observations() {
    let (document, _, page, _, _) = fixture(b"0 0 10 10 re f ", b"BT ET");
    let error = observe_page(&document, page).unwrap_err();
    assert_eq!(error.reason, "unsupported or invalid operator");
    assert!(error.content_offset.is_some());
}

#[test]
fn preserves_unclipped_bounds_and_path_across_graphics_state() {
    let (document, _, page, _, _) = fixture(b"0 0 100 100 re q f Q 200 200", b" 10 10 re f");
    let evidence = observe_page(&document, page).unwrap();
    assert_eq!(evidence.contents.paints[0].path_bounds, rect(0, 0, 100, 100));
    assert_eq!(evidence.contents.paints[0].bounds, Some(rect(10, 10, 100, 80)));
    assert_eq!(evidence.contents.paints[1].path_bounds, rect(200, 200, 210, 210));
    assert_eq!(evidence.contents.paints[1].bounds, None);
}

#[test]
fn rejects_too_many_streams_even_if_they_are_empty() {
    let (mut document, _, page, first, _) = fixture(b"", b"");
    let entries = vec![Object::Reference(first); MAX_CONTENT_STREAMS + 1];
    document.get_dictionary_mut(page).unwrap().set("Contents", entries);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "page Contents stream limit exceeded");
}

#[test]
fn handles_comment_and_number_split_across_streams() {
    let (document, _, page, _, _) = fixture(b"% comment contin", b"ues\n1", );
    let error = observe_page(&document, page).unwrap_err();
    assert_eq!(error.reason, "incomplete operands, path, clip or graphics stack");
    let (document, _, page, _, _) = fixture(b"% comment contin", b"ues\n10 10 20 20 re f");
    assert_eq!(observe_page(&document, page).unwrap().contents.paints[0].bounds, Some(rect(10, 10, 30, 30)));
    let (document, _, page, _, _) = fixture(b"1", b"0 10 20 20 re f");
    assert_eq!(observe_page(&document, page).unwrap().contents.paints[0].bounds, Some(rect(10, 10, 30, 30)));
}

#[test]
fn rejects_missing_stream_invalid_filter_and_external_content() {
    let (mut document, _, page, first, second) = fixture(b"0 0 10 10 re f", b"");
    document.objects.remove(&second);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "unresolved object");
    document.objects.insert(second, Stream::new(dictionary! {}, Vec::new()).into());
    let stream = document.get_object_mut(first).unwrap().as_stream_mut().unwrap();
    stream.dict.set("Filter", 42);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "filtered or external page content unsupported");
    let stream = document.get_object_mut(first).unwrap().as_stream_mut().unwrap();
    stream.dict.remove(b"Filter");
    stream.dict.set("F", Object::string_literal("external"));
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "filtered or external page content unsupported");
}

#[test]
fn rejects_rotation_user_unit_empty_crop_and_page_tree_cycles() {
    let (mut document, pages, page, _, _) = fixture(b"q", b" Q");
    document.get_dictionary_mut(pages).unwrap().set("Rotate", 90);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "unsupported page rotation");
    document.get_dictionary_mut(pages).unwrap().remove(b"Rotate");
    document.get_dictionary_mut(page).unwrap().set("UserUnit", 2);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "unsupported page UserUnit");
    document.get_dictionary_mut(page).unwrap().remove(b"UserUnit");
    document.get_dictionary_mut(page).unwrap().set("CropBox", vec![200.into(), 200.into(), 300.into(), 300.into()]);
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "empty effective CropBox");
    document.get_dictionary_mut(page).unwrap().remove(b"CropBox");
    document.get_dictionary_mut(pages).unwrap().set("Parent", Object::Reference(pages));
    assert_eq!(observe_page(&document, page).unwrap_err().reason, "cyclic or deep page tree");
}
