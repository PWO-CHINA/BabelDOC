use std::collections::HashMap;

use _rust_il::llm_only_translation::boundary_typesetting_matches;
use babeldoc_p2_stages::font_mapper::SourceFontStyle;
use serde_json::{Value, json};

fn style() -> SourceFontStyle {
    SourceFontStyle { bold: false, italic: true, monospaced: false, serif: true }
}

fn config() -> Value {
    json!({
        "config_fields": {"lang_out": "zh"},
        "progress_monitor_present": true,
        "debug_information_before_typesetting": false,
        "font_source_styles": {"font": {"bold": false, "italic": true, "monospaced": false, "serif": true}}
    })
}

#[test]
fn production_parity_requires_exact_language_styles_progress_and_debug_state() {
    pyo3::Python::initialize();
    let styles = HashMap::from([("font".to_owned(), style())]);
    assert!(boundary_typesetting_matches("zh", Some(&styles), &config()).unwrap());
    assert!(!boundary_typesetting_matches("en", Some(&styles), &config()).unwrap());
    assert!(!boundary_typesetting_matches("zh", None, &config()).unwrap());
    let mut different = styles.clone();
    different.get_mut("font").unwrap().bold = true;
    assert!(!boundary_typesetting_matches("zh", Some(&different), &config()).unwrap());
    let mut changed = config();
    changed["progress_monitor_present"] = false.into();
    assert!(!boundary_typesetting_matches("zh", Some(&styles), &changed).unwrap());
    changed = config();
    changed["debug_information_before_typesetting"] = true.into();
    assert!(!boundary_typesetting_matches("zh", Some(&styles), &changed).unwrap());
    changed.as_object_mut().unwrap().remove("debug_information_before_typesetting");
    assert!(!boundary_typesetting_matches("zh", Some(&styles), &changed).unwrap());
}

#[test]
fn incomplete_production_font_configuration_cannot_match() {
    pyo3::Python::initialize();
    let styles = HashMap::from([("font".to_owned(), style())]);
    let mut changed = config();
    changed["font_source_styles"]["font"].as_object_mut().unwrap().remove("italic");
    assert!(boundary_typesetting_matches("zh", Some(&styles), &changed).is_err());
    changed = config();
    changed["font_source_styles"] = Value::Null;
    assert!(!boundary_typesetting_matches("zh", Some(&styles), &changed).unwrap());
    assert!(!boundary_typesetting_matches("zh", Some(&HashMap::new()), &changed).unwrap());
    changed = config();
    changed.as_object_mut().unwrap().remove("progress_monitor_present");
    assert!(!boundary_typesetting_matches("zh", Some(&styles), &changed).unwrap());
    changed = config();
    changed["config_fields"]["lang_out"] = Value::Null;
    assert!(boundary_typesetting_matches("zh", Some(&styles), &changed).is_err());
}
