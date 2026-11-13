use _rust_il::font_mapper::{
    FontMapperHandle, font_mapper_handle, font_mapper_handle_with_source_styles,
};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use serde_json::json;
use std::sync::Arc;

#[test]
fn verified_styles_match_python_and_unverified_handles_are_unavailable() {
    let cache_root = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .expect("a home directory is required for cached production fonts");
    let production_font = std::env::var_os("BABELDOC_TEST_PRODUCTION_FONT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(cache_root)
                .join(".cache/babeldoc/fonts/GoNotoKurrent-Regular.ttf")
        });
    assert!(
        production_font.is_file(),
        "a cached production font is required for this test"
    );
    let candidates = [
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let path = candidates
        .iter()
        .find(|path| std::path::Path::new(path).is_file())
        .expect("a real system font is required for this test");
    let bytes = std::fs::read(&production_font).unwrap();
    Python::initialize();
    Python::attach(|py| {
        let hashlib = py.import("hashlib").unwrap();
        let digest: String = hashlib
            .call_method1("sha3_256", (PyBytes::new(py, &bytes),))
            .unwrap()
            .call_method0("hexdigest")
            .unwrap()
            .extract()
            .unwrap();
        let family = json!({"normal": ["regular", "alias"], "script": [], "fallback": ["regular"], "base": ["regular"]}).to_string();
        let paths = json!({"regular": production_font, "alias": production_font}).to_string();
        let hashes = json!({"regular": digest, "alias": digest}).to_string();
        let (handle, dictionary) =
            font_mapper_handle_with_source_styles(py, &family, &paths, &hashes, None).unwrap();
        let source_styles = handle
            .source_styles()
            .expect("verified styles must be available");
        let dictionary = dictionary.bind(py);
        assert_eq!(source_styles.len(), dictionary.len());
        assert_eq!(source_styles.len(), 2);
        let regular = handle.mapper.font_by_id("regular").unwrap();
        let base = handle.mapper.font_by_id("base").unwrap();
        assert!(Arc::ptr_eq(&regular, &base));
        let mapped_id = handle.mapped_font_id(false, false, false, false, 'A').unwrap();
        let mapped_face = handle.mapper.font_by_id(&mapped_id).unwrap();
        assert_ne!(mapped_face.has_glyph('A' as u32), 0);
        assert!(handle.mapper.font_by_id("missing").is_none());
        assert!(!handle.has_char(py, "\u{10ffff}"));
        assert!(handle.mapped_font_id(false, false, false, false, '\u{10ffff}').is_none());
        for (id, style) in source_styles {
            let item = dictionary
                .get_item(id)
                .unwrap()
                .unwrap()
                .cast_into::<PyDict>()
                .unwrap();
            for (name, expected) in [
                ("bold", style.bold),
                ("italic", style.italic),
                ("monospaced", style.monospaced),
                ("serif", style.serif),
            ] {
                assert_eq!(
                    item.get_item(name)
                        .unwrap()
                        .unwrap()
                        .extract::<bool>()
                        .unwrap(),
                    expected
                );
            }
        }
        let system_paths = json!({"regular": path, "alias": path}).to_string();
        let legacy = font_mapper_handle(&family, &system_paths, None).unwrap();
        assert!(legacy.source_styles().is_none());
        let shared = FontMapperHandle::from_shared(Arc::clone(&handle.mapper));
        assert!(shared.source_styles().is_none());
        let bad_hashes = json!({"regular": "00", "alias": "00"}).to_string();
        assert!(
            font_mapper_handle_with_source_styles(py, &family, &paths, &bad_hashes, None).is_err()
        );
        let missing_alias_hash = json!({"regular": digest}).to_string();
        assert!(font_mapper_handle_with_source_styles(py, &family, &paths, &missing_alias_hash, None).is_err());
        let bad_alias_hash = json!({"regular": digest, "alias": "00"}).to_string();
        assert!(font_mapper_handle_with_source_styles(py, &family, &paths, &bad_alias_hash, None).is_err());
    });
}
