//! Best-effort raw-stream observations, never a safety or completeness proof.
//!
//! The existing lexer performs recovery and does not report every recovery. PDF
//! graphics, resource and content semantics are not validated by this module.
use std::collections::HashSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::tokens::{Lexer, Token};

const MAX_DEPTH: usize = 64;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_CALLS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Indirect(ObjectId),
    Direct,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentEntry {
    pub index: usize,
    pub identity: Identity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub name: Vec<u8>,
    pub target: Identity,
    pub resource_owner: Identity,
    pub operation_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormCall {
    pub path: Vec<Invocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uncertainty {
    pub path: Vec<Invocation>,
    pub detail: &'static str,
}

#[derive(Clone, Debug)]
pub struct RawStreamInventory {
    pub page: ObjectId,
    pub contents: Vec<ContentEntry>,
    pub form_calls: Vec<FormCall>,
    pub uncertainties: Vec<Uncertainty>,
}

struct Observer<'a> {
    doc: &'a Document,
    report: RawStreamInventory,
    total_bytes: usize,
    attempts: usize,
}

impl<'a> Observer<'a> {
    fn uncertain(&mut self, path: &[Invocation], detail: &'static str) {
        self.report.uncertainties.push(Uncertainty { path: path.to_vec(), detail });
    }

    fn resolve(&self, object: &'a Object) -> Option<(Identity, &'a Object)> {
        let identity = match object {
            Object::Reference(id) => Identity::Indirect(*id),
            _ => Identity::Direct,
        };
        self.doc.dereference(object).ok().map(|(_, value)| (identity, value))
    }

    fn dictionary(&self, object: &'a Object) -> Option<&'a Dictionary> {
        self.resolve(object)?.1.as_dict().ok()
    }

    fn decode(&mut self, stream: &lopdf::Stream, path: &[Invocation]) -> Option<Vec<u8>> {
        let remaining = MAX_TOTAL_BYTES.saturating_sub(self.total_bytes);
        if remaining == 0 {
            self.uncertain(path, "decoded byte budget exhausted");
            return None;
        }
        match stream.get_plain_content_with_limit(remaining.min(MAX_STREAM_BYTES)) {
            Ok(bytes) => { self.total_bytes += bytes.len(); Some(bytes) }
            Err(_) => { self.uncertain(path, "stream decoding failed or exceeded limit"); None }
        }
    }

    fn observe(&mut self, bytes: &[u8], resources: Option<(&'a Dictionary, Identity)>, path: &mut Vec<Invocation>, active: &mut HashSet<ObjectId>) {
        let mut lexer = Lexer::new(bytes);
        let mut index = 0;
        loop {
            let operation = match lexer.operation() {
                Ok(Some(operation)) => operation,
                Ok(None) => break,
                Err(_) => { self.uncertain(path, "content lexer failed; remaining operations unobserved"); break; }
            };
            if operation.op == "Do" {
                self.attempts += 1;
                if self.attempts > MAX_CALLS { self.uncertain(path, "invocation limit reached"); break; }
                let name = match operation.args.as_slice() {
                    [Token::Name(name)] => crate::tokens::decode_name(name).chars().map(|character| character as u8).collect(),
                    _ => { self.uncertain(path, "invalid or ambiguous Do operands"); index += 1; continue; }
                };
                let Some((dict, owner)) = resources.clone() else {
                    path.push(Invocation { name, target: Identity::Missing, resource_owner: Identity::Missing, operation_index: index });
                    self.uncertain(path, "Do has no resolvable resources"); path.pop(); index += 1; continue;
                };
                let target = dict.get(b"XObject").ok().and_then(|obj| self.dictionary(obj))
                    .and_then(|dict| dict.get(&name).ok());
                let Some(target) = target else {
                    path.push(Invocation { name, target: Identity::Missing, resource_owner: owner, operation_index: index });
                    self.uncertain(path, "XObject resource missing or invalid"); path.pop(); index += 1; continue;
                };
                let Some((identity, object)) = self.resolve(target) else {
                    let target = match target { Object::Reference(id) => Identity::Indirect(*id), _ => Identity::Missing };
                    path.push(Invocation { name, target, resource_owner: owner, operation_index: index });
                    self.uncertain(path, "XObject reference missing or cyclic"); path.pop(); index += 1; continue;
                };
                path.push(Invocation { name, target: identity.clone(), resource_owner: owner, operation_index: index });
                let Ok(stream) = object.as_stream() else {
                    self.uncertain(path, "XObject is not a stream"); path.pop(); index += 1; continue;
                };
                match stream.dict.get(b"Subtype").ok().and_then(|obj| obj.as_name().ok()) {
                    Some(b"Image") => { self.uncertain(path, "image XObject semantics unexamined"); }
                    Some(b"Form") => {
                        self.report.form_calls.push(FormCall { path: path.clone() });
                        if path.len() >= MAX_DEPTH { self.uncertain(path, "Form depth limit reached"); }
                        else if matches!(identity, Identity::Indirect(id) if active.contains(&id)) { self.uncertain(path, "recursive Form cycle"); }
                        else {
                            let next_resources = match stream.dict.get(b"Resources") {
                                Ok(value) => match self.dictionary(value) {
                                    Some(dict) => Some((dict, identity.clone())),
                                    None => { self.uncertain(path, "Form resources invalid"); None }
                                },
                                Err(_) => { self.uncertain(path, "Form resources absent; caller resources used as observation fallback"); resources.clone() }
                            };
                            if let Some(bytes) = self.decode(stream, path) {
                                if let Identity::Indirect(id) = identity { active.insert(id); }
                                self.observe(&bytes, next_resources, path, active);
                                if let Identity::Indirect(id) = identity { active.remove(&id); }
                            }
                        }
                    }
                    _ => self.uncertain(path, "XObject subtype missing or unsupported"),
                }
                path.pop();
            }
            index += 1;
        }
    }
}

/// Observe one page in an already loaded document. An empty inventory is never
/// an approval: every result carries unconditional limitations.
pub fn inventory_page(doc: &Document, page: ObjectId) -> RawStreamInventory {
    let mut observer = Observer { doc, report: RawStreamInventory {
        page, contents: Vec::new(), form_calls: Vec::new(), uncertainties: Vec::new(),
    }, total_bytes: 0, attempts: 0 };
    observer.uncertain(&[], "non-strict lexer may recover or discard input without reporting it");
    observer.uncertain(&[], "graphics, clipping, text, resource and other PDF semantics are not validated");
    observer.uncertain(&[], "only Do Form invocations are observed; other operators and dependencies are unsupported");
    let Ok(page_dict) = doc.get_dictionary(page) else {
        observer.uncertain(&[], "page missing or invalid"); return observer.report;
    };
    let mut current = Some((page_dict, Identity::Indirect(page)));
    let mut visited = HashSet::new();
    visited.insert(page);
    let mut resources = None;
    while let Some((dict, owner)) = current {
        if let Ok(value) = dict.get(b"Resources") {
            resources = observer.dictionary(value).map(|dict| (dict, owner));
            if resources.is_none() { observer.uncertain(&[], "page resources invalid"); }
            break;
        }
        current = match dict.get(b"Parent") {
            Ok(Object::Reference(id)) if visited.insert(*id) => match doc.get_dictionary(*id) {
                Ok(dict) => Some((dict, Identity::Indirect(*id))),
                Err(_) => { observer.uncertain(&[], "page parent missing or invalid"); None }
            },
            Ok(_) => { observer.uncertain(&[], "page parent invalid or cyclic"); None }
            Err(_) => None,
        };
    }
    if resources.is_none() { observer.uncertain(&[], "page resources absent or unresolved"); }
    let Ok(contents) = page_dict.get(b"Contents") else {
        observer.uncertain(&[], "page Contents absent"); return observer.report;
    };
    let Some((_, resolved)) = observer.resolve(contents) else {
        observer.report.contents.push(ContentEntry { index: 0, identity: match contents { Object::Reference(id) => Identity::Indirect(*id), _ => Identity::Missing } });
        observer.uncertain(&[], "page Contents missing or cyclic"); return observer.report;
    };
    let entries: Vec<&Object> = match resolved { Object::Array(items) => items.iter().collect(), _ => vec![contents] };
    if entries.is_empty() { observer.uncertain(&[], "empty Contents array"); }
    let mut joined = Vec::new();
    let mut complete = true;
    for (index, entry) in entries.into_iter().enumerate() {
        let identity = match entry { Object::Reference(id) => Identity::Indirect(*id), _ => Identity::Direct };
        observer.report.contents.push(ContentEntry { index, identity });
        match observer.resolve(entry).and_then(|(_, object)| object.as_stream().ok()) {
            Some(stream) => match observer.decode(stream, &[]) {
                Some(bytes) => joined.extend(bytes),
                None => complete = false,
            },
            None => { observer.uncertain(&[], "Contents entry missing, cyclic or not a stream"); complete = false; }
        }
    }
    if complete { observer.observe(&joined, resources, &mut Vec::new(), &mut HashSet::new()); }
    else { observer.uncertain(&[], "page content observation skipped because stream sequence is incomplete"); }
    observer.report
}
