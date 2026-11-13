//! Private-value evidence for formulas in a document and its uncompacted fork.
//!
//! Equality is meaningful only for the same document lineage without compaction.
//! This records IR values, not the actual PDF writer resources or output. It must
//! not be used to relax the existing safety gates. Never log the snapshot.

use babeldoc_ir_core::{AccessError, Document};
use serde_json::Value;

#[derive(Clone, PartialEq, Eq)]
pub struct FormulaSourceEvidence {
    paragraph_scope: Value,
    formulas: Vec<FormulaEvidence>,
}

#[derive(Clone, PartialEq, Eq)]
struct FormulaEvidence {
    identity: u64,
    characters: Vec<u64>,
    curves: Vec<u64>,
    forms: Vec<u64>,
    value: Value,
}

/// Capture before mutation and compare with a capture from the same fork lineage.
/// Neither document may be compacted between captures. Equality does not prove
/// that the writer has the fonts, forms, graphics state, or original PDF resources.
pub fn capture_formula_source_evidence(
    document: &Document,
    page_index: usize,
    paragraph_index: usize,
) -> Result<FormulaSourceEvidence, AccessError> {
    capture_formula_source_evidence_many(document, &[(page_index, paragraph_index)])?
        .pop().ok_or(AccessError::WrongType)
}

pub fn capture_formula_source_evidence_many(
    document: &Document,
    targets: &[(usize, usize)],
) -> Result<Vec<FormulaSourceEvidence>, AccessError> {
    if targets.is_empty() { return Ok(Vec::new()); }
    let view = document.read();
    let artifact = document.artifact()?;
    let nodes = artifact.get("graphic_state_nodes").and_then(Value::as_array)
        .ok_or(AccessError::WrongType)?;
    targets.iter().map(|&(page_index, paragraph_index)| {
    let paragraph = view.document().page_at(&view, page_index)?
        .pdf_paragraph_at(&view, paragraph_index)?;
    let paragraph_value = artifact.get("document").and_then(|v| v.get("page"))
        .and_then(|v| v.get(page_index)).and_then(|v| v.get("pdf_paragraph"))
        .and_then(|v| v.get(paragraph_index)).ok_or(AccessError::WrongType)?;
    let compositions = paragraph_value.get("pdf_paragraph_composition")
        .and_then(Value::as_array).ok_or(AccessError::WrongType)?;
    let paragraph_scope = paragraph_value.get("xobj_id").ok_or(AccessError::WrongType)?;
    if !paragraph_scope.is_null() && paragraph_scope.as_i64().is_none() && paragraph_scope.as_u64().is_none() {
        return Err(AccessError::WrongType);
    }
    let mut formulas = Vec::new();
    let mut composition_count = 0;
    for (index, composition) in paragraph.pdf_paragraph_composition_iter(&view)?.enumerate() {
        composition_count += 1;
        let composition = composition?;
        if let Some(formula) = composition.pdf_formula(&view)? {
            if composition.pdf_line(&view)?.is_some()
                || composition.pdf_character(&view)?.is_some()
                || composition.pdf_same_style_characters(&view)?.is_some()
                || composition.pdf_same_style_unicode_characters(&view)?.is_some()
            {
                return Err(AccessError::WrongType);
            }
            let value = compositions.get(index).and_then(|v| v.get("pdf_formula"))
                .filter(|v| v.is_object()).ok_or(AccessError::WrongType)?;
            formulas.push(FormulaEvidence {
                identity: formula.identity(),
                characters: formula.pdf_character_iter(&view)?.map(|v| v.map(|v| v.identity())).collect::<Result<_, _>>()?,
                curves: formula.pdf_curve_iter(&view)?.map(|v| v.map(|v| v.identity())).collect::<Result<_, _>>()?,
                forms: formula.pdf_form_iter(&view)?.map(|v| v.map(|v| v.identity())).collect::<Result<_, _>>()?,
                value: normalize(value, nodes)?,
            });
        }
    }
    if composition_count != compositions.len() { return Err(AccessError::WrongType); }
    Ok(FormulaSourceEvidence {
        paragraph_scope: paragraph_scope.clone(),
        formulas,
    })
    }).collect()
}

