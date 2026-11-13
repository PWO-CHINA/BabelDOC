//! Conservative page-tree ordinal observation; this does not establish IR lineage,
//! paragraph identity, drawing preservation, or the identity of any output PDF.
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashSet;

const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uncertainty { Invalid(&'static str), Limit(&'static str), Cancelled }

fn reference(value: &Object) -> Result<ObjectId, Uncertainty> {
    value.as_reference().map_err(|_| Uncertainty::Invalid("expected direct reference"))
}

fn dictionary(pdf: &Document, id: ObjectId) -> Result<&Dictionary, Uncertainty> {
    match pdf.objects.get(&id) {
        Some(Object::Dictionary(dict)) => Ok(dict),
        _ => Err(Uncertainty::Invalid("page tree object")),
    }
}

fn field<'a>(dict: &'a Dictionary, name: &[u8]) -> Result<&'a Object, Uncertainty> {
    dict.get(name).map_err(|_| Uncertainty::Invalid("missing page tree field"))
}

fn walk(pdf: &Document, id: ObjectId, parent: Option<ObjectId>, depth: usize, size: i64,
        seen: &mut HashSet<ObjectId>, pages: &mut Vec<ObjectId>, cancelled: &mut impl FnMut() -> bool)
        -> Result<usize, Uncertainty> {
    if cancelled() { return Err(Uncertainty::Cancelled); }
    if depth > MAX_DEPTH || seen.len() >= MAX_NODES { return Err(Uncertainty::Limit("page tree")); }
    if id.0 == 0 || i64::from(id.0) >= size { return Err(Uncertainty::Invalid("page tree object outside trailer size")); }
    if !seen.insert(id) { return Err(Uncertainty::Invalid("repeated or cyclic page tree object")); }
    let dict = dictionary(pdf, id)?;
    if let Some(parent) = parent {
        if reference(field(dict, b"Parent")?)? != parent { return Err(Uncertainty::Invalid("page parent")); }
    } else if dict.has(b"Parent") { return Err(Uncertainty::Invalid("root parent")); }
    match field(dict, b"Type")?.as_name() {
        Ok(b"Page") if parent.is_some() => { pages.push(id); Ok(1) }
        Ok(b"Pages") => {
            let count = match field(dict, b"Count")? {
                Object::Integer(count) if *count >= 0 => usize::try_from(*count).map_err(|_| Uncertainty::Limit("page count"))?,
                _ => return Err(Uncertainty::Invalid("page count")),
            };
            let kids = match field(dict, b"Kids")? {
                Object::Array(kids) => kids,
                _ => return Err(Uncertainty::Invalid("page kids")),
            };
            if kids.len() > MAX_NODES { return Err(Uncertainty::Limit("page kids")); }
            let mut total = 0usize;
            for kid in kids {
                total = total.checked_add(walk(pdf, reference(kid)?, Some(id), depth + 1, size, seen, pages, cancelled)?)
                    .ok_or(Uncertainty::Limit("page count"))?;
            }
            if total != count { return Err(Uncertainty::Invalid("page count mismatch")); }
            Ok(total)
        }
        _ => Err(Uncertainty::Invalid("page tree type")),
    }
}

/// Returns source page object IDs in the parser strict fast-path order only.
pub fn observe(pdf: &Document, mut cancelled: impl FnMut() -> bool) -> Result<Vec<ObjectId>, Uncertainty> {
    if cancelled() { return Err(Uncertainty::Cancelled); }
    if pdf.is_encrypted() || pdf.was_encrypted() || pdf.trailer.has(b"Encrypt") { return Err(Uncertainty::Invalid("encrypted document")); }
    let size = match field(&pdf.trailer, b"Size")? {
        Object::Integer(size) if *size > 0 => *size,
        _ => return Err(Uncertainty::Invalid("trailer size")),
    };
    let catalog_id = reference(field(&pdf.trailer, b"Root")?)?;
    if catalog_id.0 == 0 || i64::from(catalog_id.0) >= size { return Err(Uncertainty::Invalid("catalog outside trailer size")); }
    let catalog = dictionary(pdf, catalog_id)?;
    if field(catalog, b"Type")?.as_name().ok() != Some(b"Catalog") { return Err(Uncertainty::Invalid("catalog type")); }
    let root = reference(field(catalog, b"Pages")?)?;
    let mut pages = Vec::new();
    walk(pdf, root, None, 0, size, &mut HashSet::new(), &mut pages, &mut cancelled)?;
    if pages.len() as i64 >= size { return Err(Uncertainty::Invalid("page count exceeds trailer size")); }
    if cancelled() { return Err(Uncertainty::Cancelled); }
    Ok(pages)
}
