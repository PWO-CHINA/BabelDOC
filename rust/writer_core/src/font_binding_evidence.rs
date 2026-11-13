//! Read-only evidence for a deliberately narrow page-local font binding.
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashSet;

const MAX_FONT_BYTES: usize = 16 * 1024 * 1024;
const MAX_REFERENCES: usize = 64;

/// This observation never authorizes a writer operation.
#[derive(Debug, PartialEq, Eq)]
pub struct FontBindingEvidence {
    pub outcome: std::result::Result<(), FontBindingIssue>,
    pub unverified: &'static [&'static str],
}

#[derive(Debug, PartialEq, Eq)]
pub enum FontBindingIssue {
    Encrypted,
    Missing(&'static str),
    Invalid(&'static str),
    Unsupported(&'static str),
    ReferenceCycle,
    ReferenceLimit,
    SizeLimit,
    FontBytesDiffer,
}

const UNVERIFIED: &[&str] = &[
    "glyph identity and availability",
    "Unicode mappings and extraction",
    "font widths and metrics",
    "content usage, drawing, and rendering",
    "page-tree membership and caller-selected resource usage",
    "Form resources and inherited resources",
    "final serialization and saved PDF",
];

fn resolve<'a>(pdf: &'a Document, mut object: &'a Object) -> Result<&'a Object, FontBindingIssue> {
    let mut seen = HashSet::new();
    while let Object::Reference(id) = object {
        if !seen.insert(*id) {
            return Err(FontBindingIssue::ReferenceCycle);
        }
        if seen.len() > MAX_REFERENCES {
            return Err(FontBindingIssue::ReferenceLimit);
        }
        object = pdf
            .objects
            .get(id)
            .ok_or(FontBindingIssue::Invalid("dangling reference"))?;
    }
    Ok(object)
}

fn dict<'a>(
    pdf: &'a Document,
    object: &'a Object,
    field: &'static str,
) -> Result<&'a Dictionary, FontBindingIssue> {
    match resolve(pdf, object)? {
        Object::Dictionary(value) => Ok(value),
        _ => Err(FontBindingIssue::Invalid(field)),
    }
}

fn field<'a>(
    dict: &'a Dictionary,
    key: &'static [u8],
    label: &'static str,
) -> Result<&'a Object, FontBindingIssue> {
    dict.get(key).map_err(|_| FontBindingIssue::Missing(label))
}

fn name(
    pdf: &Document,
    dict: &Dictionary,
    key: &'static [u8],
    expected: &[u8],
    label: &'static str,
) -> Result<(), FontBindingIssue> {
    match resolve(pdf, field(dict, key, label)?)? {
        Object::Name(value) if value == expected => Ok(()),
        _ => Err(FontBindingIssue::Unsupported(label)),
    }
}

fn any_name(
    pdf: &Document,
    dict: &Dictionary,
    key: &'static [u8],
    label: &'static str,
) -> Result<(), FontBindingIssue> {
    match resolve(pdf, field(dict, key, label)?)? {
        Object::Name(value) if !value.is_empty() => Ok(()),
        _ => Err(FontBindingIssue::Invalid(label)),
    }
}

