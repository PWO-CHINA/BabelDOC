use babeldoc_parser_core::{parse_observed, Config, ParseObserver, Result};
use lopdf::{dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

fn path() -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("source-identity-fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{}-{}.pdf", std::process::id(), NEXT_FILE.fetch_add(1, Ordering::Relaxed)))
}

fn document() -> Document {
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let content = doc.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Resources" => dictionary! {}, "Contents" => content,
    });
    doc.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    doc.trailer.set("ID", Object::Array(vec![Object::string_literal(vec![1; 16]), Object::string_literal(vec![2; 16])]));
    doc
}

fn bytes(mut doc: Document) -> Vec<u8> {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

#[derive(Default)]
struct Observer {
    capture: bool,
    overwrite: Option<PathBuf>,
}

impl ParseObserver for Observer {
    fn capture_source_sha256(&self) -> bool { self.capture }
    fn start(&mut self, _: usize) -> Result<()> {
        if let Some(path) = &self.overwrite {
            std::fs::write(path, b"changed after the parser read").unwrap();
        }
        Ok(())
    }
}

fn run(path: &Path, observer: &mut Observer) -> babeldoc_parser_core::Result<babeldoc_parser_core::Parsed> {
    parse_observed(path, Config::default(), observer)
}

#[test]
fn opt_in_hashes_complete_input_and_default_is_off() {
    let input = bytes(document());
    let path = path();
    std::fs::write(&path, &input).unwrap();
    assert_eq!(run(&path, &mut Observer::default()).unwrap().source_sha256, None);
    let actual = run(&path, &mut Observer { capture: true, ..Default::default() }).unwrap().source_sha256;
    assert_eq!(actual, Some(Sha256::digest(&input).into()));
}

#[test]
fn prefix_is_included_and_distinguishes_inputs() {
    let input = bytes(document());
    let path = path();
    let mut first = b"public prefix one\n".to_vec();
    first.extend_from_slice(&input);
    let mut second = b"public prefix two\n".to_vec();
    second.extend_from_slice(&input);
    let mut observer = Observer { capture: true, ..Default::default() };
    std::fs::write(&path, &first).unwrap();
    let first_hash = run(&path, &mut observer).unwrap().source_sha256;
    std::fs::write(&path, &second).unwrap();
    let second_hash = run(&path, &mut observer).unwrap().source_sha256;
    assert_eq!(first_hash, Some(Sha256::digest(&first).into()));
    assert_eq!(second_hash, Some(Sha256::digest(&second).into()));
    assert_ne!(first_hash, second_hash);
}

#[test]
fn modification_after_read_does_not_change_identity() {
    let input = bytes(document());
    let path = path();
    std::fs::write(&path, &input).unwrap();
    let mut observer = Observer { capture: true, overwrite: Some(path.clone()) };
    let parsed = run(&path, &mut observer).unwrap();
    assert_eq!(parsed.source_sha256, Some(Sha256::digest(&input).into()));
    assert_ne!(Sha256::digest(std::fs::read(path).unwrap()).as_slice(), parsed.source_sha256.unwrap());
}

#[test]
fn missing_and_invalid_paths_return_errors() {
    let missing = path();
    let mut observer = Observer { capture: true, ..Default::default() };
    assert!(run(&missing, &mut observer).is_err());
    std::fs::write(&missing, b"not a PDF").unwrap();
    assert!(run(&missing, &mut observer).is_err());
}

#[test]
fn encrypted_load_has_no_identity_when_loader_is_skipped() {
    let mut doc = document();
    let state = EncryptionState::try_from(EncryptionVersion::V1 {
        document: &doc, owner_password: "owner", user_password: "", permissions: Permissions::all(),
    }).unwrap();
    doc.encrypt(&state).unwrap();
    let path = path();
    std::fs::write(&path, bytes(doc)).unwrap();
    let mut observer = Observer { capture: true, ..Default::default() };
    assert_eq!(run(&path, &mut observer).unwrap().source_sha256, None);
}
