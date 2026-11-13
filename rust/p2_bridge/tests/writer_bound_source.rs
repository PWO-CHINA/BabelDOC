use _rust_il::writer::{FileLayer, load_writer_source, load_writer_source_with_bytes, open_writer_original, open_writer_translated};
use babeldoc_writer_core::{WriteAttempt, write_with_retry};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

fn synthetic_pdf(page_count: usize) -> Vec<u8> {
    let kids = (0..page_count).map(|index| format!("{} 0 R", index + 3)).collect::<Vec<_>>().join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"),
    ];
    for _ in 0..page_count {
        objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_owned());
    }
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    bytes
}

fn path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("WRITER_BOUND_EVIDENCE_DIR").expect("evidence directory required"))
        .join(format!("{name}.pdf"))
}

#[test]
fn verified_document_survives_path_replacement_and_unbound_remains_compatible() {
    let source = path("replacement");
    let original = synthetic_pdf(1);
    let replacement = synthetic_pdf(2);
    std::fs::write(&source, &original).unwrap();
    let digest: [u8; 32] = Sha256::digest(&original).into();
    let (loaded, bytes) = load_writer_source_with_bytes(&source, Some(&digest)).unwrap();
    std::fs::write(&source, &replacement).unwrap();
    assert_eq!(loaded.get_pages().len(), 1);
    assert_eq!(bytes.as_deref(), Some(original.as_slice()));
    assert!(load_writer_source(&source, Some(&digest)).unwrap_err().contains("mismatch"));
    assert_eq!(load_writer_source(&source, None).unwrap().get_pages().len(), 2);
    let missing = path("missing");
    assert!(!missing.exists());
    assert!(load_writer_source(&missing, Some(&digest)).is_err());
}

struct CaptureFile {
    calls: RefCell<Vec<(String, usize)>>,
    fail: bool,
}

impl FileLayer for CaptureFile {
    fn call<A>(&self, _: &str, method: &str, args: A) -> PyResult<Py<PyAny>>
    where A: for<'py> IntoPyObject<'py, Target = PyTuple> {
        Python::attach(|py| {
            let tuple = _rust_il::callbacks::into_py_tuple(py, args)?;
            let arg = tuple.bind(py).get_item(0)?;
            let size = if method.ends_with("_bytes") {
                let bytes = arg.cast::<PyBytes>()?;
                babeldoc_writer_core::PdfDocument::load_mem(bytes.as_bytes())
                    .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?
                    .get_pages().len()
            } else {
                let path: String = arg.extract()?;
                babeldoc_writer_core::PdfDocument::load(path)
                    .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?
                    .get_pages().len()
            };
            self.calls.borrow_mut().push((method.to_owned(), size));
            if self.fail { return Err(pyo3::exceptions::PyRuntimeError::new_err("callback failure")); }
            Ok(py.None())
        })
    }
}

#[test]
fn original_callback_receives_verified_bytes_after_replacement() {
    Python::initialize();
    let source = path("callback");
    let original = synthetic_pdf(1);
    std::fs::write(&source, &original).unwrap();
    let digest: [u8; 32] = Sha256::digest(&original).into();
    let (_, bytes) = load_writer_source_with_bytes(&source, Some(&digest)).unwrap();
    std::fs::write(&source, synthetic_pdf(2)).unwrap();
    let file = CaptureFile { calls: RefCell::new(Vec::new()), fail: false };
    let source_path = source.to_str().unwrap();
    open_writer_original(&file, source_path, bytes.as_deref()).unwrap();
    open_writer_original(&file, source_path, bytes.as_deref()).unwrap();
    open_writer_original(&file, source_path, None).unwrap();
    assert_eq!(*file.calls.borrow(), vec![("open_original_bytes".into(), 1), ("open_original_bytes".into(), 1), ("open_original".into(), 2)]);
    let failing = CaptureFile { calls: RefCell::new(Vec::new()), fail: true };
    assert!(open_writer_original(&failing, source_path, bytes.as_deref()).is_err());
    assert!(open_writer_original(&file, source_path, Some(b"not a PDF")).is_err());
}

#[test]
fn translated_callback_receives_generated_bytes_after_replacement() {
    Python::initialize();
    let translated = path("translated_callback");
    let generated = synthetic_pdf(1);
    std::fs::write(&translated, synthetic_pdf(2)).unwrap();
    let file = CaptureFile { calls: RefCell::new(Vec::new()), fail: false };
    let translated_path = translated.to_str().unwrap();
    open_writer_translated(&file, translated_path, Some(&generated)).unwrap();
    open_writer_translated(&file, translated_path, None).unwrap();
    assert_eq!(*file.calls.borrow(), vec![("open_translated_bytes".into(), 1), ("open_translated".into(), 2)]);
    let failing = CaptureFile { calls: RefCell::new(Vec::new()), fail: true };
    assert!(open_writer_translated(&failing, translated_path, Some(&generated)).is_err());
    assert!(open_writer_translated(&file, translated_path, Some(b"not a PDF")).is_err());
}

struct RetrySource<'a> {
    source: &'a Path,
    digest: &'a [u8; 32],
    replacement: Vec<u8>,
    attempts: usize,
    closes: usize,
}

impl WriteAttempt for RetrySource<'_> {
    type Value = ();
    type Error = String;

    fn attempt(&mut self, _checked: bool) -> Result<(), String> {
        self.attempts += 1;
        load_writer_source(self.source, Some(self.digest))?;
        Err("simulated writer failure".into())
    }
    fn close(&mut self) -> Result<(), String> {
        self.closes += 1;
        if self.closes == 1 {
            std::fs::write(self.source, &self.replacement).unwrap();
        }
        Ok(())
    }
    fn ordinary(&self, _: &String) -> bool { true }
    fn failed(&mut self, _: &String) {}
    fn recovered(&mut self) {}
}

#[test]
fn retry_rechecks_the_source_after_a_failed_attempt() {
    let source = path("retry");
    let original = synthetic_pdf(1);
    std::fs::write(&source, &original).unwrap();
    let digest: [u8; 32] = Sha256::digest(&original).into();
    let mut retry = RetrySource { source: &source, digest: &digest, replacement: synthetic_pdf(2), attempts: 0, closes: 0 };
    let error = write_with_retry(&mut retry).unwrap_err();
    assert!(error.contains("mismatch"));
    assert_eq!((retry.attempts, retry.closes), (2, 2));
}
