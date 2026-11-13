use babeldoc_ir_core as ir;
use babeldoc_p2_stages::{
    NoopCancellation, NoopProgress, StageContext,
    font_mapper::font_mapper_from_asset_paths,
    stages::typesetting::{PreviewRejection, PreviewUnit, Typesetting},
};

fn stage() -> Typesetting {
    let font = if cfg!(windows) { "C:/Windows/Fonts/arial.ttf" } else { "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf" };
    assert!(std::path::Path::new(font).is_file());
    let family = serde_json::json!({"normal":["test"],"script":[],"fallback":["test"],"base":["test"]});
    let mapper = font_mapper_from_asset_paths(&family, &serde_json::json!({"test":font}), None).unwrap();
    Typesetting::new("en", mapper, true, Default::default())
}

fn bounds(x: f64, y: f64, x2: f64, y2: f64) -> ir::FieldValue<ir::types::Box> {
    ir::BoxInit { x: Some(x), y: Some(y), x2: Some(x2), y2: Some(y2) }.into_value()
}

fn style() -> ir::FieldValue<ir::types::PdfStyle> {
    ir::PdfStyleInit { font_id: Some("test".into()), font_size: Some(10.0), ..Default::default() }.into_value()
}

fn document(text: &str, width: f64, footer: bool) -> ir::Document {
    let root = ir::Node::new(ir::NodeData::Document(ir::DataDocument {
        page: vec![ir::Edge::Inline(Box::new(ir::Node::new(ir::NodeData::Page(Default::default()))))],
        total_pages: Some(1),
    }));
    let mut document = ir::Document::from_parts(root, Default::default(), Default::default()).unwrap();
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let mut paragraphs = vec![ir::PdfParagraphInit {
            r#box: Some(bounds(10.0, 50.0, 10.0 + width, 100.0)), unicode: Some(text.into()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some(text.into()), pdf_style: Some(style()), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()];
        if footer {
            let characters = [("1", 20), ("2", 21)].into_iter().enumerate().map(|(index, (text, glyph))| {
                let left = 10.0 + index as f64 * 6.0;
                ir::PdfParagraphCompositionInit {
                    pdf_character: Some(ir::PdfCharacterInit {
                        char_unicode: Some(text.into()), pdf_character_id: Some(glyph),
                        r#box: Some(bounds(left, 10.0, left + 5.0, 20.0)),
                        visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(left, 10.0, left + 5.0, 20.0)) }.into_value()),
                        pdf_style: Some(style()), render_order: Some(10 + index as i64),
                        debug_info: Some(index == 0), advance: Some(5.0), ..Default::default()
                    }.into_value()), ..Default::default()
                }.into_value()
            }).collect();
            paragraphs.push(ir::PdfParagraphInit {
                r#box: Some(bounds(10.0, 10.0, 21.0, 20.0)), unicode: Some("12".into()),
                pdf_paragraph_composition: characters, ..Default::default()
            }.into_value());
        }
        page.set_pdf_paragraph(&mut view, paragraphs).unwrap();
    }
    document
}

fn preview(document: &ir::Document) -> (babeldoc_p2_stages::stages::typesetting::TypesettingPreview, Option<ir::Document>) {
    let (mut previews, fork) = stage().audit_fork_with_document(
        document, &[(0, 0)], &mut StageContext::new(&NoopCancellation, &NoopProgress),
    ).unwrap();
    (previews.remove(0), fork)
}

#[test]
fn leading_and_wrapped_spaces_remain_exact_bounded_evidence() {
    for (text, width, minimum_markers) in [("  Alpha beta ", 100.0, 2), ("A    B", 14.0, 1)] {
        let original = document(text, width, false);
        let (result, fork) = preview(&original);
        assert!(!result.rejections.contains(&PreviewRejection::TextMismatch), "{result:?}");
        assert!(!result.rejections.contains(&PreviewRejection::CaptureFailed), "{result:?}");
        assert!(!result.rejections.contains(&PreviewRejection::InvalidSpacingEvidence), "{result:?}");
        assert!(!result.rejections.contains(&PreviewRejection::OutsideOriginalTargetBounds), "{result:?}");
        let fork = fork.expect("exact output must be captured");
        let artifact = fork.artifact().unwrap();
        let compositions = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"].as_array().unwrap();
        let actual: String = compositions.iter().map(|part| part["pdf_character"]["char_unicode"].as_str().unwrap()).collect();
        assert_eq!(actual, text);
        let markers: Vec<_> = compositions.iter().map(|part| &part["pdf_character"])
            .filter(|character| character["synthetic_spacing"] == true).collect();
        assert!(markers.len() >= minimum_markers, "{text:?}: {compositions:?}");
        for marker in markers {
            assert!(marker["pdf_character_id"].is_null());
            assert_eq!(marker["char_unicode"], " ");
            assert_eq!(marker["box"]["x"], marker["box"]["x2"]);
        }
        assert!(result.units.iter().any(|unit| matches!(unit, PreviewUnit::Spacing { bounds: Some(bounds) } if bounds.left == bounds.right)));
    }
}

