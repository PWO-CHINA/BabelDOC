use _rust_il::{
    boundary_writer_evidence::{diagnose_boundary_writer, diagnose_boundary_writer_bound, inspect_source_paint, SourcePaintCheck, SourceTextOutcome, MAX_SOURCE_TEXT_PAGES, MAX_SOURCE_TEXT_EVENT_BYTES},
    callbacks::CancelFlag,
    font_mapper::font_mapper_handle,
    writer::{AssetSpec, WriteConfig},
};
use babeldoc_ir_core as ir;
use babeldoc_parser_core::{Config, ParseObserver};
use babeldoc_p2_stages::{NoopCancellation, NoopProgress, StageContext, stages::typesetting::Typesetting};
use babeldoc_writer_core::PageObjectMap;
use serde_json::json;
use sha2::{Digest, Sha256};

fn node(data: ir::NodeData) -> ir::Edge {
    ir::Edge::Inline(Box::new(ir::Node::new(data)))
}

fn synthetic_pdf() -> Vec<u8> {
    synthetic_pdf_with_content("")
}

fn synthetic_pdf_with_content(content: &str) -> Vec<u8> {
    synthetic_pdf_with_resources(content, "<< >>")
}

fn synthetic_pdf_with_resources(content: &str, fonts: &str) -> Vec<u8> {
    synthetic_pdf_many(content, fonts, 1)
}

fn synthetic_pdf_many(content: &str, fonts: &str, count: usize) -> Vec<u8> {
    let kids = (0..count).map(|index| format!("{} 0 R", index + 3)).collect::<Vec<_>>().join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {count} >>"),
    ];
    for index in 0..count {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 100] /Resources << /Font {fonts} >> /Contents {} 0 R >>", 100 + index, count + 3));
    }
    objects.push(format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content));
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
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    bytes
}