fn inspect(
    pdf: &Document,
    page: ObjectId,
    resource_name: &[u8],
    expected: &[u8],
) -> Result<(), FontBindingIssue> {
    if pdf.trailer.has(b"Encrypt") || pdf.is_encrypted() || pdf.was_encrypted() {
        return Err(FontBindingIssue::Encrypted);
    }
    if expected.is_empty() || expected.len() > MAX_FONT_BYTES {
        return Err(FontBindingIssue::SizeLimit);
    }
    if resource_name.is_empty() {
        return Err(FontBindingIssue::Invalid("resource name"));
    }
    let page = dict(
        pdf,
        pdf.objects
            .get(&page)
            .ok_or(FontBindingIssue::Missing("page"))?,
        "page",
    )?;
    name(pdf, page, b"Type", b"Page", "page Type")?;
    let resources = dict(
        pdf,
        field(
            page,
            b"Resources",
            "page-local Resources (inheritance unsupported)",
        )?,
        "Resources",
    )?;
    if resources.has(b"XObject") {
        return Err(FontBindingIssue::Unsupported("XObject/Form resources"));
    }
    let fonts = dict(
        pdf,
        field(resources, b"Font", "Font resources")?,
        "Font resources",
    )?;
    let font = dict(
        pdf,
        fonts
            .get(resource_name)
            .map_err(|_| FontBindingIssue::Missing("font resource"))?,
        "font",
    )?;
    name(pdf, font, b"Type", b"Font", "font Type")?;
    name(pdf, font, b"Subtype", b"Type0", "font Subtype")?;
    any_name(pdf, font, b"BaseFont", "font BaseFont")?;
    name(pdf, font, b"Encoding", b"Identity-H", "Encoding")?;
    let descendants = resolve(pdf, field(font, b"DescendantFonts", "DescendantFonts")?)?;
    let descendant = match descendants {
        Object::Array(items) if items.len() == 1 => dict(pdf, &items[0], "descendant")?,
        _ => return Err(FontBindingIssue::Invalid("DescendantFonts")),
    };
    name(pdf, descendant, b"Type", b"Font", "descendant Type")?;
    name(
        pdf,
        descendant,
        b"Subtype",
        b"CIDFontType2",
        "descendant Subtype",
    )?;
    any_name(pdf, descendant, b"BaseFont", "descendant BaseFont")?;
    let system = dict(
        pdf,
        field(descendant, b"CIDSystemInfo", "CIDSystemInfo")?,
        "CIDSystemInfo",
    )?;
    for (key, expected, label) in [
        (b"Registry".as_slice(), b"Adobe".as_slice(), "Registry"),
        (b"Ordering".as_slice(), b"Identity".as_slice(), "Ordering"),
    ] {
        match resolve(pdf, field(system, key, label)?)? {
            Object::String(value, _) if value == expected => (),
            _ => return Err(FontBindingIssue::Unsupported(label)),
        }
    }
    match resolve(pdf, field(system, b"Supplement", "Supplement")?)? {
        Object::Integer(value) if *value >= 0 => (),
        _ => return Err(FontBindingIssue::Invalid("Supplement")),
    }
    if let Ok(mapping) = descendant.get(b"CIDToGIDMap") {
        match resolve(pdf, mapping)? {
            Object::Name(value) if value == b"Identity" => (),
            _ => return Err(FontBindingIssue::Unsupported("CIDToGIDMap")),
        }
    }
    let descriptor = dict(
        pdf,
        field(descendant, b"FontDescriptor", "FontDescriptor")?,
        "FontDescriptor",
    )?;
    name(
        pdf,
        descriptor,
        b"Type",
        b"FontDescriptor",
        "descriptor Type",
    )?;
    any_name(pdf, descriptor, b"FontName", "descriptor FontName")?;
    let stream = match resolve(pdf, field(descriptor, b"FontFile2", "FontFile2")?)? {
        Object::Stream(stream) => stream,
        _ => return Err(FontBindingIssue::Invalid("FontFile2")),
    };
    if stream.dict.has(b"Filter") || stream.dict.has(b"DecodeParms") {
        return Err(FontBindingIssue::Unsupported("filtered FontFile2"));
    }
    if stream.dict.has(b"F") || stream.dict.has(b"FFilter") || stream.dict.has(b"FDecodeParms") {
        return Err(FontBindingIssue::Unsupported("external FontFile2"));
    }
    if stream.content.len() > MAX_FONT_BYTES {
        return Err(FontBindingIssue::SizeLimit);
    }
    match resolve(pdf, field(&stream.dict, b"Length1", "FontFile2 Length1")?)? {
        Object::Integer(length) if usize::try_from(*length).ok() == Some(stream.content.len()) => {
            ()
        }
        _ => return Err(FontBindingIssue::Invalid("FontFile2 Length1")),
    }
    if stream.content.as_ref() != expected {
        return Err(FontBindingIssue::FontBytesDiffer);
    }
    Ok(())
}

/// Inspect one named entry in a page's own /Resources /Font dictionary.
/// Accepts only unfiltered FontFile2 streams; no content stream is interpreted.
pub fn observe_font_binding(
    pdf: &Document,
    page: ObjectId,
    resource_name: &[u8],
    expected_font_bytes: &[u8],
) -> FontBindingEvidence {
    FontBindingEvidence {
        outcome: inspect(pdf, page, resource_name, expected_font_bytes),
        unverified: UNVERIFIED,
    }
}
