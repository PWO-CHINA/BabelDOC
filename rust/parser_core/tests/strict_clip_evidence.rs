use babeldoc_parser_core::strict_clip_evidence::{observe, ClipEventKind, PaintKind, Rect, MAX_CLIP_EVENTS, MAX_PAINT_EVENTS};
use lopdf::{dictionary, Document, Object, Stream};

fn rect(x_min: i128, y_min: i128, x_max: i128, y_max: i128) -> Rect {
    Rect { x_min, y_min, x_max, y_max }
}

fn reopened_contents(data: &[u8]) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages = doc.new_object_id();
    let page = doc.new_object_id();
    let content = doc.add_object(Stream::new(dictionary! {}, data.to_vec()));
    doc.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
    doc.objects.insert(page, dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()], "Contents" => content }.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    let reopened = Document::load_mem(&bytes).unwrap();
    let contents = reopened.get_object(content).unwrap().as_stream().unwrap().get_plain_content_with_limit(1024 * 1024).unwrap();
    assert_eq!(contents, data);
    contents
}

fn run(data: &str) -> Result<babeldoc_parser_core::strict_clip_evidence::Evidence, babeldoc_parser_core::strict_clip_evidence::Uncertainty> {
    observe(&reopened_contents(data.as_bytes()), rect(0, 0, 100, 100))
}

#[test]
fn clips_intersect_with_both_rules_and_restore_graphics_state() {
    let evidence = run("% public synthetic\n10 10 80 80 re W n q 20 0 50 70 re W* n Q 0 30 60 60 re W n").unwrap();
    assert_eq!(evidence.events.iter().map(|event| (event.kind, event.clip)).collect::<Vec<_>>(), vec![(ClipEventKind::Intersection, Some(rect(10, 10, 90, 90))), (ClipEventKind::Intersection, Some(rect(20, 10, 70, 70))), (ClipEventKind::Restore, Some(rect(10, 10, 90, 90))), (ClipEventKind::Intersection, Some(rect(10, 30, 60, 90)))]);
    assert_eq!(evidence.final_clip, Some(rect(10, 30, 60, 90)));
}

#[test]
fn rectangle_fill_bounds_follow_ctm_clip_and_graphics_restore() {
    let data = "10 10 80 80 re W n q 2 0 0 3 5 7 cm 0 0 20 20 re f 50 50 4 4 re f* Q 0 0 20 20 re F";
    let evidence = run(data).unwrap();
    assert_eq!(evidence.paints.iter().map(|paint| paint.bounds).collect::<Vec<_>>(), vec![
        Some(rect(10, 10, 45, 67)), None, Some(rect(10, 10, 20, 20)),
    ]);
    assert_eq!(evidence.paints.iter().map(|paint| paint.offset).collect::<Vec<_>>(), vec![
        data.find("f 50").unwrap(), data.find("f* Q").unwrap(), data.rfind('F').unwrap(),
    ]);
    assert_eq!(evidence.final_clip, Some(rect(10, 10, 90, 90)));
}

#[test]
fn rectangle_fill_empty_and_late_unsupported_operators() {
    let evidence = run("0 0 0 10 re f 20 20 -10 -10 re f*").unwrap();
    assert_eq!(evidence.paints.iter().map(|paint| paint.bounds).collect::<Vec<_>>(), vec![None, Some(rect(10, 10, 20, 20))]);
    for data in ["f", "0 0 10 10 re W f", "0 0 10 10 re f 0 0 m S", "0 0 10 10 re f /X Do"] {
        assert!(run(data).is_err());
    }
}

#[test]
fn rectangle_strokes_expand_by_transformed_width_and_restore() {
    let evidence = run("q 5 w 10 10 10 20 re S Q 2 0 0 3 5 7 cm 3 w 10 10 10 10 re B* ").unwrap();
    assert_eq!(evidence.paints[0].kind, PaintKind::Stroke);
    assert_eq!(evidence.paints[0].path_bounds, rect(10, 10, 20, 30));
    assert_eq!(evidence.paints[0].bounds, Some(rect(7, 7, 23, 33)));
    assert_eq!(evidence.paints[1].kind, PaintKind::FillStroke);
    assert_eq!(evidence.paints[1].path_bounds, rect(25, 37, 45, 67));
    assert_eq!(evidence.paints[1].bounds, Some(rect(22, 32, 48, 72)));
    let restored = run("q 8 w 10 10 10 10 re S Q 10 10 10 10 re S").unwrap();
    assert_eq!(restored.paints[0].bounds, Some(rect(6, 6, 24, 24)));
    assert_eq!(restored.paints[1].bounds, Some(rect(9, 9, 21, 21)));
}

#[test]
fn rectangle_strokes_reject_hairlines_degenerate_paths_and_other_state() {
    for data in ["0 w 10 10 10 10 re S", "1 w 10 10 0 10 re S", "-1 w 10 10 10 10 re S",
        "1 J 10 10 10 10 re S", "2 j 10 10 10 10 re S", "[1] 0 d 10 10 10 10 re S"] {
        assert!(run(data).is_err(), "accepted {data:?}");
    }
    assert!(run("0 w 10 10 10 10 re f").is_ok());
}

