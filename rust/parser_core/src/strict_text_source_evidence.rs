//! Local observations of a borrowed, already-loaded PDF snapshot only.
//! Success is not a safety decision and never authorizes writing.

use std::collections::HashSet;
use std::ops::Range;

use lopdf::{Dictionary, Document, Object, ObjectId};

pub const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_STREAMS: usize = 4096;
pub const MAX_REFERENCES: usize = 4096;
pub const MAX_EVENTS: usize = 4096;
pub const MAX_STATE_DEPTH: usize = 64;
pub const MAX_TOKENS: usize = 100_000;
pub const MAX_NAME_BYTES: usize = 127;
pub const MAX_STRING_BYTES: usize = 64 * 1024;
pub const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

pub const UNVERIFIED: &[&str] = &[
    "file and IR identity or correspondence",
    "byte integrity during loading and preprocessing",
    "file absolute stream offsets",
    "font resource interpretation, encoding, Unicode and glyph identity",
    "graphics transformations, clipping, outlines and visibility",
    "writer behavior, final PDF and safety authorization",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Encrypted,
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uncertainty {
    pub reason: Reason,
    pub offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRange {
    pub stream: ObjectId,
    /// Offset in the concatenation of decoded stream contents, not the PDF file.
    pub concatenated: Range<usize>,
    /// Offset in this stream's decoded contents; raw stream bytes only when unfiltered.
    pub stream_bytes: Range<usize>,
    pub encoding: StreamEncoding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEncoding {
    Unfiltered,
    /// Entire compressed content range in the loaded stream, not an absolute file range.
    FlateDecode { compressed_stream_bytes: Range<usize> },
}

#[derive(Clone, PartialEq, Eq)]
pub struct TextEvent {
    pub sequence: usize,
    pub page: ObjectId,
    pub resource_name: Vec<u8>,
    pub encoded_bytes: Vec<u8>,
    pub operand: SourceRange,
    pub operator: SourceRange,
}

impl std::fmt::Debug for TextEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEvent").field("sequence", &self.sequence)
            .field("page", &self.page).field("operand", &self.operand)
            .field("operator", &self.operator).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub page: ObjectId,
    pub events: Vec<TextEvent>,
    pub unverified: &'static [&'static str],
}

fn fail(reason: Reason, offset: Option<usize>) -> Uncertainty { Uncertainty { reason, offset } }
fn invalid(label: &'static str) -> Uncertainty { fail(Reason::Invalid(label), None) }
fn unsupported(label: &'static str) -> Uncertainty { fail(Reason::Unsupported(label), None) }
fn limit(label: &'static str) -> Uncertainty { fail(Reason::Limit(label), None) }

struct Resolver<'a> { pdf: &'a Document, references: usize }
impl<'a> Resolver<'a> {
    fn resolve(&mut self, mut value: &'a Object) -> Result<(&'a Object, Option<ObjectId>), Uncertainty> {
        let mut seen = HashSet::new();
        let mut last = None;
        while let Object::Reference(id) = value {
            self.references = self.references.checked_add(1).ok_or_else(|| limit("references"))?;
            if self.references > MAX_REFERENCES { return Err(limit("references")); }
            if !seen.insert(*id) { return Err(invalid("reference cycle")); }
            last = Some(*id);
            value = self.pdf.objects.get(id).ok_or_else(|| invalid("dangling reference"))?;
        }
        Ok((value, last))
    }
    fn dict(&mut self, value: &'a Object) -> Result<&'a Dictionary, Uncertainty> {
        match self.resolve(value)?.0 { Object::Dictionary(dict) => Ok(dict), _ => Err(invalid("dictionary")) }
    }
}

struct Segment { id: ObjectId, start: usize, end: usize, encoding: StreamEncoding }

fn source(segments: &[Segment], range: Range<usize>) -> Result<SourceRange, Uncertainty> {
    let index = segments.partition_point(|segment| segment.end <= range.start);
    let segment = segments.get(index).filter(|segment| range.start >= segment.start && range.end <= segment.end && range.start < segment.end)
        .ok_or_else(|| fail(Reason::Unsupported("token crosses streams"), Some(range.start)))?;
    Ok(SourceRange { stream: segment.id, stream_bytes: (range.start - segment.start)..(range.end - segment.start), concatenated: range, encoding: segment.encoding.clone() })
}

fn whitespace(byte: u8) -> bool { matches!(byte, 0 | 9 | 10 | 12 | 13 | 32) }
fn delimiter(byte: u8) -> bool { whitespace(byte) || b"()<>[]{}/%".contains(&byte) }

#[derive(Clone)]
enum TokenKind { Name(Vec<u8>), Integer(i64), Hex(Vec<u8>), ArrayHex(Vec<u8>), Operator }
struct Token { kind: TokenKind, range: Range<usize> }

fn token(bytes: &[u8], cursor: &mut usize, segments: &[Segment]) -> Result<Token, Uncertainty> {
    let start = *cursor;
    let error = || fail(Reason::Invalid("invalid token"), Some(start));
    let kind = match bytes[start] {
        b'[' => {
            *cursor += 1;
            while *cursor < bytes.len() && whitespace(bytes[*cursor]) { *cursor += 1; }
            if *cursor == bytes.len() || bytes[*cursor] != b'<' { return Err(error()); }
            let inner = token(bytes, cursor, segments)?;
            let TokenKind::Hex(decoded) = inner.kind else { return Err(error()); };
            while *cursor < bytes.len() && whitespace(bytes[*cursor]) { *cursor += 1; }
            if *cursor == bytes.len() || bytes[*cursor] != b']' { return Err(error()); }
            *cursor += 1;
            TokenKind::ArrayHex(decoded)
        }
        b'/' => {
            *cursor += 1;
            let name_start = *cursor;
            while *cursor < bytes.len() && !delimiter(bytes[*cursor]) {
                if bytes[*cursor] == b'#' || !(33..=126).contains(&bytes[*cursor]) { return Err(error()); }
                *cursor += 1;
            }
            if *cursor == name_start { return Err(error()); }
            if *cursor - name_start > MAX_NAME_BYTES { return Err(fail(Reason::Limit("name bytes"), Some(start))); }
            TokenKind::Name(bytes[name_start..*cursor].to_vec())
        }
        b'<' => {
            *cursor += 1;
            let mut digits = Vec::new();
            while *cursor < bytes.len() && bytes[*cursor] != b'>' {
                let byte = bytes[*cursor];
                if !byte.is_ascii_hexdigit() { return Err(error()); }
                if digits.len() >= MAX_STRING_BYTES * 2 { return Err(fail(Reason::Limit("string bytes"), Some(start))); }
                digits.push(byte);
                *cursor += 1;
            }
            if *cursor == bytes.len() || digits.len() % 2 != 0 { return Err(error()); }
            *cursor += 1;
            let mut decoded = Vec::with_capacity(digits.len() / 2);
            for pair in digits.chunks_exact(2) {
                let digit = |byte: u8| if byte <= b'9' { byte - b'0' } else { (byte | 32) - b'a' + 10 };
                decoded.push(digit(pair[0]) * 16 + digit(pair[1]));
            }
            TokenKind::Hex(decoded)
        }
        byte if !delimiter(byte) => {
            while *cursor < bytes.len() && !delimiter(bytes[*cursor]) { *cursor += 1; }
            let value = &bytes[start..*cursor];
            let number = value[0].is_ascii_digit() || matches!(value[0], b'+' | b'-');
            if number {
                let digits = if matches!(value[0], b'+' | b'-') { &value[1..] } else { value };
                if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) { return Err(error()); }
                let integer = std::str::from_utf8(value).ok().and_then(|value| value.parse().ok()).ok_or_else(error)?;
                TokenKind::Integer(integer)
            } else { TokenKind::Operator }
        }
        _ => return Err(error()),
    };
    let range = start..*cursor;
    source(segments, range.clone())?;
    Ok(Token { kind, range })
}

/// Observe a complete page in the current in-memory snapshot; no partial result on failure.
pub fn observe_page(pdf: &Document, page: ObjectId) -> Result<Evidence, Uncertainty> {
    if pdf.trailer.has(b"Encrypt") || pdf.is_encrypted() || pdf.was_encrypted() { return Err(fail(Reason::Encrypted, None)); }
    let mut resolver = Resolver { pdf, references: 0 };
    let page_dict = match pdf.objects.get(&page) { Some(Object::Dictionary(dict)) => dict, _ => return Err(invalid("page")) };
    if page_dict.get(b"Type").ok().and_then(|value| value.as_name().ok()) != Some(b"Page".as_slice()) { return Err(invalid("page type")); }
    if page_dict.has(b"Annots") { return Err(unsupported("annotations")); }
    let mut parents = HashSet::new();
    parents.insert(page);
    let mut parent = page_dict.get(b"Parent").map_err(|_| invalid("page parent"))?;
    loop {
        let Object::Reference(id) = parent else { return Err(invalid("page parent")); };
        if !parents.insert(*id) { return Err(invalid("page tree cycle")); }
        if parents.len() > MAX_STATE_DEPTH { return Err(limit("page tree depth")); }
        let object = pdf.objects.get(id).ok_or_else(|| invalid("page parent"))?;
        let dict = resolver.dict(object)?;
        if dict.get(b"Type").ok().and_then(|value| value.as_name().ok()) != Some(b"Pages".as_slice()) { return Err(invalid("page tree type")); }
        match dict.get(b"Parent") { Ok(value) => parent = value, Err(_) => break }
    }
    let resources = resolver.dict(page_dict.get(b"Resources").map_err(|_| unsupported("inherited or missing resources"))?)?;
    if resources.has(b"XObject") { return Err(unsupported("XObject resources")); }
    if resources.iter().any(|(key, _)| key.as_slice() != b"Font") { return Err(unsupported("resource category")); }
    let fonts = resolver.dict(resources.get(b"Font").map_err(|_| invalid("font resources"))?)?;
    if fonts.iter().count() > MAX_REFERENCES { return Err(limit("font resources")); }
    for (_, value) in fonts.iter() { resolver.dict(value)?; }
    let contents = page_dict.get(b"Contents").map_err(|_| invalid("missing contents"))?;
    let resolved = resolver.resolve(contents)?.0;
    let entries = match resolved { Object::Array(values) => values.as_slice(), _ => std::slice::from_ref(contents) };
    if entries.is_empty() { return Err(invalid("empty contents")); }
    if entries.len() > MAX_STREAMS { return Err(limit("streams")); }
    let mut bytes = Vec::new();
    let mut segments = Vec::new();
    let mut input_size = 0usize;
    for entry in entries {
        let (value, id) = resolver.resolve(entry)?;
        let id = id.ok_or_else(|| invalid("direct content stream"))?;
        let Object::Stream(stream) = value else { return Err(invalid("content stream")); };
        if [b"DecodeParms".as_slice(), b"F", b"FFilter", b"FDecodeParms"].iter().any(|key| stream.dict.has(key)) { return Err(unsupported("filtered or external content")); }
        input_size = input_size.checked_add(stream.content.len()).ok_or_else(|| limit("content bytes"))?;
        if input_size > MAX_CONTENT_BYTES { return Err(limit("content bytes")); }
        let start = bytes.len();
        let encoding = match stream.dict.get(b"Filter") {
            Err(_) => {
                if stream.content.len() > MAX_CONTENT_BYTES - start { return Err(limit("content bytes")); }
                bytes.try_reserve(stream.content.len()).map_err(|_| limit("allocation"))?;
                bytes.extend_from_slice(&stream.content);
                StreamEncoding::Unfiltered
            }
            Ok(Object::Name(name)) if name == b"FlateDecode" => {
                crate::strict_page_paint_evidence::append_flate(&stream.content, &mut bytes).map_err(|error| match error.reason {
                    "content size limit exceeded" => limit("content bytes"),
                    "content allocation failed" => limit("allocation"),
                    _ => invalid("content decode failed"),
                })?;
                StreamEncoding::FlateDecode { compressed_stream_bytes: 0..stream.content.len() }
            }
            _ => return Err(unsupported("filtered or external content")),
        };
        let end = bytes.len();
        segments.push(Segment { id, start, end, encoding });
    }
    let mut cursor = 0;
    let mut operands = Vec::<Token>::new();
    let mut events = Vec::new();
    let mut in_text = false;
    let mut matrix = false;
    let mut font: Option<Vec<u8>> = None;
    let mut states = Vec::new();
    let mut token_count = 0usize;
    let mut event_bytes = 0usize;
    while cursor < bytes.len() {
        if whitespace(bytes[cursor]) { cursor += 1; continue; }
        token_count += 1;
        if token_count > MAX_TOKENS { return Err(fail(Reason::Limit("tokens"), Some(cursor))); }
        let item = token(&bytes, &mut cursor, &segments)?;
        if !matches!(item.kind, TokenKind::Operator) {
            if operands.len() == 6 { return Err(fail(Reason::Limit("operands"), Some(item.range.start))); }
            operands.push(item);
            continue;
        }
        let offset = item.range.start;
        let op = &bytes[item.range.clone()];
        let bad = || fail(Reason::Invalid("operator operands or state"), Some(offset));
        match op {
            b"q" if !in_text && operands.is_empty() => {
                if states.len() >= MAX_STATE_DEPTH { return Err(fail(Reason::Limit("state depth"), Some(offset))); }
                states.push(font.clone());
            }
            b"Q" if !in_text && operands.is_empty() => font = states.pop().ok_or_else(bad)?,
            b"BT" if !in_text && operands.is_empty() => { in_text = true; matrix = false; }
            b"ET" if in_text && operands.is_empty() => { in_text = false; matrix = false; }
            b"Tf" if in_text && operands.len() == 2 => {
                let (TokenKind::Name(name), TokenKind::Integer(size)) = (&operands[0].kind, &operands[1].kind) else { return Err(bad()); };
                if *size <= 0 || !fonts.has(name) { return Err(bad()); }
                font = Some(name.clone());
            }
            b"Tm" if in_text && operands.len() == 6 && operands.iter().all(|operand| matches!(operand.kind, TokenKind::Integer(_))) => matrix = true,
            b"Tj" | b"TJ" if in_text && matrix && operands.len() == 1 => {
                let encoded = match (&operands[0].kind, op) {
                    (TokenKind::Hex(encoded), b"Tj") | (TokenKind::ArrayHex(encoded), b"TJ") => encoded,
                    _ => return Err(bad()),
                };
                let name = font.as_ref().ok_or_else(bad)?;
                if events.len() >= MAX_EVENTS { return Err(fail(Reason::Limit("events"), Some(offset))); }
                event_bytes = event_bytes.checked_add(name.len()).and_then(|size| size.checked_add(encoded.len())).ok_or_else(|| fail(Reason::Limit("event bytes"), Some(offset)))?;
                if event_bytes > MAX_EVENT_BYTES { return Err(fail(Reason::Limit("event bytes"), Some(offset))); }
                events.push(TextEvent { sequence: events.len(), page, resource_name: name.clone(), encoded_bytes: encoded.clone(), operand: source(&segments, operands[0].range.clone())?, operator: source(&segments, item.range)? });
            }
            b"q" | b"Q" | b"BT" | b"ET" | b"Tf" | b"Tm" | b"Tj" | b"TJ" => return Err(bad()),
            _ => return Err(fail(Reason::Unsupported("operator"), Some(offset))),
        }
        operands.clear();
    }
    if in_text || !states.is_empty() || !operands.is_empty() { return Err(fail(Reason::Invalid("incomplete state"), Some(bytes.len()))); }
    Ok(Evidence { page, events, unverified: UNVERIFIED })
}
