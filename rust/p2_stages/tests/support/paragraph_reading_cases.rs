use super::{character_geometry, reading_lines, reconcile_token_layouts};
use super::super::paragraph_finder_inputs::{ParagraphFinderConfig, ParagraphFinderInputs, RandomDebugIdSource};
use babeldoc_il_types::{Box as PdfBox, PageLayout, PdfCharacter, PdfStyle, VisualBbox};
use babeldoc_ir_core as ir;
use babeldoc_p2_stages::{NoopCancellation, NoopProgress, font_mapper::font_mapper_from_asset_paths};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn evidence(path: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(path);
    serde_json::from_slice(&std::fs::read(path).expect("stored source evidence is required")).unwrap()
}

fn pdf_box(value: &Value) -> PdfBox {
    PdfBox {
        x: value["x"].as_f64(), y: value["y"].as_f64(),
        x2: value["x2"].as_f64(), y2: value["y2"].as_f64(),
    }
}

fn character(value: &Value) -> PdfCharacter {
    let compact = value.get("text").is_some();
    let bounds = pdf_box(&value["box"]);
    let visual = if compact { &value["visual_box"] } else { &value["visual_bbox"]["box"] };
    let font = if compact { &value["font_id"] } else { &value["pdf_style"]["font_id"] };
    let size = if compact { &value["font_size"] } else { &value["pdf_style"]["font_size"] };
    PdfCharacter {
        r#box: Some(Box::new(bounds)),
        visual_bbox: Some(Box::new(VisualBbox { r#box: Some(Box::new(pdf_box(visual))) })),
        pdf_style: Some(Box::new(PdfStyle {
            font_id: font.as_str().map(str::to_owned), font_size: size.as_f64(), ..Default::default()
        })),
        char_unicode: value[if compact { "text" } else { "char_unicode" }].as_str().map(str::to_owned),
        pdf_character_id: value["pdf_character_id"].as_i64(),
        render_order: value["render_order"].as_i64(),
        xobj_id: value["xobj_id"].as_i64(),
        vertical: value["vertical"].as_bool(),
        advance: value["advance"].as_f64(),
        ..Default::default()
    }
}

fn bounds_init(bounds: &PdfBox) -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit { x: bounds.x, y: bounds.y, x2: bounds.x2, y2: bounds.y2 }.into_value()
}

fn character_init(character: &PdfCharacter) -> ir::FieldValue<ir::types::PdfCharacter> {
    let style = character.pdf_style.as_deref().unwrap();
    ir::PdfCharacterInit {
        r#box: character.r#box.as_deref().map(bounds_init),
        visual_bbox: character.visual_bbox.as_deref().map(|visual| ir::VisualBboxInit {
            r#box: visual.r#box.as_deref().map(bounds_init),
        }.into_value()),
        pdf_style: Some(ir::PdfStyleInit {
            font_id: style.font_id.clone(), font_size: style.font_size, ..Default::default()
        }.into_value()),
        char_unicode: character.char_unicode.clone(), pdf_character_id: character.pdf_character_id,
        render_order: character.render_order, xobj_id: character.xobj_id,
        vertical: character.vertical, advance: character.advance,
        ..Default::default()
    }.into_value()
}

fn run_document(characters: &[PdfCharacter], layouts: &[PageLayout]) -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::LayoutParserView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_page_layout(&mut view, layouts.iter().map(|layout| ir::PageLayoutInit {
            id: layout.id, r#box: layout.r#box.as_deref().map(bounds_init),
            class_name: layout.class_name.clone(), conf: layout.conf,
        }.into_value()).collect()).unwrap();
    }
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        page.set_pdf_character(&mut view, characters.iter().map(character_init).collect()).unwrap();
    }
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    let config = ParagraphFinderConfig::default();
    let mut ids = RandomDebugIdSource;
    let mut inputs = ParagraphFinderInputs { config: &config, font_mapper: &mapper, debug_ids: &mut ids };
    let mut context = crate::StageContext::new(&NoopCancellation, &NoopProgress);
    super::process_with(&mut inputs, &mut document, &mut context).unwrap();
    document
}