#[test]
fn missing_real_character_still_fails_capture() {
    let mut document = document("Alpha", 100.0, false);
    {
        let mut view = document.view::<ir::TypesettingView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        let composition = paragraph.pdf_paragraph_composition_at(&view, 0).unwrap();
        composition.pdf_same_style_unicode_characters(&view).unwrap().unwrap().set_unicode(&mut view, None).unwrap();
    }
    let (result, fork) = preview(&document);
    assert!(result.rejections.contains(&PreviewRejection::TypesettingFailed), "{result:?}");
    assert!(fork.is_none());
}

#[test]
fn direct_character_footer_does_not_abort_body_capture() {
    let document = document("Alpha beta", 100.0, true);
    let before = document.artifact().unwrap();
    let (result, fork) = preview(&document);
    assert!(!result.rejections.contains(&PreviewRejection::TypesettingFailed), "{result:?}");
    assert!(!result.rejections.contains(&PreviewRejection::CaptureFailed), "{result:?}");
    let artifact = fork.expect("footer must not abort capture").artifact().unwrap();
    assert_eq!(before["document"]["page"][0]["pdf_paragraph"][1]["pdf_paragraph_composition"],
        artifact["document"]["page"][0]["pdf_paragraph"][1]["pdf_paragraph_composition"]);
    assert_eq!(before, document.artifact().unwrap());
}

fn wrapped_rich_document(source_characters: &[(char, f64, f64, f64)], text: &str, translated: bool) -> ir::Document {
    let mut document = document(text, 200.0, false);
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        let characters = source_characters.iter().copied().map(|(ch, x, x2, y)| ir::PdfCharacterInit {
                char_unicode: Some(ch.to_string()), pdf_character_id: Some(ch as i64),
                r#box: Some(bounds(x, y, x2, y + 10.0)),
                visual_bbox: Some(ir::VisualBboxInit { r#box: Some(bounds(x, y, x2, y + 10.0)) }.into_value()),
                pdf_style: Some(style()), advance: Some(x2 - x), ..Default::default()
            }.into_value()).collect();
        let mut compositions = Vec::new();
        if translated {
            compositions.push(ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(ir::PdfSameStyleUnicodeCharactersInit {
                    unicode: Some("Test ".into()), pdf_style: Some(style()), ..Default::default()
                }.into_value()), ..Default::default()
            }.into_value());
        }
        compositions.push(ir::PdfParagraphCompositionInit {
            pdf_same_style_characters: Some(ir::PdfSameStyleCharactersInit {
                pdf_style: Some(style()), pdf_character: characters, ..Default::default()
            }.into_value()), ..Default::default()
        }.into_value());
        paragraph.set_pdf_paragraph_composition(&mut view, compositions).unwrap();
    }
    document
}

fn render(document: &mut ir::Document) -> serde_json::Value {
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    stage().process(document, &mut context).unwrap();
    document.artifact().unwrap()
}

#[test]
fn reflow_preserves_word_boundaries_inside_unchanged_rich_text() {
    let characters = [('o', 54.0, 58.0, 90.0), ('r', 58.0, 61.0, 90.0),
                      ('S', 10.0, 16.0, 80.0), ('h', 16.0, 20.0, 80.0)];
    let mut document = wrapped_rich_document(&characters, "Test or Sh", true);
    let (preview, fork) = preview(&document);
    assert!(!preview.rejections.contains(&PreviewRejection::TextMismatch), "{preview:?}");
    assert!(fork.is_some(), "the inferred source boundary must be auditable: {preview:?}");
    let artifact = render(&mut document);
    let output = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]
        .as_array().unwrap().iter().map(|part| &part["pdf_character"]).collect::<Vec<_>>();
    let source = output.iter().filter(|character| character["pdf_character_id"].as_i64()
        .is_some_and(|id| characters.iter().any(|(ch, ..)| *ch as i64 == id))).collect::<Vec<_>>();
    assert_eq!(source.iter().map(|character| character["char_unicode"].as_str().unwrap()).collect::<String>(), "orSh");
    let index = output.iter().position(|character| character["char_unicode"] == "r" && character["pdf_character_id"] == 'r' as i64).unwrap();
    let marker = output[index + 1];
    let right = output[index + 2];
    assert_eq!(marker["char_unicode"], " ");
    assert_eq!(marker["synthetic_spacing"], true);
    assert!(marker["pdf_character_id"].is_null());
    assert_eq!(marker["box"]["x"], marker["box"]["x2"]);
    assert_eq!(right["char_unicode"], "S");
    assert!(right["box"]["x"].as_f64().unwrap() - output[index]["box"]["x2"].as_f64().unwrap() > 0.5);
}

