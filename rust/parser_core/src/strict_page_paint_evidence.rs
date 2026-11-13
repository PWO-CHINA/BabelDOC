//! Strict observations of a small subset of a loaded page's original streams.
//!
//! These observations do not bind the document to an IR or writer and do not
//! inspect annotations, text, forms, transparency or arbitrary paths and strokes.
//! Unsupported input invalidates the entire observation; success does not
//! permit changing the page or relaxing a safety gate.

use std::collections::HashSet;

use flate2::{Decompress, FlushDecompress, Status};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::strict_clip_evidence::{self, Evidence, Rect};

const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_TREE_DEPTH: usize = 256;
pub const MAX_CONTENT_STREAMS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageEvidence {
    pub page: ObjectId,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub stream_count: usize,
    pub contents: Evidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uncertainty {
    pub reason: &'static str,
    pub content_offset: Option<usize>,
}

fn failure(reason: &'static str) -> Uncertainty {
    Uncertainty { reason, content_offset: None }
}

fn resolve<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object, Uncertainty> {
    document.dereference(object).map(|(_, object)| object).map_err(|_| failure("unresolved object"))
}

fn integer(document: &Document, object: &Object) -> Result<i128, Uncertainty> {
    match resolve(document, object)? {
        Object::Integer(value) => Ok(i128::from(*value)),
        _ => Err(failure("unsupported or invalid numeric value")),
    }
}

fn rectangle(document: &Document, object: &Object) -> Result<Rect, Uncertainty> {
    let Object::Array(values) = resolve(document, object)? else { return Err(failure("invalid page box")); };
    if values.len() != 4 { return Err(failure("invalid page box")); }
    let bounds = Rect {
        x_min: integer(document, &values[0])?,
        y_min: integer(document, &values[1])?,
        x_max: integer(document, &values[2])?,
        y_max: integer(document, &values[3])?,
    };
    if bounds.x_min >= bounds.x_max || bounds.y_min >= bounds.y_max {
        return Err(failure("empty or inverted page box"));
    }
    Ok(bounds)
}

fn inherited<'a>(document: &'a Document, page: ObjectId) -> Result<(Rect, Rect), Uncertainty> {
    let mut current = page;
    let mut seen = HashSet::new();
    let mut media = None;
    let mut crop = None;
    let mut rotation = None;
    loop {
        if seen.len() >= MAX_PAGE_TREE_DEPTH || !seen.insert(current) { return Err(failure("cyclic or deep page tree")); }
        let dict: &Dictionary = document.get_dictionary(current).map_err(|_| failure("invalid page tree object"))?;
        let expected_type: &[u8] = if current == page { b"Page" } else { b"Pages" };
        if dict.get(b"Type").ok().and_then(|value| value.as_name().ok()) != Some(expected_type) {
            return Err(failure("invalid page tree type"));
        }
        if current == page {
            if let Ok(unit) = dict.get(b"UserUnit") {
                if integer(document, unit)? != 1 { return Err(failure("unsupported page UserUnit")); }
            }
        }
        if media.is_none() { media = dict.get(b"MediaBox").ok(); }
        if crop.is_none() { crop = dict.get(b"CropBox").ok(); }
        if rotation.is_none() { rotation = dict.get(b"Rotate").ok(); }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => current = *parent,
            Ok(_) => return Err(failure("invalid page parent")),
            Err(_) if current == page => return Err(failure("page has no parent")),
            Err(_) => break,
        }
    }
    if rotation.map(|value| integer(document, value)).transpose()?.is_some_and(|value| value % 360 != 0) {
        return Err(failure("unsupported page rotation"));
    }
    let media = rectangle(document, media.ok_or_else(|| failure("missing MediaBox"))?)?;
    let crop = crop.map(|value| rectangle(document, value)).transpose()?.unwrap_or(media);
    let crop = Rect {
        x_min: media.x_min.max(crop.x_min),
        y_min: media.y_min.max(crop.y_min),
        x_max: media.x_max.min(crop.x_max),
        y_max: media.y_max.min(crop.y_max),
    };
    if crop.x_min >= crop.x_max || crop.y_min >= crop.y_max { return Err(failure("empty effective CropBox")); }
    Ok((media, crop))
}