fn run_stage(characters: &[PdfCharacter], layouts: &[PageLayout]) -> Value {
    run_document(characters, layouts).artifact().unwrap()
}

#[test]
fn typesetting_replaces_reconciled_source_without_leaving_fragments() {
    use babeldoc_p2_stages::stages::typesetting::Typesetting;
    let fragments = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let font_path = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font_path}), None).unwrap();
    for (label, range, replacement, protected) in [
        ("01-Tyser", (14160, 14205), "Supplementary information: translated description", "Supplementary information"),
        ("03-Cassetta", (11790, 11830), "1. Metchnikoff, E. translated title", "Metchnikoff"),
        ("08-science-5-6", (13236, 13312), "www.sciencemag.org/cgi/content/full/science.aar2131/DC1 translated methods", "www.sciencemag.org/cgi/content/full/science.aar2131/DC1"),
    ] {
        let characters: Vec<_> = fragments[label]["characters"].as_array().unwrap().iter()
            .filter(|item| item["render_order"].as_i64().is_some_and(|order| (range.0..=range.1).contains(&order)))
            .map(character).collect();
        let mut document = run_document(&characters, &stored_layouts(label));
        {
            let mut view = document.view::<ir::ParagraphFinderView>();
            let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
            paragraph.set_pdf_paragraph_composition(&mut view, vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some(replacement.into()),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(6.0), ..Default::default() }.into_value()),
                    ..Default::default()
                }.into_value()),
                ..Default::default()
            }.into_value()]).unwrap();
            paragraph.set_unicode(&mut view, Some(replacement.into())).unwrap();
        }
        let mut context = crate::StageContext::new(&NoopCancellation, &NoopProgress);
        Typesetting::new("en", mapper.clone(), false, Default::default()).process(&mut document, &mut context).unwrap();
        let artifact = document.artifact().unwrap();
        let page = &artifact["document"]["page"][0];
        assert!(page["pdf_character"].as_array().unwrap().is_empty(), "{label}");
        let paragraphs = page["pdf_paragraph"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 1, "{label}");
        let output: Vec<_> = paragraphs[0]["pdf_paragraph_composition"].as_array().unwrap().iter()
            .map(|part| &part["pdf_character"]).collect();
        let text: String = output.iter().map(|character| character["char_unicode"].as_str().unwrap_or_default()).collect();
        assert_eq!(text, replacement, "{label}");
        assert_eq!(text.matches(protected).count(), 1, "{label}");
        for (index, first) in output.iter().enumerate() {
            if first["char_unicode"] == " " { continue; }
            for second in &output[index + 1..] {
                if second["char_unicode"] == " " { continue; }
                let a = pdf_box(&first["box"]);
                let b = pdf_box(&second["box"]);
                let intersection = (a.x2.unwrap().min(b.x2.unwrap()) - a.x.unwrap().max(b.x.unwrap())).max(0.0)
                    * (a.y2.unwrap().min(b.y2.unwrap()) - a.y.unwrap().max(b.y.unwrap())).max(0.0);
                assert!(intersection < 0.01, "{label}: {first} overlaps {second}");
            }
        }
    }
}

fn source_owners(artifact: &Value) -> BTreeMap<i64, (usize, String, Value)> {
    let mut owners = BTreeMap::new();
    let page = &artifact["document"]["page"][0];
    for (index, paragraph) in page["pdf_paragraph"].as_array().unwrap().iter().enumerate() {
        for composition in paragraph["pdf_paragraph_composition"].as_array().unwrap() {
            let Some(characters) = composition["pdf_line"]["pdf_character"].as_array() else { continue; };
            for character in characters {
                if let Some(order) = character["render_order"].as_i64() {
                    assert!(owners.insert(order, (index, character["char_unicode"].as_str().unwrap().into(), character["box"].clone())).is_none(), "duplicate render order {order}");
                }
            }
        }
    }
    assert!(page["pdf_character"].as_array().unwrap().is_empty());
    owners
}