#[test]
fn rectangle_fill_event_limit_rejects_partial_evidence() {
    let fill = "0 0 10 10 re f ";
    let input = fill.repeat(MAX_PAINT_EVENTS);
    assert_eq!(observe(input.as_bytes(), rect(0, 0, 100, 100)).unwrap().paints.len(), MAX_PAINT_EVENTS);
    let extra = format!("{input}{fill}");
    let error = observe(extra.as_bytes(), rect(0, 0, 100, 100)).unwrap_err();
    assert_eq!((error.offset, error.reason), (extra.len() - 2, "paint event limit exceeded"));
}

#[test]
fn applies_composed_ctm_and_negative_dimensions_at_path_construction() {
    let evidence = run("2 0 0 3 10 20 cm 1 0 0 1 5 4 cm 20 10 -10 -5 re W n").unwrap();
    assert_eq!(evidence.final_clip, Some(rect(40, 47, 60, 62)));
    let reflected = run("-1 0 0 1 80 0 cm 10 10 20 20 re W n").unwrap();
    assert_eq!(reflected.final_clip, Some(rect(50, 10, 70, 30)));
}

#[test]
fn empty_intersection_and_path_discard_are_observed() {
    let evidence = run("0 0 5 5 re n 10 10 10 10 re W n 30 30 2 2 re W* n").unwrap();
    assert_eq!(evidence.final_clip, None);
    assert_eq!(evidence.events.len(), 2);
    assert_eq!(run("0 0 0 10 re W n").unwrap().final_clip, None);
}

#[test]
fn nested_restores_report_positions_and_recover_from_empty_clip() {
    let data = "q 10 10 20 20 re W n q 80 80 5 5 re W n Q Q";
    let evidence = run(data).unwrap();
    let expected = [
        ("n q", ClipEventKind::Intersection, Some(rect(10, 10, 30, 30))),
        ("n Q", ClipEventKind::Intersection, None),
        ("Q Q", ClipEventKind::Restore, Some(rect(10, 10, 30, 30))),
        ("Q", ClipEventKind::Restore, Some(rect(0, 0, 100, 100))),
    ];
    let offsets = [data.find(expected[0].0).unwrap(), data.find(expected[1].0).unwrap(), data.find(expected[2].0).unwrap(), data.rfind(expected[3].0).unwrap()];
    for (event, ((_, kind, clip), offset)) in evidence.events.iter().zip(expected.into_iter().zip(offsets)) {
        assert_eq!((event.offset, event.kind, event.clip), (offset, kind, clip));
    }
    assert_eq!(evidence.events.len(), expected.len());
    assert_eq!(evidence.final_clip, Some(rect(0, 0, 100, 100)));
}

#[test]
fn event_budget_counts_intersections_and_restores() {
    let clip = "0 0 10 10 re W n ";
    let input = clip.repeat(MAX_CLIP_EVENTS);
    let evidence = observe(input.as_bytes(), rect(0, 0, 100, 100)).unwrap();
    assert_eq!(evidence.events.len(), MAX_CLIP_EVENTS);
    let extra = format!("{input}q Q");
    let error = observe(extra.as_bytes(), rect(0, 0, 100, 100)).unwrap_err();
    assert_eq!((error.offset, error.reason), (extra.len() - 1, "clip event limit exceeded"));
    let restores = "q Q ".repeat(MAX_CLIP_EVENTS + 1);
    let error = observe(restores.as_bytes(), rect(0, 0, 100, 100)).unwrap_err();
    assert_eq!((error.offset, error.reason), (restores.len() - 2, "clip event limit exceeded"));
    let extra_clip = format!("{input}{clip}");
    let error = observe(extra_clip.as_bytes(), rect(0, 0, 100, 100)).unwrap_err();
    assert_eq!((error.offset, error.reason), (extra_clip.len() - 2, "clip event limit exceeded"));
}

#[test]
fn stack_limit_and_late_errors_reject_all_evidence() {
    let input = format!("{}{}", "q ".repeat(256), "Q ".repeat(256));
    assert_eq!(observe(input.as_bytes(), rect(0, 0, 100, 100)).unwrap().events.len(), 256);
    let too_deep = "q ".repeat(257);
    let error = observe(too_deep.as_bytes(), rect(0, 0, 100, 100)).unwrap_err();
    assert_eq!((error.offset, error.reason), (512, "graphics stack limit exceeded"));
    assert!(run("q 0 0 10 10 re W n Q /X Do").is_err());
}

#[test]
fn rejects_malformed_and_unsupported_input_without_partial_evidence() {
    for data in [
        "Q", "q", "1 q", "1 2 3 re", "1 2 3 4 5 re", "10 10 20 20 re W", "W n",
        "0 0 10 10 re W q n Q", "0 0 10 10 re W W* n", "0 0 10 10 re", "1",
        "0 0 10 10 re 0 0 2 2 re W n", "1 1 0 1 0 0 cm", "1 0 1 1 0 0 cm",
        "0 0 0 1 0 0 cm", "0.5 0 1 1 re W n", "1e2 0 1 1 re W n", "/X Do",
        "0 0 m", "BT ET", "0 0 10 10 re W S", "n unknown", "[1] n", "9223372036854775808 n",
        "2 0 0 3 10 20 cm 1 0 0 1 5 4 cm 20 10 -10 -5 re 1 0 0 1 999 999 cm W n",
        "q 0 0 10 10 re Q W n",
        "9223372036854775807 0 0 1 0 0 cm 9223372036854775807 0 0 1 0 0 cm 3 0 0 1 0 0 cm",
    ] {
        assert!(run(data).is_err(), "accepted {data:?}");
    }
    assert!(observe(b"", rect(10, 0, 0, 1)).is_err());
}
