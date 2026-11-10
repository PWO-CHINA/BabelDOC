#[path = "../src/layout_audit.rs"]
mod layout_audit;
use layout_audit::{audit, AuditInput, Bounds, ExpectedPart, Formula, Glyph, NeighborEvidence, Rejection, TextPart};

fn bounds(left: f64, bottom: f64, right: f64, top: f64) -> Bounds { Bounds { left, bottom, right, top } }
fn glyph(text: &str, left: f64, right: f64) -> TextPart {
    TextPart::Glyph(Glyph { text: Some(text.into()), glyph_id: Some(1), font_size: Some(8.0), bounds: Some(bounds(left, 20.0, right, 28.0)) })
}
fn formula(id: &str, left: f64, right: f64) -> TextPart {
    TextPart::Formula(Formula { id: id.into(), bounds: Some(bounds(left, 20.0, right, 28.0)) })
}
fn valid() -> AuditInput {
    AuditInput {
        page_bounds: Some(bounds(0.0, 0.0, 100.0, 100.0)),
        expected_parts: Some(vec![ExpectedPart::Text("甲".into()), ExpectedPart::Formula("f1".into()), ExpectedPart::Text("乙".into()), ExpectedPart::Formula("f2".into())]),
        text_parts: Some(vec![glyph("甲", 10.0, 18.0), formula("f1", 18.0, 23.0), glyph("乙", 23.0, 31.0), formula("f2", 31.0, 36.0)]),
        neighbors: Some(NeighborEvidence { complete: true, bounds: vec![Some(bounds(10.0, 5.0, 40.0, 20.0))] }),
        minimum_font_size: 7.0,
    }
}

#[test]
fn accepts_complete_interleaved_units_and_touching_edges() { assert_eq!(audit(&valid()), Ok(())); }
#[test]
fn rejects_formula_moved_across_text_even_when_ids_unchanged() {
    let mut input = valid(); input.text_parts.as_mut().unwrap().swap(1, 2);
    assert!(audit(&input).unwrap_err().contains(&Rejection::TextMismatch));
}
#[test]
fn rejects_formula_glyph_and_formula_formula_overlaps() {
    let mut input = valid();
    input.text_parts = Some(vec![glyph("甲", 10.0, 20.0), formula("f1", 19.0, 25.0), glyph("乙", 25.0, 31.0), formula("f2", 24.0, 30.0)]);
    let reasons = audit(&input).unwrap_err();
    assert!(reasons.contains(&Rejection::FormulaUnitOverlap { formula: 1, other: 0 }));
    assert!(reasons.contains(&Rejection::FormulaUnitOverlap { formula: 1, other: 3 }));
    assert!(reasons.contains(&Rejection::FormulaUnitOverlap { formula: 3, other: 2 }));
}
#[test]
fn rejects_missing_glyph_small_type_and_bad_geometry() {
    let mut input = valid();
    let TextPart::Glyph(glyph) = &mut input.text_parts.as_mut().unwrap()[0] else { panic!("expected glyph") };
    glyph.glyph_id = Some(0); glyph.font_size = Some(6.0); glyph.bounds = Some(bounds(99.0, 20.0, 101.0, 28.0));
    let reasons = audit(&input).unwrap_err();
    assert!(reasons.contains(&Rejection::MissingGlyph(0)));
    assert!(reasons.contains(&Rejection::InvalidFontSize(0)));
    assert!(reasons.contains(&Rejection::GlyphOutsidePage(0)));
}
#[test]
fn rejects_missing_and_overlapping_formula_and_neighbor_evidence() {
    let mut input = valid();
    input.neighbors = Some(NeighborEvidence { complete: false, bounds: vec![Some(bounds(18.0, 21.0, 20.0, 25.0)), None] });
    let TextPart::Formula(formula) = &mut input.text_parts.as_mut().unwrap()[3] else { panic!("expected formula") }; formula.bounds = None;
    let reasons = audit(&input).unwrap_err();
    assert!(reasons.contains(&Rejection::IncompleteNeighbors));
    assert!(reasons.contains(&Rejection::MissingOrInvalidNeighborBounds(1)));
    assert!(reasons.contains(&Rejection::FormulaNeighborOverlap { formula: 1, neighbor: 0 }));
    assert!(reasons.contains(&Rejection::MissingOrInvalidFormulaBounds(3)));
}
#[test]
fn rejects_formula_loss_and_duplicate_identity() {
    let mut input = valid(); input.text_parts.as_mut().unwrap().pop();
    assert!(audit(&input).unwrap_err().contains(&Rejection::FormulaMismatch));
    let mut input = valid(); input.expected_parts.as_mut().unwrap()[3] = ExpectedPart::Formula("f1".into());
    assert!(audit(&input).unwrap_err().contains(&Rejection::InvalidFormulaIdentity));
}
#[test]
fn checks_ligature_and_proven_suppressed_whitespace() {
    let mut input = valid();
    input.expected_parts = Some(vec![ExpectedPart::Text("fi 乙".into())]);
    input.text_parts = Some(vec![glyph("fi", 10.0, 18.0), TextPart::SuppressedWhitespace { text: " ".into(), confirmed_layout_suppression: true }, glyph("乙", 18.0, 26.0)]);
    assert_eq!(audit(&input), Ok(()));
    input.text_parts.as_mut().unwrap()[1] = TextPart::SuppressedWhitespace { text: " ".into(), confirmed_layout_suppression: false };
    assert!(audit(&input).unwrap_err().contains(&Rejection::UnprovenWhitespace(1)));
}