fn normalize(value: &Value, nodes: &[Value]) -> Result<Value, AccessError> {
    match value {
        Value::String(value) if matches!(value.as_str(), "nan" | "inf" | "-inf") => Err(AccessError::WrongType),
        Value::Number(value) if value.as_f64().is_some_and(|n| n == 0.0 && n.is_sign_negative()) => Err(AccessError::WrongType),
        Value::Array(values) => values.iter().map(|v| normalize(v, nodes)).collect(),
        Value::Object(object) => {
            if object.get("$py").and_then(Value::as_str) == Some("float") {
                let bits = object.get("bits").and_then(Value::as_str)
                    .filter(|s| s.len() == 16)
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    .ok_or(AccessError::WrongType)?;
                if !f64::from_bits(bits).is_finite() { return Err(AccessError::WrongType); }
            }
            if object.get("kind").and_then(Value::as_str) == Some("view") {
                if object.len() != 5 || !object.contains_key("tail_ref")
                    || !object.get("latest_versions").is_some_and(Value::is_array)
                    || !object.get("include_clipping").is_some_and(Value::is_boolean)
                    || !object.get("suffix_parts").is_some_and(Value::is_array)
                { return Err(AccessError::WrongType); }
            }
            let mut result = serde_json::Map::new();
            for (key, item) in object {
                if key == "pdf_original_path_primitive" && !item.is_null() {
                    let args = item.get("args").and_then(Value::as_array).ok_or(AccessError::WrongType)?;
                    if !args.iter().all(Value::is_number) { return Err(AccessError::WrongType); }
                }
                if key == "tail_ref" && object.get("kind").and_then(Value::as_str) == Some("view") {
                    let mut chain = Vec::new();
                    let mut current = item.clone();
                    while !current.is_null() {
                        let index = current.as_u64().and_then(|v| usize::try_from(v).ok()).ok_or(AccessError::WrongType)?;
                        if chain.len() >= nodes.len() { return Err(AccessError::WrongType); }
                        let node = nodes.get(index).and_then(Value::as_object).ok_or(AccessError::WrongType)?;
                        if node.get("ordinal").and_then(Value::as_u64) != Some(index as u64) { return Err(AccessError::WrongType); }
                        if node.len() != 7 || !node.contains_key("arg") || !node.contains_key("replace_key")
                            || !node.get("op").is_some_and(Value::is_string)
                            || !node.get("version").is_some_and(Value::is_u64)
                            || !node.get("length").is_some_and(Value::is_u64)
                        { return Err(AccessError::WrongType); }
                        current = node.get("parent_ordinal").cloned().ok_or(AccessError::WrongType)?;
                        let mut record = node.clone();
                        record.remove("ordinal");
                        record.remove("parent_ordinal");
                        chain.push(normalize(&Value::Object(record), nodes)?);
                    }
                    result.insert(key.clone(), Value::Array(chain));
                } else {
                    result.insert(key.clone(), normalize(item, nodes)?);
                }
            }
            Ok(Value::Object(result))
        }
        _ => Ok(value.clone()),
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::normalize;
    use serde_json::json;

    #[test]
    fn rejects_incomplete_history_and_null_path_arguments() {
        let incomplete_view = json!({"kind":"view", "latest_versions":[], "include_clipping":true, "suffix_parts":[]});
        assert!(normalize(&incomplete_view, &[]).is_err());
        let view = json!({"kind":"view", "tail_ref":0, "latest_versions":[], "include_clipping":true, "suffix_parts":[]});
        let incomplete_node = json!({"ordinal":0, "parent_ordinal":null, "op":"m", "arg":"", "version":1, "length":1});
        assert!(normalize(&view, &[incomplete_node]).is_err());
        assert!(normalize(&json!({"pdf_original_path_primitive":{"op":"m", "args":[null]}}), &[]).is_err());
        assert!(normalize(&json!({"pdf_original_path_primitive":{"op":"m"}}), &[]).is_err());
    }
}