#[test]
fn bound_source_text_is_independent_and_fails_closed() {
    pyo3::Python::initialize();
    let font = std::env::var_os("BABELDOC_TEST_FONT").expect("public test font required");
    let family = json!({"normal":["regular"],"script":[],"fallback":["regular"],"base":["regular"]}).to_string();
    let paths = json!({"regular":font.to_string_lossy()}).to_string();
    let mapper = font_mapper_handle(&family, &paths, None).unwrap();
    let content = "BT /SecretFont 12 Tf 1 0 0 1 0 0 Tm <4142> Tj ET";
    let fonts = "<< /SecretFont << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";
    let bytes = synthetic_pdf_with_resources(content, fonts);
    let path = std::env::temp_dir().join(format!("babeldoc-source-text-{}.pdf", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let cfg: WriteConfig = serde_json::from_value(json!({"source":path,"translated":"unused","debug_original":"unused"})).unwrap();
    let specs: Vec<AssetSpec> = serde_json::from_value(json!([{"id":"regular","metadata":{"font_name":"PublicFont","ascent":0.9,"descent":-0.2,"encoding_length":2}}])).unwrap();
    let document = fork();
    let before = document.artifact().unwrap();
    let map = PageObjectMap::new(vec![(3, 0)]);
    let digest = Sha256::digest(&bytes).into();
    let flag = CancelFlag::default();
    for checked in [false, true] {
        let report = diagnose_boundary_writer_bound(&document, &cfg, &specs, &map, &mapper, &flag, checked, &digest, &[0, 1, 0]);
        assert!(report.source_bytes_match, "{report:?}");
        assert_eq!(report.source_text_pages.len(), 2);
        let page = &report.source_text_pages[0];
        assert_eq!((page.page_index, page.page_number), (0, Some(0)));
        let SourceTextOutcome::Observed(observation) = &page.outcome else { panic!("{report:?}") };
        assert_eq!(observation.page, (3, 0));
        assert_eq!(observation.events.len(), 1);
        let event = &observation.events[0];
        assert_eq!(event.encoded_bytes, b"AB");
        assert_eq!(event.resource_name, b"SecretFont");
        assert_eq!(event.operand.stream, (4, 0));
        assert_eq!(&content.as_bytes()[event.operand.stream_bytes.clone()], b"<4142>");
        assert_eq!(&content.as_bytes()[event.operator.stream_bytes.clone()], b"Tj");
        assert_eq!(event.operand.concatenated, event.operand.stream_bytes);
        assert!(!observation.unverified.is_empty());
        assert!(matches!(report.source_text_pages[1].outcome, SourceTextOutcome::Uncertain(_)));
        let debug = format!("{report:?}");
        assert!(!debug.contains("SecretFont") && !debug.contains("4142"));
        assert_eq!(document.artifact().unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mismatch = diagnose_boundary_writer_bound(&document, &cfg, &specs, &map, &mapper, &flag, checked, &[0; 32], &[0]);
        assert!(!mismatch.source_bytes_match);
        assert!(matches!(mismatch.source_text_pages[0].outcome, SourceTextOutcome::Uncertain(_)));
    }
    let unsupported = synthetic_pdf_with_resources("BT /SecretFont 12 Tf 1 0 0 1 0 0 Tm (private) Tj ET", fonts);
    std::fs::write(&path, &unsupported).unwrap();
    let changed = diagnose_boundary_writer_bound(&document, &cfg, &specs, &map, &mapper, &flag, false, &digest, &[0]);
    assert!(!changed.source_bytes_match);
    assert!(matches!(changed.source_text_pages[0].outcome, SourceTextOutcome::Uncertain(_)));
    let unsupported_digest = Sha256::digest(&unsupported).into();
    let report = diagnose_boundary_writer_bound(&document, &cfg, &specs, &map, &mapper, &flag, false, &unsupported_digest, &[0]);
    assert!(report.source_bytes_match);
    assert!(matches!(report.source_text_pages[0].outcome, SourceTextOutcome::Uncertain(_)));
    assert!(!format!("{report:?}").contains("private"));
    assert_eq!(document.artifact().unwrap(), before);
    assert_eq!(std::fs::read(&path).unwrap(), unsupported);
    flag.cancel();
    let report = diagnose_boundary_writer_bound(&document, &cfg, &specs, &map, &mapper, &flag, false, &unsupported_digest, &[0]);
    assert!(report.cancelled);
    assert!(matches!(report.source_text_pages[0].outcome, SourceTextOutcome::Uncertain(_)));
}

fn fork() -> ir::Document {
    let bounds = || {
        node(ir::NodeData::Box(ir::DataBox {
            x: Some(0.0),
            y: Some(0.0),
            x2: Some(10.0),
            y2: Some(10.0),
        }))
    };
    let character = node(ir::NodeData::PdfCharacter(ir::DataPdfCharacter {
        pdf_character_id: Some(36),
        char_unicode: Some("A".into()),
        render_order: Some(1),
        sub_render_order: Some(0),
        r#box: Some(bounds()),
        pdf_style: Some(node(ir::NodeData::PdfStyle(ir::DataPdfStyle {
            font_id: Some("regular".into()),
            font_size: Some(12.0),
            ..Default::default()
        }))),
        ..Default::default()
    }));
    let page = ir::DataPage {
        page_number: Some(0),
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox {
            r#box: Some(bounds()),
        }))),
        pdf_character: vec![character],
        pdf_font: vec![node(ir::NodeData::PdfFont(ir::DataPdfFont {
            font_id: Some("regular".into()),
            encoding_length: Some(2),
            ..Default::default()
        }))],
        ..Default::default()
    };
    ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(page))],
            ..Default::default()
        })),
        Default::default(),
        Default::default(),
    )
    .unwrap()
    .fork()
}

fn many_page_fork(count: usize) -> ir::Document {
    let pages = (0..count).map(|index| node(ir::NodeData::Page(ir::DataPage {
        page_number: Some(index as i64),
        cropbox: Some(node(ir::NodeData::Cropbox(ir::DataCropbox {
            r#box: Some(node(ir::NodeData::Box(ir::DataBox {
                x: Some(0.0), y: Some(0.0), x2: Some(100.0), y2: Some(100.0),
            }))),
        }))),
        ..Default::default()
    }))).collect();
    ir::Document::from_parts(
        ir::Node::new(ir::NodeData::Document(ir::DataDocument { page: pages, ..Default::default() })),
        Default::default(),
        Default::default(),
    ).unwrap().fork()
}