fn verify_sources(characters: &[PdfCharacter], artifact: &Value) -> BTreeMap<i64, (usize, String, Value)> {
    let owners = source_owners(artifact);
    for source in characters {
        let text = source.char_unicode.as_deref().unwrap_or_default();
        if text.chars().all(char::is_whitespace) { continue; }
        let order = source.render_order.unwrap();
        let (_, actual, bounds) = owners.get(&order).expect("source glyph must have one owner");
        assert_eq!(actual, text);
        let original = source.r#box.as_deref().unwrap();
        assert_eq!(bounds["x"].as_f64(), original.x);
        assert_eq!(bounds["y"].as_f64(), original.y);
        assert_eq!(bounds["x2"].as_f64(), original.x2);
        assert_eq!(bounds["y2"].as_f64(), original.y2);
    }
    owners
}

fn stored_layouts(label: &str) -> Vec<PageLayout> {
    let artifact = evidence(&format!(".baseline-repair-20261112/ordering/{label}-pre/pretranslation-artifact.json"));
    artifact["document"]["page"][1]["page_layout"].as_array().unwrap().iter().map(|value| PageLayout {
        id: value["id"].as_i64(), class_name: value["class_name"].as_str().map(str::to_owned),
        r#box: Some(Box::new(pdf_box(&value["box"]))), conf: value["conf"].as_f64(),
        ..Default::default()
    }).collect()
}

#[test]
fn saved_source_fragments_have_one_contiguous_paragraph_owner() {
    let fragments = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    for (label, range, target, expected, continuation) in [
        ("01-Tyser", (14160, 14205), (14160, 14184), "Supplementary information", "The online"),
        ("03-Cassetta", (11790, 11830), (11793, 11802), "Metchnikoff", "E. Ueber"),
        ("08-science-5-6", (13236, 13312), (13236, 13290), "www.sciencemag.org/cgi/content/full/science.aar2131/DC1", "Materials and Methods"),
    ] {
        let characters: Vec<_> = fragments[label]["characters"].as_array().unwrap().iter()
            .filter(|item| item["render_order"].as_i64().is_some_and(|order| (range.0..=range.1).contains(&order)))
            .map(character).collect();
        let artifact = run_stage(&characters, &stored_layouts(label));
        let owners = verify_sources(&characters, &artifact);
        let target_owners: BTreeSet<_> = owners.iter().filter(|(order, _)| (target.0..=target.1).contains(*order))
            .map(|(_, (owner, _, _))| *owner).collect();
        assert_eq!(target_owners.len(), 1, "{label}: {target_owners:?}");
        let paragraph = &artifact["document"]["page"][0]["pdf_paragraph"][*target_owners.first().unwrap()];
        let text = paragraph["unicode"].as_str().unwrap();
        assert!(text.contains(expected), "{label}: {text}");
        assert!(text.contains(continuation), "{label}: {text}");
    }
}

#[test]
fn science_authors_and_legal_use_source_lines_and_preserve_glyphs() {
    let stored = evidence(".baseline-priority-20261112/ordering/ordering-evidence.json");
    for (region, expected) in [
        ("authors", "Jonathan Barasch and Katalin Suszták"),
        ("legal", "The title Science is a registered trademark of AAAS."),
    ] {
        let characters: Vec<_> = stored[region]["characters_in_paint_order"].as_array().unwrap().iter().map(character).collect();
        let layout = PageLayout {
            id: Some(1), class_name: Some("plain text".into()),
            r#box: Some(Box::new(PdfBox { x: Some(0.0), y: Some(0.0), x2: Some(1000.0), y2: Some(1000.0) })),
            ..Default::default()
        };
        let artifact = run_stage(&characters, &[layout]);
        verify_sources(&characters, &artifact);
        let paragraphs = artifact["document"]["page"][0]["pdf_paragraph"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 1, "{region}");
        let text = paragraphs[0]["unicode"].as_str().unwrap();
        assert!(text.contains(expected), "{region}: {text}");
        assert!(text.starts_with(if region == "authors" { "Jihwan Park" } else { "Science (print ISSN" }), "{region}: {text}");
    }
}

