use babeldoc_parser_core::strict_text_source_evidence::{observe_page, Reason, StreamEncoding, MAX_CONTENT_BYTES, MAX_EVENT_BYTES, MAX_EVENTS, MAX_NAME_BYTES, MAX_REFERENCES, MAX_STATE_DEPTH, MAX_STREAMS, MAX_STRING_BYTES, MAX_TOKENS};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use std::io::Write;

fn compressed(input: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}

fn set_compressed(pdf: &mut Document, id: ObjectId, input: &[u8]) {
    let stream = pdf.get_object_mut(id).unwrap().as_stream_mut().unwrap();
    stream.content = compressed(input).into();
    stream.dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
}

fn fixture(first: &[u8], second: &[u8]) -> (Document, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let page = pdf.new_object_id();
    let first = pdf.add_object(Stream::new(dictionary! {}, first.to_vec()));
    let second = pdf.add_object(Stream::new(dictionary! {}, second.to_vec()));
    let font = pdf.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    pdf.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
    pdf.objects.insert(page, dictionary! {
        "Type" => "Page", "Parent" => pages,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => vec![Object::Reference(first), Object::Reference(second)],
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
    }.into());
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    let mut data = Vec::new();
    pdf.save_to(&mut data).unwrap();
    (Document::load_mem(&data).unwrap(), page, pages, first, second)
}

fn valid() -> (Document, ObjectId, ObjectId, ObjectId, ObjectId) {
    fixture(b"q BT /F1 12 Tf 1 0 0 1 0 0 Tm ", b"<4142> Tj ET Q")
}

#[test]
fn observes_loaded_multistream_event_and_ranges() {
    let (pdf, page, _, _, second) = valid();
    let result = observe_page(&pdf, page).unwrap();
    assert_eq!(result.page, page);
    assert_eq!(result.events.len(), 1);
    let event = &result.events[0];
    assert_eq!(event.sequence, 0);
    assert_eq!(event.page, page);
    assert!(event.resource_name == b"F1");
    assert!(event.encoded_bytes == b"AB");
    assert_eq!(event.operand.stream, second);
    assert_eq!(event.operand.stream_bytes, 0..6);
    assert_eq!(event.operand.encoding, StreamEncoding::Unfiltered);
    assert_eq!(event.operator.stream_bytes, 7..9);
    assert_eq!(event.operand.concatenated.start, b"q BT /F1 12 Tf 1 0 0 1 0 0 Tm ".len());
    let debug = format!("{result:?}");
    assert!(!debug.contains("4142") && !debug.contains("F1"));
    assert!(!result.unverified.is_empty());
}

#[test]
fn observes_saved_reopened_flate_single_hex_array() {
    let content = b"q BT 1 0 0 1 0 0 Tm /F1 12 Tf [<4142>] TJ ET Q";
    let (mut pdf, page, _, first, _) = fixture(b"", b"");
    set_compressed(&mut pdf, first, content);
    pdf.get_dictionary_mut(page).unwrap().set("Contents", Object::Reference(first));
    let compressed_len = pdf.get_object(first).unwrap().as_stream().unwrap().content.len();
    let mut saved = Vec::new();
    pdf.save_to(&mut saved).unwrap();
    let reopened = Document::load_mem(&saved).unwrap();
    let evidence = observe_page(&reopened, page).unwrap();
    let event = &evidence.events[0];
    let start = content.windows(8).position(|value| value == b"[<4142>]").unwrap();
    assert_eq!(event.encoded_bytes, b"AB");
    assert_eq!(event.operand.stream, first);
    assert_eq!(event.operand.stream_bytes, start..start + 8);
    assert_eq!(event.operand.concatenated, start..start + 8);
    assert_eq!(event.operator.stream_bytes, start + 9..start + 11);
    assert_eq!(event.operand.encoding, StreamEncoding::FlateDecode { compressed_stream_bytes: 0..compressed_len });
    assert!(evidence.unverified.contains(&"file absolute stream offsets"));
    let debug = format!("{evidence:?}");
    assert!(!debug.contains("4142") && !debug.contains("F1") && !debug.contains("AB"));
}

