use super::ordinary_translation::{InputPlaceholder, TranslateInput};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompositionLocation {
    pub page: usize,
    pub paragraph: usize,
    pub composition: usize,
}

#[derive(Debug, PartialEq)]
struct StyleEvidence {
    font_xref: i64,
    font_size: f64,
}

fn optional_integer(value: Option<&Value>) -> Option<Option<i64>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => value.as_i64().map(Some),
    }
}

fn empty_graphic_state(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Object(state)) => state.len() == 1
            && state.get("passthrough_per_char_instruction")
                .is_some_and(|value| value.is_null() || value.as_str() == Some("")),
        _ => false,
    }
}

fn resolve_font_xref(page: &Value, scope: Option<i64>, font_id: &str) -> Option<i64> {
    let objects = page.get("pdf_xobject")?.as_array()?;
    for object in objects {
        if optional_integer(object.get("xobj_id"))? == scope {
            return None;
        }
    }
    let mut found = None;
    for font in page.get("pdf_font")?.as_array()? {
        if font.get("font_id").and_then(Value::as_str) == Some(font_id) {
            if found.is_some() {
                return None;
            }
            found = Some(font.get("xref_id")?.as_i64().filter(|id| *id > 0)?);
        }
    }
    found
}

fn style_evidence(page: &Value, scope: Option<i64>, style: &Value) -> Option<StyleEvidence> {
    let fields = style.as_object()?;
    if fields.keys().any(|key| !matches!(key.as_str(), "font_id" | "font_size" | "graphic_state"))
        || !fields.contains_key("graphic_state") || !empty_graphic_state(style.get("graphic_state")) {
        return None;
    }
    if !matches!(scope, None | Some(0)) {
        return None;
    }
    let font_id = style.get("font_id")?.as_str().filter(|id| !id.is_empty())?;
    let font_size = style.get("font_size")?.as_f64().filter(|size| size.is_finite() && *size > 0.0)?;
    Some(StyleEvidence { font_xref: resolve_font_xref(page, scope, font_id)?, font_size })
}

fn composition_evidence(document: &Value, location: CompositionLocation) -> Option<(StyleEvidence, String)> {
    let page = document.get("document")?.get("page")?.get(location.page)?;
    let paragraph = page.get("pdf_paragraph")?.get(location.paragraph)?;
    if !paragraph.as_object()?.contains_key("xobj_id") {
        return None;
    }
    let scope = optional_integer(paragraph.get("xobj_id"))?;
    let composition = paragraph.get("pdf_paragraph_composition")?.get(location.composition)?.as_object()?;
    if composition.iter().any(|(name, value)| name != "pdf_same_style_characters" && !value.is_null()) {
        return None;
    }
    let group = composition.get("pdf_same_style_characters")?;
    let evidence = style_evidence(page, scope, group.get("pdf_style")?)?;
    let characters = group.get("pdf_character")?.as_array().filter(|characters| !characters.is_empty())?;
    let mut text = String::new();
    for character in characters {
        if !character.as_object()?.contains_key("xobj_id")
            || optional_integer(character.get("xobj_id"))? != scope
            || !matches!(character.get("vertical"), Some(Value::Null) | Some(Value::Bool(false)))
            || character.get("char_unicode").and_then(Value::as_str).is_none_or(str::is_empty)
            || ["scale", "visual_roundtrip_text_render_mode", "visual_roundtrip_text_scale_x", "visual_roundtrip_text_scale_y"]
                .iter().any(|name| !character.get(*name).is_some_and(Value::is_null))
            || style_evidence(page, scope, character.get("pdf_style")?)? != evidence {
            return None;
        }
        text.push_str(character.get("char_unicode")?.as_str()?);
    }
    Some((evidence, text))
}

fn boundary_styles_equivalent(document: &Value, left: CompositionLocation, right: CompositionLocation) -> bool {
    if left.page == right.page {
        return false;
    }
    match (composition_evidence(document, left), composition_evidence(document, right)) {
        (Some((left, _)), Some((right, _))) => left == right,
        _ => false,
    }
}

pub fn composition_styles_equivalent(document: &Value, left: CompositionLocation, right: CompositionLocation) -> bool {
    boundary_styles_equivalent(document, left, right)
}

pub fn whole_sentence_boundary_styles_equivalent(
    document: &Value,
    left_page: usize,
    left_paragraph: usize,
    left: &TranslateInput,
    right_page: usize,
    right_paragraph: usize,
    right: &TranslateInput,
) -> bool {
    let (Some(InputPlaceholder::Rich(left_rich)), Some(InputPlaceholder::Rich(right_rich))) =
        (left.placeholders.last(), right.placeholders.first()) else { return false };
    if left_page == right_page || left_rich.source_text.is_empty() || right_rich.source_text.is_empty() {
        return false;
    }
    let left_location = CompositionLocation { page: left_page, paragraph: left_paragraph, composition: left_rich.composition_index };
    let right_location = CompositionLocation { page: right_page, paragraph: right_paragraph, composition: right_rich.composition_index };
    match (composition_evidence(document, left_location), composition_evidence(document, right_location)) {
        (Some((left_style, left_text)), Some((right_style, right_text))) =>
            left_style == right_style && left_text == left_rich.source_text && right_text == right_rich.source_text,
        _ => false,
    }
}