#[test]
fn unsafe_scripts_formulas_and_columns_are_not_geometrically_reordered() {
    let source = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let characters: Vec<_> = source["01-Tyser"]["characters"].as_array().unwrap().iter()
        .filter(|item| item["render_order"].as_i64().is_some_and(|order| (14160..=14165).contains(&order)))
        .map(character).collect();
    assert!(reading_lines(&characters).is_some());
    for variant in 0..4 {
        let mut unsafe_characters = characters.clone();
        match variant {
            0 => unsafe_characters[1].char_unicode = Some("א".into()),
            1 => unsafe_characters[1].formula_layout_id = Some(0),
            2 => unsafe_characters[1].vertical = Some(true),
            _ => unsafe_characters[1].pdf_style.as_mut().unwrap().font_size = Some(3.0),
        }
        assert!(reading_lines(&unsafe_characters).is_none());
    }
    let mut columns = characters.clone();
    for character in &mut columns[3..] {
        let bounds = character.r#box.as_mut().unwrap();
        bounds.x = Some(bounds.x.unwrap() + 100.0);
        bounds.x2 = Some(bounds.x2.unwrap() + 100.0);
    }
    assert!(reading_lines(&columns).is_none());
}

#[test]
fn staggered_columns_are_not_treated_as_one_reading_flow() {
    let source = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let mut characters: Vec<_> = source["01-Tyser"]["characters"].as_array().unwrap().iter()
        .filter(|item| item["render_order"].as_i64().is_some_and(|order| (14160..=14165).contains(&order)))
        .map(character).collect();
    for (index, character) in characters.iter_mut().enumerate() {
        let left = if index < 3 { 20.0 + index as f64 * 4.0 } else { 120.0 + (index - 3) as f64 * 4.0 };
        let bottom = if index < 3 { 100.0 } else { 90.0 };
        let bounds = character.r#box.as_mut().unwrap();
        bounds.x = Some(left);
        bounds.x2 = Some(left + 4.0);
        bounds.y = Some(bottom);
        bounds.y2 = Some(bottom + 6.0);
    }
    assert!(reading_lines(&characters).is_none());
}

#[test]
fn fragment_reconciliation_respects_structural_and_formula_boundaries() {
    use crate::shared::layout_helper::Layout;
    let source = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let mut characters: Vec<_> = source["01-Tyser"]["characters"].as_array().unwrap().iter()
        .filter(|item| item["render_order"].as_i64().is_some_and(|order| (14160..=14164).contains(&order)))
        .map(character).collect();
    let geometry = character_geometry(&characters[0]).unwrap();
    let layout = |id, left, right| PageLayout {
        id: Some(id), class_name: Some("plain text".into()),
        r#box: Some(Box::new(PdfBox { x: Some(left), x2: Some(right), y: Some(geometry.1 - 1.0), y2: Some(geometry.3 + 1.0) })),
        ..Default::default()
    };
    let original = vec![Some(Layout::new(Some(1), Some("plain text".into()))), Some(Layout::new(Some(1), Some("plain text".into()))), Some(Layout::new(Some(1), Some("plain text".into()))), Some(Layout::new(Some(1), Some("plain text".into()))), Some(Layout::new(Some(2), Some("title".into())))];
    let mut assignments = original.clone();
    reconcile_token_layouts(&characters, &mut assignments, &[layout(1, 300.0, 320.0), layout(2, 320.0, 340.0)]);
    assert_eq!(assignments, original);
    reconcile_token_layouts(&characters, &mut assignments, &[layout(1, 300.0, 322.0), layout(2, 321.0, 340.0)]);
    assert_eq!(assignments, original);
    characters[4].formula_layout_id = Some(0);
    reconcile_token_layouts(&characters, &mut assignments, &[layout(1, 300.0, 340.0), layout(2, 315.0, 340.0)]);
    assert_eq!(assignments, original);
}