pub(crate) fn append_flate(input: &[u8], output: &mut Vec<u8>) -> Result<(), Uncertainty> {
    let mut decoder = Decompress::new(true);
    let mut chunk = [0u8; 8192];
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let offset = usize::try_from(before_in).map_err(|_| failure("content decode failed"))?;
        let remaining = MAX_CONTENT_BYTES - output.len();
        let capacity = chunk.len().min(remaining.saturating_add(1));
        let status = decoder.decompress(&input[offset..], &mut chunk[..capacity], FlushDecompress::None)
            .map_err(|_| failure("content decode failed"))?;
        let produced = usize::try_from(decoder.total_out() - before_out).map_err(|_| failure("content decode failed"))?;
        if produced > remaining { return Err(failure("content size limit exceeded")); }
        output.try_reserve(produced).map_err(|_| failure("content allocation failed"))?;
        output.extend_from_slice(&chunk[..produced]);
        if status == Status::StreamEnd {
            return if decoder.total_in() == input.len() as u64 { Ok(()) } else { Err(failure("content decode failed")) };
        }
        if decoder.total_in() == before_in && decoder.total_out() == before_out {
            return Err(failure("content decode failed"));
        }
    }
}

/// Observe one already-loaded page. Its object ID and source bytes must be bound independently.
pub fn observe_page(document: &Document, page: ObjectId) -> Result<PageEvidence, Uncertainty> {
    if document.trailer.has(b"Encrypt") || document.was_encrypted() {
        return Err(failure("encrypted page content unsupported"));
    }
    let (media_box, crop_box) = inherited(document, page)?;
    let dict = document.get_dictionary(page).map_err(|_| failure("invalid page"))?;
    let contents = dict.get(b"Contents").map_err(|_| failure("page Contents missing"))?;
    let entries = match resolve(document, contents)? {
        Object::Array(values) => values.as_slice(),
        _ => std::slice::from_ref(contents),
    };
    if entries.is_empty() { return Err(failure("page Contents empty")); }
    if entries.len() > MAX_CONTENT_STREAMS { return Err(failure("page Contents stream limit exceeded")); }
    let mut bytes = Vec::new();
    let mut input_size = 0usize;
    for entry in entries {
        let stream = resolve(document, entry)?.as_stream().map_err(|_| failure("page Contents entry is not a stream"))?;
        if [b"DecodeParms".as_slice(), b"F", b"FFilter", b"FDecodeParms"].iter().any(|key| stream.dict.has(key)) {
            return Err(failure("filtered or external page content unsupported"));
        }
        input_size = input_size.checked_add(stream.content.len()).ok_or_else(|| failure("content size limit exceeded"))?;
        if input_size > MAX_CONTENT_BYTES { return Err(failure("content size limit exceeded")); }
        match stream.dict.get(b"Filter") {
            Err(_) => {
                if stream.content.len() > MAX_CONTENT_BYTES - bytes.len() { return Err(failure("content size limit exceeded")); }
                bytes.try_reserve(stream.content.len()).map_err(|_| failure("content allocation failed"))?;
                bytes.extend_from_slice(&stream.content);
            }
            Ok(Object::Name(name)) if name == b"FlateDecode" => append_flate(&stream.content, &mut bytes)?,
            _ => return Err(failure("filtered or external page content unsupported")),
        }
    }
    let contents = strict_clip_evidence::observe(&bytes, crop_box)
        .map_err(|error| Uncertainty { reason: error.reason, content_offset: Some(error.offset) })?;
    Ok(PageEvidence { page, media_box, crop_box, stream_count: entries.len(), contents })
}