#[test]
fn source_wrap_insertion_does_not_modify_cjk_or_passthrough() {
    let cjk = [('中', 54.0, 64.0, 90.0), ('文', 10.0, 20.0, 80.0)];
    let mut document = wrapped_rich_document(&cjk, "Test 中文", true);
    let artifact = render(&mut document);
    let characters = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]
        .as_array().unwrap().iter().map(|part| &part["pdf_character"]).collect::<Vec<_>>();
    let index = characters.iter().position(|character| character["char_unicode"] == "中").unwrap();
    assert_eq!(characters[index + 1]["char_unicode"], "文");
    assert_eq!(characters[index]["pdf_character_id"], '中' as i64);
    assert_eq!(characters[index + 1]["pdf_character_id"], '文' as i64);

    let latin = [('o', 54.0, 58.0, 90.0), ('r', 58.0, 61.0, 90.0),
                 ('S', 10.0, 16.0, 80.0), ('h', 16.0, 20.0, 80.0)];
    let mut document = wrapped_rich_document(&latin, "or Sh", false);
    let artifact = render(&mut document);
    let characters = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]
        .as_array().unwrap().iter().map(|part| &part["pdf_character"]).collect::<Vec<_>>();
    assert_eq!(characters.len(), latin.len());
    for (character, (ch, x, x2, y)) in characters.iter().zip(latin) {
        assert_eq!(character["pdf_character_id"], ch as i64);
        assert_eq!(character["box"]["x"], x);
        assert_eq!(character["box"]["x2"], x2);
        assert_eq!(character["box"]["y"], y);
    }
}

fn source_spacing_document(translated: bool) -> ir::Document {
    let characters = [('y', 50.0, 55.0, 90.0), ('.', 55.0, 58.0, 90.0),
                      (' ', 58.0, 58.0, 90.0), ('C', 10.0, 16.0, 80.0)];
    let mut document = wrapped_rich_document(&characters, "y. C", translated);
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let paragraph = view.document().page_at(&view, 0).unwrap().pdf_paragraph_at(&view, 0).unwrap();
        let composition = paragraph.pdf_paragraph_composition_at(&view, usize::from(translated)).unwrap();
        let rich = composition.pdf_same_style_characters(&view).unwrap().unwrap();
        let space = rich.pdf_character_at(&view, 2).unwrap();
        space.set_pdf_character_id(&mut view, None).unwrap();
        space.set_synthetic_spacing(&mut view, Some(true)).unwrap();
        space.set_advance(&mut view, Some(0.0)).unwrap();
    }
    document
}

#[test]
fn existing_zero_width_source_space_gets_visible_advance_only_during_reflow() {
    let mut document = source_spacing_document(true);
    let (preview, fork) = preview(&document);
    assert!(!preview.rejections.contains(&PreviewRejection::TextMismatch), "{preview:?}");
    assert!(!preview.rejections.contains(&PreviewRejection::InvalidSpacingEvidence), "{preview:?}");
    assert!(fork.is_some(), "{preview:?}");
    let artifact = render(&mut document);
    let characters = artifact["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"]
        .as_array().unwrap().iter().map(|part| &part["pdf_character"]).collect::<Vec<_>>();
    let index = characters.iter().position(|character| character["char_unicode"] == ".").unwrap();
    assert_eq!(characters[index + 1]["synthetic_spacing"], true);
    assert_eq!(characters[index + 1]["box"]["x"], characters[index + 1]["box"]["x2"]);
    assert_eq!(characters[index + 2]["char_unicode"], "C");
    assert!(characters[index + 2]["box"]["x"].as_f64().unwrap()
        - characters[index]["box"]["x2"].as_f64().unwrap() > 0.5);
    for glyph in ['y', '.', 'C'] {
        assert_eq!(characters.iter().filter(|character| character["pdf_character_id"] == glyph as i64).count(), 1);
    }

    let mut passthrough = source_spacing_document(false);
    let before = passthrough.artifact().unwrap();
    let after = render(&mut passthrough);
    let original = before["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"][0]
        ["pdf_same_style_characters"]["pdf_character"].as_array().unwrap();
    let output = after["document"]["page"][0]["pdf_paragraph"][0]["pdf_paragraph_composition"].as_array().unwrap();
    assert_eq!(output.len(), original.len());
    for (part, original) in output.iter().zip(original) {
        assert_eq!(part["pdf_character"]["box"], original["box"]);
        assert_eq!(part["pdf_character"]["pdf_character_id"], original["pdf_character_id"]);
    }
}
