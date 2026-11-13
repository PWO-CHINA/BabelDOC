use crate::font_mapper::FontMapperHandle;
use babeldoc_il_types::RuntimeBool;
use babeldoc_ir_core::{self as ir, Document, TranslationView, TreeView};
use babeldoc_p2_stages::{
    font_mapper::SourceFontStyle,
    translation::cross_boundary::{Location, OutputPart, ResolvedParagraph, Resource},
};
use pyo3::{exceptions::PyRuntimeError, prelude::*};
use std::collections::{HashMap, HashSet};

#[path = "boundary_graphics.rs"]
pub(crate) mod boundary_graphics;

type View<'a> = TreeView<'a, TranslationView>;
type Paragraph = ir::Cursor<ir::types::PdfParagraph>;
type Composition = ir::Cursor<ir::types::PdfParagraphComposition>;
type Style = ir::Cursor<ir::types::PdfStyle>;

pyo3::create_exception!(babeldoc, BoundaryApplyRejected, PyRuntimeError);

fn error(location: Location, message: &str) -> PyErr {
    BoundaryApplyRejected::new_err(format!(
        "boundary apply at page {}, paragraph {}: {message}",
        location.page, location.paragraph
    ))
}

fn rejected(location: Location, message: &str) -> PyErr {
    BoundaryApplyRejected::new_err(format!(
        "boundary apply at page {}, paragraph {}: {message}",
        location.page, location.paragraph
    ))
}

fn paragraph(view: &View<'_>, location: Location) -> PyResult<Paragraph> {
    view.document()
        .page_at(view, location.page)
        .and_then(|page| page.pdf_paragraph_at(view, location.paragraph))
        .map_err(|_| error(location, "paragraph is unavailable"))
}

fn composition(view: &View<'_>, resource: &Resource) -> PyResult<Composition> {
    paragraph(view, resource.location)?
        .pdf_paragraph_composition_at(view, resource.composition)
        .map_err(|_| error(resource.location, "composition is unavailable"))
}

fn flag(value: &Option<RuntimeBool>) -> bool {
    match value {
        Some(RuntimeBool::Bool(value)) => *value,
        Some(RuntimeBool::Int(value)) => *value != 0,
        None => false,
    }
}

fn font_style(
    font: ir::Cursor<ir::types::PdfFont>,
    view: &View<'_>,
    location: Location,
) -> PyResult<SourceFontStyle> {
    let invalid = |_| error(location, "source font metadata is unavailable");
    Ok(SourceFontStyle {
        bold: flag(font.bold(view).map_err(invalid)?),
        italic: flag(font.italic(view).map_err(invalid)?),
        monospaced: flag(font.monospace(view).map_err(invalid)?),
        serif: flag(font.serif(view).map_err(invalid)?),
    })
}

fn find_font(
    fonts: impl Iterator<Item = Result<ir::Cursor<ir::types::PdfFont>, ir::AccessError>>,
    view: &View<'_>,
    location: Location,
    font_id: &str,
) -> PyResult<Option<SourceFontStyle>> {
    let mut exact = None;
    let mut alias = None;
    let mut ambiguous_alias = false;
    for font in fonts {
        let font = font.map_err(|_| error(location, "source font is unavailable"))?;
        let Some(id) = font
            .font_id(view)
            .map_err(|_| error(location, "source font id is unavailable"))?
        else {
            continue;
        };
        if id == font_id {
            let style = font_style(font, view, location)?;
            if exact.is_some_and(|existing| existing != style) {
                return Err(rejected(location, "ambiguous source font id"));
            }
            exact = Some(style);
        } else if id.trim_start_matches('/') == font_id.trim_start_matches('/') {
            let style = font_style(font, view, location)?;
            ambiguous_alias |= alias.is_some_and(|existing| existing != style);
            alias = Some(style);
        }
    }
    if exact.is_some() {
        return Ok(exact);
    }
    if ambiguous_alias {
        return Err(rejected(location, "ambiguous source font alias"));
    }
    Ok(alias)
}