#[test]
fn overlapping_columns_do_not_transfer_a_complete_word() {
    use crate::shared::layout_helper::Layout;
    let source = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let characters: Vec<_> = source["01-Tyser"]["characters"].as_array().unwrap().iter()
        .filter(|item| item["render_order"].as_i64().is_some_and(|order| (14160..=14164).contains(&order)))
        .map(character).collect();
    let layout = |id, left, right| PageLayout {
        id: Some(id), class_name: Some("plain text".into()),
        r#box: Some(Box::new(PdfBox { x: Some(left), x2: Some(right), y: Some(260.0), y2: Some(290.0) })),
        ..Default::default()
    };
    let mut assignments = vec![Some(Layout::new(Some(1), Some("plain text".into()))); 4];
    assignments.push(Some(Layout::new(Some(2), Some("plain text".into()))));
    let original = assignments.clone();
    reconcile_token_layouts(&characters, &mut assignments, &[layout(1, 300.0, 340.0), layout(2, 315.0, 350.0)]);
    assert_eq!(assignments, original);
}

#[test]
fn adjacent_real_columns_and_formula_objects_keep_separate_owners() {
    let fragments = evidence(".baseline-repair-20261112/ordering/fragment-evidence.json");
    let mut characters: Vec<_> = fragments["01-Tyser"]["characters"].as_array().unwrap().iter()
        .filter(|item| item["render_order"].as_i64().is_some_and(|order| (14160..=14164).contains(&order)))
        .map(character).collect();
    let source_box = characters[3].visual_bbox.as_ref().unwrap().r#box.as_ref().unwrap();
    let split = (source_box.x2.unwrap() + characters[4].visual_bbox.as_ref().unwrap().r#box.as_ref().unwrap().x.unwrap()) / 2.0;
    let layout = |id, label: &str, x, x2, y, y2| PageLayout {
        id: Some(id), class_name: Some(label.into()),
        r#box: Some(Box::new(PdfBox { x: Some(x), x2: Some(x2), y: Some(y), y2: Some(y2) })),
        ..Default::default()
    };
    let artifact = run_stage(&characters, &[
        layout(1, "plain text", 300.0, split, 270.0, 283.0),
        layout(2, "plain text", split, 340.0, 270.0, 283.0),
    ]);
    assert_eq!(verify_sources(&characters, &artifact).values().map(|owner| owner.0).collect::<BTreeSet<_>>().len(), 2);
    characters[4].xobj_id = Some(42);
    let artifact = run_stage(&characters, &[layout(1, "plain text", 300.0, 340.0, 270.0, 283.0)]);
    assert_eq!(verify_sources(&characters, &artifact).values().map(|owner| owner.0).collect::<BTreeSet<_>>().len(), 2);
    let mut assignments = vec![Some(crate::shared::layout_helper::Layout::new(Some(1), Some("table_cell".into()))); characters.len()];
    assignments[4] = Some(crate::shared::layout_helper::Layout::new(Some(2), Some("title".into())));
    let original = assignments.clone();
    reconcile_token_layouts(&characters, &mut assignments, &[
        layout(1, "table_cell", 300.0, 340.0, 270.0, 283.0),
        layout(2, "title", 315.0, 340.0, 270.0, 283.0),
    ]);
    assert_eq!(assignments, original);
}