#[test]
fn rejects_invalid_flate_and_filters() {
    for variant in 0..8 {
        let (mut pdf, page, _, first, _) = valid();
        set_compressed(&mut pdf, first, b"q Q ");
        let stream = pdf.get_object_mut(first).unwrap().as_stream_mut().unwrap();
        match variant {
            0 => { let last = stream.content.len() - 1; stream.content[last] ^= 1; }
            1 => { let mut data = stream.content.to_vec(); data.pop(); stream.content = data.into(); }
            2 => { let mut data = stream.content.to_vec(); data.push(0); stream.content = data.into(); }
            3 => stream.dict.set("Filter", Object::Name(b"Unknown".to_vec())),
            4 => stream.dict.set("Filter", vec![Object::Name(b"FlateDecode".to_vec())]),
            5 => stream.dict.set("DecodeParms", Object::Null),
            6 => stream.dict.set("F", Object::string_literal("external")),
            _ => stream.dict.set("FFilter", Object::Name(b"FlateDecode".to_vec())),
        }
        assert!(observe_page(&pdf, page).is_err(), "variant {variant}");
    }
    let (mut pdf, page, _, first, _) = valid();
    set_compressed(&mut pdf, first, &vec![b' '; MAX_CONTENT_BYTES + 1]);
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("content bytes"));
}

#[test]
fn rejects_other_arrays_and_cross_stream_arrays() {
    for array in ["[]", "[<41><42>]", "[<41> 1]", "[[<41>]]", "[<4>]", "[<4 1>]", "[(A)]", "[<41>] Tj"] {
        let content = format!("q BT 1 0 0 1 0 0 Tm /F1 12 Tf {array} TJ ET Q");
        let (pdf, page, _, _, _) = fixture(content.as_bytes(), b"");
        assert!(observe_page(&pdf, page).is_err(), "{array}");
    }
    for (first, second) in [
        ("q BT 1 0 0 1 0 0 Tm /F1 12 Tf [", "<41>] TJ ET Q"),
        ("q BT 1 0 0 1 0 0 Tm /F1 12 Tf [<41>", "] TJ ET Q"),
        ("q BT 1 0 0 1 0 0 Tm /F1 12 Tf [<41>] TJ ET Q ", "0 0 m"),
    ] {
        let (pdf, page, _, _, _) = fixture(first.as_bytes(), second.as_bytes());
        assert!(observe_page(&pdf, page).is_err());
    }
}

#[test]
fn maps_operand_and_operator_in_different_streams() {
    let (pdf, page, _, first, second) = fixture(b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41>", b" Tj ET");
    let result = observe_page(&pdf, page).unwrap();
    assert_eq!(result.events[0].operand.stream, first);
    assert_eq!(result.events[0].operator.stream, second);
    assert_eq!(result.events[0].operator.stream_bytes, 1..3);
}

#[test]
fn locates_tokens_after_many_empty_streams() {
    let (mut pdf, page, _, first, second) = fixture(b"", b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41> Tj ET");
    let mut entries = vec![Object::Reference(first); MAX_STREAMS - 2];
    entries.push(Object::Reference(second));
    pdf.get_dictionary_mut(page).unwrap().set("Contents", entries);
    let result = observe_page(&pdf, page).unwrap();
    assert_eq!(result.events[0].operand.stream, second);
    assert_eq!(result.events[0].operator.stream, second);
}

#[test]
fn rejects_encrypted_and_previously_decrypted_snapshot() {
    use lopdf::encryption::{EncryptionState, EncryptionVersion, Permissions};
    let (mut pdf, page, _, _, _) = valid();
    pdf.trailer.set("ID", vec![Object::string_literal("public-test-id"), Object::string_literal("public-test-id")]);
    let version = EncryptionVersion::V1 { document: &pdf, owner_password: "owner", user_password: "user", permissions: Permissions::all() };
    let state = EncryptionState::try_from(version).unwrap();
    pdf.encrypt(&state).unwrap();
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Encrypted);
    pdf.decrypt("user").unwrap();
    assert!(pdf.was_encrypted());
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Encrypted);
}