#[test]
fn bound_source_text_limits_pages_and_aggregate_event_bytes() {
    use babeldoc_parser_core::strict_text_source_evidence::Reason;
    pyo3::Python::initialize();
    let font = std::env::var_os("BABELDOC_TEST_FONT").expect("public test font required");
    let family = json!({"normal":["regular"],"script":[],"fallback":["regular"],"base":["regular"]}).to_string();
    let paths = json!({"regular":font.to_string_lossy()}).to_string();
    let mapper = font_mapper_handle(&family, &paths, None).unwrap();
    let path = std::env::temp_dir().join(format!("babeldoc-source-text-budget-{}.pdf", std::process::id()));
    let cfg: WriteConfig = serde_json::from_value(json!({"source":path,"translated":"unused","debug_original":"unused"})).unwrap();
    let flag = CancelFlag::default();
    let fonts = "<< /SecretFont << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";
    let content = "BT /SecretFont 12 Tf 1 0 0 1 0 0 Tm <4142> Tj ET";
    let bytes = synthetic_pdf_many(content, fonts, MAX_SOURCE_TEXT_PAGES + 2);
    std::fs::write(&path, &bytes).unwrap();
    let document = many_page_fork(MAX_SOURCE_TEXT_PAGES + 2);
    let before = document.artifact().unwrap();
    let map = PageObjectMap::new((0..MAX_SOURCE_TEXT_PAGES + 2).map(|index| (index as u32 + 3, 0)).collect());
    let digest = Sha256::digest(&bytes).into();
    let targets: Vec<_> = (0..MAX_SOURCE_TEXT_PAGES + 2).collect();
    let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &map, &mapper, &flag, false, &digest, &targets);
    assert!(report.source_bytes_match);
    assert_eq!(report.source_text_pages.len(), targets.len());
    assert!(report.source_text_pages[..MAX_SOURCE_TEXT_PAGES].iter().all(|page| matches!(page.outcome, SourceTextOutcome::Observed(_))));
    assert!(report.source_text_pages[MAX_SOURCE_TEXT_PAGES..].iter().all(|page| matches!(&page.outcome, SourceTextOutcome::Uncertain(issue) if issue.reason == Reason::Limit("source text page budget"))));
    assert_eq!(document.artifact().unwrap(), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    let hex = "41".repeat(65_536);
    let mut large_content = String::from("BT /SecretFont 12 Tf 1 0 0 1 0 0 Tm ");
    for _ in 0..17 { large_content.push_str(&format!("<{hex}> Tj ")); }
    large_content.push_str("ET");
    let bytes = synthetic_pdf_many(&large_content, fonts, 3);
    std::fs::write(&path, &bytes).unwrap();
    let document = many_page_fork(3);
    let before = document.artifact().unwrap();
    let map = PageObjectMap::new(vec![(3, 0), (4, 0), (5, 0)]);
    let digest = Sha256::digest(&bytes).into();
    let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &map, &mapper, &flag, true, &digest, &[0, 1, 2]);
    assert!(report.source_bytes_match);
    let SourceTextOutcome::Observed(first) = &report.source_text_pages[0].outcome else { panic!("{report:?}") };
    let retained: usize = first.events.iter().map(|event| event.encoded_bytes.len() + event.resource_name.len()).sum();
    assert!(retained <= MAX_SOURCE_TEXT_EVENT_BYTES && retained * 2 > MAX_SOURCE_TEXT_EVENT_BYTES);
    assert!(report.source_text_pages[1..].iter().all(|page| matches!(&page.outcome, SourceTextOutcome::Uncertain(issue) if issue.reason == Reason::Limit("source text event byte budget"))));
    assert!(!format!("{report:?}").contains("SecretFont"));
    assert_eq!(document.artifact().unwrap(), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn bound_page_identity_rejects_wrong_maps() {
    pyo3::Python::initialize();
    let font = std::env::var_os("BABELDOC_TEST_FONT").expect("public test font required");
    let family = json!({"normal":["regular"],"script":[],"fallback":["regular"],"base":["regular"]}).to_string();
    let paths = json!({"regular":font.to_string_lossy()}).to_string();
    let mapper = font_mapper_handle(&family, &paths, None).unwrap();
    let bytes = synthetic_pdf_many("", "<< >>", 2);
    let path = std::env::temp_dir().join(format!("babeldoc-page-identity-{}.pdf", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let cfg: WriteConfig = serde_json::from_value(json!({"source":path,"translated":"unused","debug_original":"unused"})).unwrap();
    let document = many_page_fork(2);
    let before = document.artifact().unwrap();
    let digest = Sha256::digest(&bytes).into();
    let parsed = babeldoc_parser_core::parse_observed(&path, Config::default(), &mut CaptureSource).unwrap();
    let loaded = babeldoc_writer_core::PdfDocument::load_mem(&bytes).unwrap();
    assert_eq!(babeldoc_parser_core::strict_page_identity::observe(&loaded, || false).unwrap(), parsed.page_objects);
    for checked in [false, true] {
        for (ids, expected) in [(vec![(3, 0), (4, 0)], vec![true, true]), (vec![(4, 0), (3, 0)], vec![false, false]), (vec![(3, 0), (3, 0)], vec![false, false]), (vec![(3, 0)], vec![false, false]), (vec![(3, 0), (4, 1)], vec![false, false]), (vec![(3, 0), (4, 0), (3, 0)], vec![false, false])] {
            let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &PageObjectMap::new(ids), &mapper, &CancelFlag::default(), checked, &digest, &[0, 1]);
            for (index, valid) in expected.iter().enumerate() {
                assert_eq!(matches!(report.source_text_pages[index].outcome, SourceTextOutcome::Observed(_)), *valid, "{report:?}");
                assert_eq!(report.source_pages.iter().any(|page| page.page_index == index), *valid, "{report:?}");
            }
        }
        for ids in [vec![(3, 0), (3, 0)], vec![(3, 0)], vec![]] {
            let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &PageObjectMap::new(ids), &mapper, &CancelFlag::default(), checked, &digest, &[0]);
            assert!(matches!(report.source_text_pages[0].outcome, SourceTextOutcome::Uncertain(_)), "{report:?}");
            assert!(report.source_pages.is_empty(), "{report:?}");
            assert_eq!(report.source_page_issues.len(), 1);
        }
        let flag = CancelFlag::default();
        flag.cancel();
        let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &PageObjectMap::new(vec![(3, 0), (4, 0)]), &mapper, &flag, checked, &digest, &[0, 1]);
        assert!(report.cancelled && report.source_pages.is_empty());
        assert!(report.source_text_pages.iter().all(|page| matches!(page.outcome, SourceTextOutcome::Uncertain(_))));
    }
    let original_text = std::str::from_utf8(&bytes).unwrap();
    for malformed in [
        original_text.replace("/Count 2", "/Count 1"),
        original_text.replace("/Kids [3 0 R 4 0 R]", "/Kids [3 0 R 3 0 R]"),
        original_text.replace("/Kids [3 0 R 4 0 R]", "/Kids [2 0 R 4 0 R]"),
    ] {
        let malformed = malformed.into_bytes();
        std::fs::write(&path, &malformed).unwrap();
        let malformed_digest = Sha256::digest(&malformed).into();
        for checked in [false, true] {
            let report = diagnose_boundary_writer_bound(&document, &cfg, &[], &PageObjectMap::new(vec![(3, 0), (4, 0)]), &mapper, &CancelFlag::default(), checked, &malformed_digest, &[0, 1]);
            assert!(report.source_bytes_match && report.source_pages.is_empty(), "{report:?}");
            assert!(report.source_text_pages.iter().all(|page| matches!(page.outcome, SourceTextOutcome::Uncertain(_))), "{report:?}");
            assert_eq!(report.source_page_issues.len(), 2);
        }
    }
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(document.artifact().unwrap(), before);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn production_inputs_are_observed_without_mutating_fork_or_file() {
    pyo3::Python::initialize();
    let font = std::env::var_os("BABELDOC_TEST_FONT")
        .expect("BABELDOC_TEST_FONT must point to a profiled public font");
    let within_font_budget = std::fs::metadata(&font).unwrap().len() <= 8 * 1024 * 1024;
    let family =
        json!({"normal": ["regular"], "script": [], "fallback": ["regular"], "base": ["regular"]})
            .to_string();
    let paths = json!({"regular": font.to_string_lossy()}).to_string();
    let mapper = font_mapper_handle(&family, &paths, None).unwrap();
    let path = std::env::temp_dir().join(format!(
        "babeldoc-writer-evidence-{}.pdf",
        std::process::id()
    ));
    let bytes = synthetic_pdf();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let cfg: WriteConfig = serde_json::from_value(
        json!({"source": path, "translated": "unused", "debug_original": "unused"}),
    )
    .unwrap();
    let specs: Vec<AssetSpec> = serde_json::from_value(json!([{"id":"regular", "metadata":{"font_name":"PublicFont", "ascent":0.9, "descent":-0.2, "encoding_length":2}}])).unwrap();
    let ir = fork();
    let before = ir.artifact().unwrap();
    let map = PageObjectMap::new(vec![(3, 0)]);
    let flag = CancelFlag::default();
    for checked in [false, true] {
        let report = diagnose_boundary_writer(&ir, &cfg, &specs, &map, &mapper, &flag, checked);
        assert!(!report.cancelled && report.errors.is_empty(), "{report:?}");
        assert!(!report.source_bytes_match);
        assert_eq!(report.pages_applied, 1);
        assert_eq!(report.font_bindings[0].page_index, 0);
        if within_font_budget {
            assert!(report.font_binding_inventory_complete);
            assert!(report.font_binding_inventory_limit.is_none());
            assert!(matches!(&report.font_bindings[0].outcome, babeldoc_writer_core::InMemoryFontBindingOutcome::Observed(binding) if binding.outcome.is_ok()));
        } else {
            assert!(!report.font_binding_inventory_complete);
            assert_eq!(report.font_binding_inventory_limit, Some(babeldoc_writer_core::FontBindingInventoryLimit::ComparisonBytes));
            assert!(matches!(&report.font_bindings[0].outcome, babeldoc_writer_core::InMemoryFontBindingOutcome::ComparisonBudgetExceeded));
        }
        assert!(!report.unverified.is_empty());
        let digest = Sha256::digest(&bytes).into();
        let report = diagnose_boundary_writer_bound(&ir, &cfg, &specs, &map, &mapper, &flag, checked, &digest, &[0]);
        assert!(report.source_bytes_match && report.errors.is_empty(), "{report:?}");
        assert_eq!(report.pages_applied, 1);
        assert_eq!(report.source_pages.len(), 1, "{report:?}");
        assert!(report.source_page_issues.is_empty(), "{report:?}");
        let report = diagnose_boundary_writer_bound(&ir, &cfg, &specs, &map, &mapper, &flag, checked, &[0; 32], &[0]);
        assert!(!report.source_bytes_match);
        assert_eq!(report.pages_applied, 0);
        assert_eq!(report.errors[0].stage, "source_identity_mismatch");
    }
    let missing: Vec<AssetSpec> = serde_json::from_value(json!([{"id":"missing", "metadata":{"font_name":"PublicFont", "ascent":0.9, "descent":-0.2, "encoding_length":2}}])).unwrap();
    assert_eq!(
        diagnose_boundary_writer(&ir, &cfg, &missing, &map, &mapper, &flag, false).errors[0].stage,
        "resolve_font"
    );
    assert_eq!(
        diagnose_boundary_writer(
            &ir,
            &cfg,
            &specs,
            &PageObjectMap::default(),
            &mapper,
            &flag,
            false
        )
        .errors[0]
            .stage,
        "page_object_map"
    );
    flag.cancel();
    assert!(diagnose_boundary_writer(&ir, &cfg, &specs, &map, &mapper, &flag, false).cancelled);
    assert_eq!(before, ir.artifact().unwrap());
    assert_eq!(bytes, std::fs::read(&path).unwrap());
}

struct CaptureSource;
impl ParseObserver for CaptureSource {
    fn capture_source_sha256(&self) -> bool { true }
}

#[test]
fn parsed_source_and_actual_typesetting_fork_feed_bound_writer() {
    let font = std::env::var_os("BABELDOC_TEST_FONT").expect("BABELDOC_TEST_FONT must point to a real public font");
    let family = json!({"normal":["regular"],"script":[],"fallback":["regular"],"base":["regular"]}).to_string();
    let paths = json!({"regular":font.to_string_lossy()}).to_string();
    let mapper = font_mapper_handle(&family, &paths, None).unwrap();
    let path = std::env::temp_dir().join(format!("babeldoc-bound-writer-evidence-{}.pdf", std::process::id()));
    let bytes = synthetic_pdf_with_content("0 0 5 5 re f");
    std::fs::write(&path, &bytes).unwrap();
    let parsed = babeldoc_parser_core::parse_observed(&path, Config::default(), &mut CaptureSource).unwrap();
    let digest = parsed.source_sha256.expect("source identity captured");
    let map = PageObjectMap::new(parsed.page_objects);
    let mut document = parsed.document;
    {
        let mut view = document.view::<ir::ParagraphFinderView>();
        let page = view.document().page_at(&view, 0).unwrap();
        let bounds = || ir::BoxInit { x: Some(10.0), y: Some(10.0), x2: Some(50.0), y2: Some(30.0) }.into_value();
        page.set_pdf_paragraph(&mut view, vec![ir::PdfParagraphInit {
            r#box: Some(bounds()), unicode: Some("A".into()),
            pdf_paragraph_composition: vec![ir::PdfParagraphCompositionInit {
                pdf_line: Some(ir::PdfLineInit { r#box: Some(bounds()), pdf_character: vec![ir::PdfCharacterInit {
                    char_unicode: Some("A".into()), pdf_character_id: Some(36), r#box: Some(bounds()),
                    pdf_style: Some(ir::PdfStyleInit { font_id: Some("regular".into()), font_size: Some(10.0), ..Default::default() }.into_value()), ..Default::default()
                }.into_value()], ..Default::default() }.into_value()), ..Default::default()
            }.into_value()], ..Default::default()
        }.into_value()]).unwrap();
    }
    let before = document.artifact().unwrap();
    let stage = Typesetting::new("en", mapper.mapper.clone(), true, Default::default());
    let mut context = StageContext::new(&NoopCancellation, &NoopProgress);
    let (previews, fork) = stage.audit_fork_with_document(&document, &[(0, 0)], &mut context).unwrap();
    assert_eq!(previews[0].glyphs.len(), 1);
    let fork = fork.expect("actual typesetting fork");
    let fork_before = fork.artifact().unwrap();
    let cfg: WriteConfig = serde_json::from_value(json!({"source":path,"translated":"unused","debug_original":"unused"})).unwrap();
    let specs: Vec<AssetSpec> = serde_json::from_value(json!([{"id":"regular","metadata":{"font_name":"PublicFont","ascent":0.9,"descent":-0.2,"encoding_length":2}}])).unwrap();
    let flag = CancelFlag::default();
    let report = diagnose_boundary_writer_bound(&fork, &cfg, &specs, &map, &mapper, &flag, false, &digest, &[0]);
    assert!(report.source_bytes_match, "{report:?}");
    assert_eq!(report.pages_applied, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.source_pages.len(), 1, "{report:?}");
    assert!(report.source_page_issues.is_empty(), "{report:?}");
    let paints = &report.source_pages[0].evidence.contents.paints;
    assert_eq!(paints.len(), 1);
    assert_eq!(paints[0].bounds.unwrap().x_max, 5);
    let bounds = |left, bottom, right, top| babeldoc_p2_stages::layout_audit::Bounds { left, bottom, right, top };
    let source = &report.source_pages[0];
    assert_eq!(inspect_source_paint(source, &[bounds(4.5, 4.5, 6.0, 6.0)], || false), SourcePaintCheck::Overlap { paint: 0, unit: 0 });
    assert_eq!(inspect_source_paint(source, &[bounds(5.0, 5.0, 7.0, 7.0)], || false), SourcePaintCheck::NoObservedOverlap);
    assert_eq!(inspect_source_paint(source, &[bounds(f64::NAN, 0.0, 1.0, 1.0)], || false), SourcePaintCheck::InvalidUnit);
    assert_eq!(inspect_source_paint(source, &[bounds(0.0, 0.0, 1.0, 1.0)], || true), SourcePaintCheck::Cancelled);
    let many = vec![bounds(6.0, 6.0, 7.0, 7.0); 1_000_001];
    assert_eq!(inspect_source_paint(source, &many, || false), SourcePaintCheck::ComparisonLimitExceeded);
    let mut checks = 0;
    assert_eq!(inspect_source_paint(source, &many, || { checks += 1; checks == 5 }), SourcePaintCheck::Cancelled);
    let mut shifted = source.evidence.clone();
    shifted.crop_box.x_min = 10;
    let shifted = _rust_il::boundary_writer_evidence::SourcePagePaint { page_index: 0, page_number: 0, evidence: shifted };
    assert_eq!(inspect_source_paint(&shifted, &[bounds(0.0, 0.0, 1.0, 1.0)], || false), SourcePaintCheck::CoordinateSpaceUnverified);
    let mut clipped = source.evidence.clone();
    clipped.contents.paints[0].bounds = None;
    let clipped = _rust_il::boundary_writer_evidence::SourcePagePaint { page_index: 0, page_number: 0, evidence: clipped };
    assert_eq!(inspect_source_paint(&clipped, &[bounds(1.0, 1.0, 2.0, 2.0)], || false), SourcePaintCheck::Overlap { paint: 0, unit: 0 });
    assert_eq!(before, document.artifact().unwrap());
    assert_eq!(fork_before, fork.artifact().unwrap());
    std::fs::write(&path, b"different source").unwrap();
    let report = diagnose_boundary_writer_bound(&fork, &cfg, &specs, &map, &mapper, &flag, false, &digest, &[0]);
    assert!(!report.source_bytes_match);
    assert_eq!(report.pages_applied, 0);
    assert_eq!(report.errors[0].stage, "source_identity_mismatch");
}