fn source_font_style(
    view: &View<'_>,
    location: Location,
    source: Paragraph,
    style: Style,
    mapper: &FontMapperHandle,
) -> PyResult<SourceFontStyle> {
    let font_id = style
        .font_id(view)
        .map_err(|_| error(location, "source style is unavailable"))?
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| rejected(location, "source style has no font id"))?;
    let page = view
        .document()
        .page_at(view, location.page)
        .map_err(|_| error(location, "source page is unavailable"))?;
    let scope = source
        .xobj_id(view)
        .map_err(|_| error(location, "source scope is unavailable"))?;
    if let Some(scope) = scope {
        let mut scoped_object = None;
        for object in page
            .pdf_xobject_iter(view)
            .map_err(|_| error(location, "source scopes are unavailable"))?
        {
            let object = object.map_err(|_| error(location, "source scope is unavailable"))?;
            if object
                .xobj_id(view)
                .map_err(|_| error(location, "source scope is unavailable"))?
                == Some(scope)
            {
                if scoped_object.replace(object).is_some() {
                    return Err(rejected(location, "source scope is ambiguous"));
                }
            }
        }
        if let Some(object) = scoped_object {
            let fonts = object
                .pdf_font_iter(view)
                .map_err(|_| error(location, "source fonts are unavailable"))?;
            if let Some(found) = find_font(fonts, view, location, font_id)? {
                return Ok(found);
            }
        }
    }
    let fonts = page
        .pdf_font_iter(view)
        .map_err(|_| error(location, "source fonts are unavailable"))?;
    if let Some(found) = find_font(fonts, view, location, font_id)? {
        return Ok(found);
    }
    let face = mapper
        .mapper
        .font_by_id(font_id)
        .or_else(|| mapper.mapper.font_by_id(font_id.trim_start_matches('/')))
        .ok_or_else(|| rejected(location, "source font cannot be resolved"))?;
    if let Some(found) = mapper
        .source_styles()
        .and_then(|styles| styles.get(font_id).or_else(|| styles.get(face.id())))
    {
        return Ok(*found);
    }
    let traits = face.traits();
    let flags = face
        .pdf_font_flags()
        .map_err(|_| error(location, "source font traits are unavailable"))?;
    Ok(SourceFontStyle {
        bold: traits.bold,
        italic: traits.italic,
        monospaced: traits.monospaced,
        serif: flags.serif,
    })
}

fn text_value(
    text: String,
    style: Option<ir::FieldValue<ir::types::PdfStyle>>,
) -> ir::FieldValue<ir::types::PdfParagraphComposition> {
    ir::PdfParagraphCompositionInit {
        pdf_same_style_unicode_characters: Some(
            ir::PdfSameStyleUnicodeCharactersInit {
                pdf_style: style,
                unicode: Some(text),
                debug_info: None,
            }
            .into_value(),
        ),
        ..Default::default()
    }
    .into_value()
}

fn resource_scope(
    view: &View<'_>,
    location: Location,
    scope: Option<i64>,
) -> PyResult<Option<boundary_graphics::Scope>> {
    let page = view.document().page_at(view, location.page)
        .map_err(|_| error(location, "page scope is unavailable"))?;
    let Some(page_number) = page.page_number(view)
        .map_err(|_| error(location, "page number is unavailable"))? else { return Ok(None) };
    let mut found = None;
    for object in page.pdf_xobject_iter(view)
        .map_err(|_| error(location, "page scopes are unavailable"))? {
        let object = object.map_err(|_| error(location, "page scope is unavailable"))?;
        if object.xobj_id(view).map_err(|_| error(location, "page scope is unavailable"))? == scope {
            let Some(xref) = object.xref_id(view)
                .map_err(|_| error(location, "form reference is unavailable"))? else { return Ok(None) };
            found = Some(xref);
        }
    }
    if found.is_none() && !matches!(scope, None | Some(0)) {
        return Ok(None);
    }
    Ok(Some(boundary_graphics::Scope { page_number, form_xref: found }))
}

fn transferable_graphics(
    view: &View<'_>,
    style: Style,
    source: Location,
    source_scope: Option<i64>,
    target: Location,
    target_scope: Option<i64>,
    evidence: Option<&boundary_graphics::Evidence>,
) -> PyResult<bool> {
    let Some(graphics) = style
        .graphic_state(view)
        .map_err(|_| error(source, "source graphic state unavailable"))?
    else {
        return Ok(true);
    };
    let instructions = graphics
        .passthrough_per_char_instruction(view)
        .map_err(|_| error(source, "source graphic state unavailable"))?;
    let instructions = view
        .render_instructions(instructions)
        .map_err(|_| error(source, "source graphic state cannot be resolved"))?
        .unwrap_or_default();
    let normalized = instructions.replace(['[', ']'], " ");
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.iter().enumerate().any(|(index, token)| {
        *token == "Tr" && (index == 0 || !matches!(tokens[index - 1], "0" | "1" | "2" | "3"))
    }) {
        return Ok(false);
    }
    let self_contained = tokens.iter().all(|token| {
        matches!(
            *token,
            "g" | "G"
                | "rg"
                | "RG"
                | "k"
                | "K"
                | "w"
                | "J"
                | "j"
                | "M"
                | "d"
                | "Tr"
                | "Tc"
                | "Tw"
                | "Tz"
                | "TL"
                | "Ts"
        ) || token.parse::<f64>().is_ok_and(f64::is_finite)
    });
    if self_contained && evidence.is_none() {
        return Ok(true);
    }
    let Some(evidence) = evidence else { return Ok(false) };
    let Some(source) = resource_scope(view, source, source_scope)? else { return Ok(false) };
    let Some(target) = resource_scope(view, target, target_scope)? else { return Ok(false) };
    Ok(evidence.transferable(&instructions, source, target))
}

