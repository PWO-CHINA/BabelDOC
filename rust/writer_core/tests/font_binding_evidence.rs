use babeldoc_writer_core::{
    FontAsset, FontBindingIssue, FontFace, FontMetadata, PdfDocument, embed_font,
    observe_font_binding,
};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};

fn fixture() -> (PdfDocument, ObjectId, ObjectId, Vec<u8>) {
    let path = std::env::var_os("BABELDOC_TEST_FONT")
        .expect("BABELDOC_TEST_FONT must name a profiled public font");
    let face = FontFace::load("public", &path).unwrap();
    let bytes = face.pdf_embedding().unwrap().data.to_vec();
    let metadata = FontMetadata {
        font_name: "SameName".into(),
        ascent: 1.0,
        descent: -0.2,
        encoding_length: 2,
    };
    let mut pdf = PdfDocument::with_version("1.7");
    let font = embed_font(
        &mut pdf,
        &FontAsset {
            id: "F1",
            face: &face,
            metadata: &metadata,
        },
    )
    .unwrap();
    let page = pdf.add_object(dictionary! { "Type" => "Page", "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font) } } });
    (pdf, page, font, bytes)
}

fn descendant(pdf: &PdfDocument, font: ObjectId) -> ObjectId {
    pdf.get_dictionary(font)
        .unwrap()
        .get(b"DescendantFonts")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .as_reference()
        .unwrap()
}

fn font_file(pdf: &PdfDocument, font: ObjectId) -> ObjectId {
    let child = pdf.get_dictionary(descendant(pdf, font)).unwrap();
    let descriptor = child
        .get(b"FontDescriptor")
        .unwrap()
        .as_reference()
        .unwrap();
    pdf.get_dictionary(descriptor)
        .unwrap()
        .get(b"FontFile2")
        .unwrap()
        .as_reference()
        .unwrap()
}

#[test]
fn real_writer_embedding_and_same_name_different_bytes() {
    let (pdf, page, _, bytes) = fixture();
    let result = observe_font_binding(&pdf, page, b"F1", &bytes);
    assert_eq!(result.outcome, Ok(()));
    assert!(result.unverified.iter().any(|item| item.contains("glyph")));
    assert!(result.unverified.iter().any(|item| item.contains("saved")));
    let mut other = bytes.clone();
    other[0] ^= 1;
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &other).outcome,
        Err(FontBindingIssue::FontBytesDiffer)
    );
}

#[test]
fn wrong_encoding_and_cid_mapping_are_rejected() {
    let (mut pdf, page, font, bytes) = fixture();
    pdf.get_dictionary_mut(font)
        .unwrap()
        .set("Encoding", Object::Name(b"Identity-V".to_vec()));
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Unsupported("Encoding"))
    );
    pdf.get_dictionary_mut(font)
        .unwrap()
        .set("Encoding", Object::Name(b"Identity-H".to_vec()));
    let child = descendant(&pdf, font);
    pdf.get_dictionary_mut(child)
        .unwrap()
        .set("CIDToGIDMap", Stream::new(Dictionary::new(), vec![0, 1]));
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Unsupported("CIDToGIDMap"))
    );
    pdf.get_dictionary_mut(child)
        .unwrap()
        .set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Ok(())
    );
}

#[test]
fn missing_corrupt_and_unresolved_are_rejected() {
    let (mut pdf, page, font, bytes) = fixture();
    assert_eq!(
        observe_font_binding(&pdf, page, b"missing", &bytes).outcome,
        Err(FontBindingIssue::Missing("font resource"))
    );
    let file = font_file(&pdf, font);
    let stream = pdf.objects.get_mut(&file).unwrap().as_stream_mut().unwrap();
    stream.content[0] ^= 1;
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::FontBytesDiffer)
    );
    let stream = pdf.objects.get_mut(&file).unwrap().as_stream_mut().unwrap();
    stream
        .dict
        .set("Filter", Object::Name(b"FlateDecode".to_vec()));
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Unsupported("filtered FontFile2"))
    );
    let cycle = pdf.new_object_id();
    pdf.objects.insert(cycle, Object::Reference(cycle));
    pdf.get_dictionary_mut(page)
        .unwrap()
        .set("Resources", Object::Reference(cycle));
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::ReferenceCycle)
    );
}

#[test]
fn external_font_stream_metadata_is_rejected() {
    let (mut pdf, page, font, bytes) = fixture();
    let file = font_file(&pdf, font);
    for key in ["F", "FFilter", "FDecodeParms"] {
        pdf.objects.get_mut(&file).unwrap().as_stream_mut().unwrap().dict.set(
            key,
            Object::string_literal("external"),
        );
        assert_eq!(
            observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
            Err(FontBindingIssue::Unsupported("external FontFile2")),
            "{key} must make the embedded-byte observation uncertain"
        );
        pdf.objects.get_mut(&file).unwrap().as_stream_mut().unwrap().dict.remove(key.as_bytes());
    }
}

#[test]
fn inheritance_forms_encryption_and_limits_are_rejected() {
    let (mut pdf, page, _, bytes) = fixture();
    pdf.get_dictionary_mut(page).unwrap().remove(b"Resources");
    assert!(matches!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Missing(_))
    ));
    pdf.get_dictionary_mut(page).unwrap().set(
        "Resources",
        dictionary! { "Font" => Dictionary::new(), "XObject" => Dictionary::new() },
    );
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Unsupported("XObject/Form resources"))
    );
    pdf.trailer.set("Encrypt", Object::Null);
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &bytes).outcome,
        Err(FontBindingIssue::Encrypted)
    );
    pdf.trailer.remove(b"Encrypt");
    assert_eq!(
        observe_font_binding(&pdf, page, b"F1", &vec![0; 16 * 1024 * 1024 + 1]).outcome,
        Err(FontBindingIssue::SizeLimit)
    );
}