#[test]
fn rejects_split_tokens_and_invalid_or_late_operations() {
    for (first, second) in [
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41"[..], &b"42> Tj ET"[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <4> Tj ET"[..], &b""[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41 Tj ET"[..], &b""[..]),
        (&b"BT /F1 12-3 Tf 1 0 0 1 0 0 Tm <41> Tj ET"[..], &b""[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41> Tj ET Q"[..], &b""[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <41> Tj ET % comment"[..], &b""[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm (AB) Tj ET"[..], &b""[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <4142> Tj ET "[..], &b"unknown"[..]),
        (&b"BT /F1 12 Tf 1 0 0 1 0 0 Tm <4142> Tj ET "[..], &b"1"[..]),
    ] {
        let (pdf, page, _, _, _) = fixture(first, second);
        assert!(observe_page(&pdf, page).is_err());
    }
}

#[test]
fn rejects_bad_references_and_page_cycles() {
    let (mut pdf, page, pages, _, second) = valid();
    pdf.objects.remove(&second);
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Invalid("dangling reference"));
    pdf.objects.insert(second, Object::Reference(second));
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Invalid("reference cycle"));
    pdf.get_dictionary_mut(pages).unwrap().set("Parent", Object::Reference(pages));
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Invalid("page tree cycle"));
}

#[test]
fn rejects_filter_external_form_annotations_inheritance_and_encryption() {
    for key in ["Filter", "DecodeParms", "F", "FFilter", "FDecodeParms"] {
        let (mut pdf, page, _, first, _) = valid();
        pdf.get_object_mut(first).unwrap().as_stream_mut().unwrap().dict.set(key, 1);
        assert!(matches!(observe_page(&pdf, page).unwrap_err().reason, Reason::Unsupported(_)));
    }
    let (mut pdf, page, _, _, _) = valid();
    pdf.get_dictionary_mut(page).unwrap().set("Annots", Vec::<Object>::new());
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Unsupported("annotations"));
    pdf.get_dictionary_mut(page).unwrap().remove(b"Annots");
    pdf.get_dictionary_mut(page).unwrap().set("Resources", dictionary! { "Font" => dictionary! {}, "XObject" => dictionary! {} });
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Unsupported("XObject resources"));
    pdf.get_dictionary_mut(page).unwrap().remove(b"Resources");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Unsupported("inherited or missing resources"));
    pdf.trailer.set("Encrypt", Object::Null);
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Encrypted);
}

#[test]
fn rejects_state_and_resource_limits() {
    let (mut pdf, page, _, first, _) = valid();
    pdf.get_dictionary_mut(page).unwrap().set("Contents", vec![Object::Reference(first); MAX_STREAMS + 1]);
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("streams"));
    let stream = format!("{}{}", "q ".repeat(MAX_STATE_DEPTH + 1), "Q ".repeat(MAX_STATE_DEPTH + 1));
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("state depth"));
    let stream = format!("BT /F1 12 Tf 1 0 0 1 0 0 Tm {} ET", "<41> Tj ".repeat(MAX_EVENTS + 1));
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("events"));
}

#[test]
fn rejects_token_string_name_and_cumulative_byte_limits() {
    let stream = "q Q ".repeat(MAX_TOKENS / 2 + 1);
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("tokens"));
    let stream = format!("BT /{} 12 Tf ET", "A".repeat(MAX_NAME_BYTES + 1));
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("name bytes"));
    let stream = format!("BT /F1 12 Tf 1 0 0 1 0 0 Tm <{}> Tj ET", "41".repeat(MAX_STRING_BYTES + 1));
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("string bytes"));
    let chunk = format!("<{}> Tj ", "41".repeat(MAX_STRING_BYTES));
    let stream = format!("BT /F1 12 Tf 1 0 0 1 0 0 Tm {} ET", chunk.repeat(MAX_EVENT_BYTES / MAX_STRING_BYTES + 1));
    let (pdf, page, _, _, _) = fixture(stream.as_bytes(), b"");
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("event bytes"));
    let (mut pdf, page, _, first, _) = valid();
    pdf.get_object_mut(first).unwrap().as_stream_mut().unwrap().content = vec![b' '; MAX_CONTENT_BYTES + 1].into();
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("content bytes"));
}

#[test]
fn rejects_reference_limit() {
    let (mut pdf, page, _, first, _) = valid();
    let mut current = first;
    for _ in 0..MAX_REFERENCES {
        let next = pdf.new_object_id();
        pdf.objects.insert(current, Object::Reference(next));
        current = next;
    }
    pdf.objects.insert(current, Stream::new(dictionary! {}, Vec::new()).into());
    assert_eq!(observe_page(&pdf, page).unwrap_err().reason, Reason::Limit("references"));
}