struct PreparedParagraph {
    location: Location,
    paragraph: Paragraph,
    unicode: String,
    compositions: Vec<ir::FieldValue<ir::types::PdfParagraphComposition>>,
}

fn apply_to_snapshot(
    document: &mut Document,
    outputs: &[ResolvedParagraph],
    mapper: &FontMapperHandle,
    evidence: Option<&boundary_graphics::Evidence>,
) -> PyResult<()> {
    let mut view = document.view::<TranslationView>();
    let mut locations = HashSet::new();
    let mut target_ids = HashSet::new();
    let mut targets = Vec::with_capacity(outputs.len());
    let mut original_formulas = HashMap::<u64, usize>::new();

    for output in outputs {
        if !locations.insert(output.location) {
            return Err(rejected(output.location, "duplicate target paragraph"));
        }
        let target = paragraph(&view, output.location)?;
        if !target_ids.insert(target.identity()) {
            return Err(rejected(
                output.location,
                "target paragraphs share an identity",
            ));
        }
        for item in target
            .pdf_paragraph_composition_iter(&view)
            .map_err(|_| error(output.location, "target compositions are unavailable"))?
        {
            let item =
                item.map_err(|_| error(output.location, "target composition is unavailable"))?;
            if let Some(formula) = item
                .pdf_formula(&view)
                .map_err(|_| error(output.location, "target formula is unavailable"))?
            {
                *original_formulas.entry(formula.identity()).or_default() += 1;
            }
        }
        targets.push(
            target
                .retain(&mut view)
                .map_err(|_| error(output.location, "target cannot be retained"))?,
        );
    }

    let page_count = view
        .document()
        .page_len(&view)
        .map_err(|_| PyRuntimeError::new_err("boundary apply: page list is unavailable"))?;
    for page_index in 0..page_count {
        let page = view
            .document()
            .page_at(&view, page_index)
            .map_err(|_| PyRuntimeError::new_err("boundary apply: page is unavailable"))?;
        let count = page.pdf_paragraph_len(&view).map_err(|_| {
            PyRuntimeError::new_err("boundary apply: paragraph list is unavailable")
        })?;
        for paragraph_index in 0..count {
            let location = Location {
                page: page_index,
                paragraph: paragraph_index,
            };
            let candidate = paragraph(&view, location)?;
            if target_ids.contains(&candidate.identity()) && !locations.contains(&location) {
                return Err(rejected(
                    location,
                    "target paragraph is also referenced elsewhere",
                ));
            }
        }
    }

    let mut emitted_formulas = HashMap::<u64, usize>::new();
    let mut prepared = Vec::with_capacity(outputs.len());
    for (output, target) in outputs.iter().zip(targets) {
        let location = output.location;
        let target_scope = target
            .xobj_id(&view)
            .map_err(|_| error(location, "target scope is unavailable"))?;
        let mut unicode = String::new();
        let mut compositions = Vec::new();
        for part in &output.parts {
            match part {
                OutputPart::Text { text, style: None } => {
                    if text.is_empty() {
                        continue;
                    }
                    let style = target
                        .pdf_style(&view)
                        .map_err(|_| error(location, "target style is unavailable"))?
                        .ok_or_else(|| rejected(location, "target style is missing"))?;
                    let style = style
                        .share(&mut view)
                        .map_err(|_| error(location, "target style cannot be retained"))?;
                    unicode.push_str(text);
                    compositions.push(text_value(text.clone(), Some(style)));
                }
                OutputPart::Text {
                    text,
                    style: Some(resource),
                } => {
                    let source = paragraph(&view, resource.location)?;
                    let rich = composition(&view, resource)?
                        .pdf_same_style_characters(&view)
                        .map_err(|_| error(resource.location, "source rich style is unavailable"))?
                        .ok_or_else(|| {
                            rejected(resource.location, "source composition is not rich text")
                        })?;
                    let style = rich
                        .pdf_style(&view)
                        .map_err(|_| error(resource.location, "source style is unavailable"))?
                        .ok_or_else(|| rejected(resource.location, "source style is missing"))?;
                    if text.is_empty() {
                        continue;
                    }
                    let source_scope = source
                        .xobj_id(&view)
                        .map_err(|_| error(resource.location, "source scope is unavailable"))?;
                    if (resource.location.page != location.page || source_scope != target_scope)
                        && !transferable_graphics(
                            &view, style, resource.location, source_scope,
                            location, target_scope, evidence,
                        )?
                    {
                        return Err(rejected(
                            location,
                            "rich text uses page-scoped graphics that cannot move to the destination",
                        ));
                    }
                    let traits =
                        source_font_style(&view, resource.location, source, style, mapper)?;
                    let style = style
                        .retain(&mut view)
                        .map_err(|_| error(resource.location, "source style cannot be retained"))?;
                    let mut runs: Vec<(String, String)> = Vec::new();
                    for character in text.chars() {
                        let font_id = if character == '\n' {
                            runs.last()
                                .map(|(id, _)| id.clone())
                                .unwrap_or_else(|| mapper.mapper.base_font().id().to_owned())
                        } else {
                            mapper
                                .mapper
                                .map(traits, character)
                                .ok_or_else(|| {
                                    rejected(location, "mapped font has no required glyph")
                                })?
                                .id()
                                .to_owned()
                        };
                        if let Some((previous_id, previous_text)) = runs.last_mut() {
                            if *previous_id == font_id {
                                previous_text.push(character);
                                continue;
                            }
                        }
                        runs.push((font_id, character.to_string()));
                    }
                    unicode.push_str(text);
                    for (font_id, value) in runs {
                        let mut mapped = style.copy_shallow(&mut view).map_err(|_| {
                            error(resource.location, "source style cannot be retained")
                        })?;
                        mapped.font_id = Some(font_id);
                        compositions.push(text_value(value, Some(mapped.into_value())));
                    }
                }
                OutputPart::Formula(resource) => {
                    let source = paragraph(&view, resource.location)?;
                    let source_scope = source
                        .xobj_id(&view)
                        .map_err(|_| error(resource.location, "source scope is unavailable"))?;
                    if resource.location.page != location.page || source_scope != target_scope {
                        return Err(rejected(
                            location,
                            "formula cannot move across page or XObject scope",
                        ));
                    }
                    let formula = composition(&view, resource)?
                        .pdf_formula(&view)
                        .map_err(|_| error(resource.location, "source formula is unavailable"))?
                        .ok_or_else(|| {
                            rejected(resource.location, "source composition is not a formula")
                        })?;
                    *emitted_formulas.entry(formula.identity()).or_default() += 1;
                    for character in formula.pdf_character_iter(&view).map_err(|_| {
                        error(resource.location, "formula characters are unavailable")
                    })? {
                        let character = character.map_err(|_| {
                            error(resource.location, "formula character is unavailable")
                        })?;
                        if let Some(value) = character.char_unicode(&view).map_err(|_| {
                            error(resource.location, "formula character is unavailable")
                        })? {
                            unicode.push_str(value);
                        }
                    }
                    let formula = formula
                        .retain(&mut view)
                        .and_then(|formula| formula.share(&mut view))
                        .map_err(|_| error(resource.location, "formula cannot be retained"))?;
                    compositions.push(
                        ir::PdfParagraphCompositionInit {
                            pdf_formula: Some(formula),
                            ..Default::default()
                        }
                        .into_value(),
                    );
                }
            }
        }
        prepared.push(PreparedParagraph {
            location,
            paragraph: target,
            unicode,
            compositions,
        });
    }
    if original_formulas != emitted_formulas {
        return Err(BoundaryApplyRejected::new_err(
            "boundary apply: formula identities or counts changed",
        ));
    }
    for item in prepared {
        item.paragraph
            .set_unicode(&mut view, Some(item.unicode))
            .map_err(|_| error(item.location, "target unicode could not be written"))?;
        item.paragraph
            .set_pdf_paragraph_composition(&mut view, item.compositions)
            .map_err(|_| error(item.location, "target compositions could not be written"))?;
    }
    Ok(())
}

pub(crate) fn apply_with_evidence(
    document: &mut Document,
    outputs: &[ResolvedParagraph],
    mapper: &FontMapperHandle,
    evidence: Option<&boundary_graphics::Evidence>,
) -> PyResult<()> {
    if outputs.is_empty() {
        return Ok(());
    }
    let mut snapshot = document.fork();
    apply_to_snapshot(&mut snapshot, outputs, mapper, evidence)?;
    *document = snapshot;
    Ok(())
}
