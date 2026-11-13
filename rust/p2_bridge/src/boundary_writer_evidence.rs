use crate::{
    callbacks::CancelFlag,
    font_mapper::FontMapperHandle,
    writer::{AssetSpec, WriteConfig},
};
use babeldoc_ir_core::Document;
use babeldoc_writer_core as core;
use sha2::{Digest, Sha256};

pub const MAX_SOURCE_TEXT_PAGES: usize = 64;
pub const MAX_SOURCE_TEXT_EVENT_BYTES: usize = 2 * 1024 * 1024;

pub const UNVERIFIED: &[&str] = &[
    "source PDF binding to IR lineage and target paragraph identity",
    "source PDF page-map binding for unbound calls",
    "source PDF identity in a subsequent production writer attempt and its callbacks",
    "identity of the final saved and postprocessed PDF",
    "font file identity and binding to layout or future writer execution",
    "correctness of same-named font resources",
    "original drawing and clipping preservation",
    "saved, subsetted, and reopened output",
    "transaction safety or permission to release safety gates",
    "complete resource observation, including inheritance and malformed resources",
    "font IDs excluded by registration's used_font_ids, including formula-only fonts",
    "form failures hidden by a subsequent page-apply error",
    "source text bytes and resource names mapped to IR characters, Unicode, glyphs or fonts",
    "source text visibility, clipping, placement, or successful writeback",
    "IR lineage, paragraph identity, drawing preservation, or final output identity from page ordinal matching",
];

#[derive(Debug)]
pub struct EvidenceIssue {
    pub stage: &'static str,
    pub page_number: Option<i64>,
    pub scope: Option<i64>,
    pub xref_id: Option<i64>,
}

#[derive(Debug)]
pub struct EvidenceDiagnostic {
    pub page_number: i64,
    pub scope: Option<i64>,
    pub output_scope: Option<i64>,
    pub output_is_form: bool,
    pub object_kind: &'static str,
    pub object_id: u64,
    pub render_order: i64,
    pub sub_render_order: i64,
    pub kind: core::WriterDiagnosticKind,
}

pub struct WriterEvidence {
    pub cancelled: bool,
    pub source_bytes_match: bool,
    pub source_pages: Vec<SourcePagePaint>,
    pub source_page_issues: Vec<SourcePageIssue>,
    /// Independent, non-authorizing source-text observations for bound target pages.
    pub source_text_pages: Vec<SourceTextPage>,
    pub pages_applied: usize,
    pub diagnostics: Vec<EvidenceDiagnostic>,
    pub errors: Vec<EvidenceIssue>,
    /// Page-local observations for registration's `used_font_ids` set; formula-only
    /// fonts can be omitted, and actual content usage and output are not established.
    pub font_bindings: Vec<core::InMemoryFontBinding>,
    /// Traversal coverage of those page/registration-ID pairs only.
    pub font_binding_inventory_complete: bool,
    pub font_binding_inventory_limit: Option<core::FontBindingInventoryLimit>,
    pub unverified: &'static [&'static str],
}

impl std::fmt::Debug for WriterEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WriterEvidence")
            .field("cancelled", &self.cancelled)
            .field("source_bytes_match", &self.source_bytes_match)
            .field("source_pages", &self.source_pages)
            .field("source_page_issues", &self.source_page_issues)
            .field("source_text_pages", &self.source_text_pages)
            .field("pages_applied", &self.pages_applied)
            .field("diagnostics", &self.diagnostics)
            .field("errors", &self.errors)
            .field("font_bindings_count", &self.font_bindings.len())
            .field("font_binding_inventory_complete", &self.font_binding_inventory_complete)
            .field("font_binding_inventory_limit", &self.font_binding_inventory_limit)
            .field("unverified", &self.unverified)
            .finish()
    }
}

#[derive(Debug)]
pub struct SourcePagePaint {
    pub page_index: usize,
    pub page_number: i64,
    pub evidence: babeldoc_parser_core::strict_page_paint_evidence::PageEvidence,
}

#[derive(Debug)]
pub struct SourcePageIssue {
    pub page_index: usize,
    pub reason: &'static str,
    pub content_offset: Option<usize>,
}

#[derive(Debug)]
pub struct SourceTextPage {
    pub page_index: usize,
    pub page_number: Option<i64>,
    pub outcome: SourceTextOutcome,
}

#[derive(Debug)]
pub enum SourceTextOutcome {
    Observed(babeldoc_parser_core::strict_text_source_evidence::Evidence),
    Uncertain(babeldoc_parser_core::strict_text_source_evidence::Uncertainty),
}

fn text_uncertain(reason: &'static str) -> SourceTextOutcome {
    SourceTextOutcome::Uncertain(babeldoc_parser_core::strict_text_source_evidence::Uncertainty {
        reason: babeldoc_parser_core::strict_text_source_evidence::Reason::Invalid(reason),
        offset: None,
    })
}

fn text_limit(reason: &'static str) -> SourceTextOutcome {
    SourceTextOutcome::Uncertain(babeldoc_parser_core::strict_text_source_evidence::Uncertainty {
        reason: babeldoc_parser_core::strict_text_source_evidence::Reason::Limit(reason),
        offset: None,
    })
}

fn cancel_text_observations(evidence: &mut WriterEvidence) {
    evidence.cancelled = true;
    evidence.source_pages.clear();
    for page in &mut evidence.source_text_pages {
        page.outcome = text_uncertain("cancelled");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SourcePaintCheck {
    NoObservedOverlap,
    Overlap { paint: usize, unit: usize },
    CoordinateSpaceUnverified,
    InvalidUnit,
    ComparisonLimitExceeded,
    Cancelled,
}

pub fn inspect_source_paint(
    source: &SourcePagePaint,
    occupied: &[babeldoc_p2_stages::layout_audit::Bounds],
    mut cancelled: impl FnMut() -> bool,
) -> SourcePaintCheck {
    const MAX_COMPARISONS: usize = 1_000_000;
    if cancelled() { return SourcePaintCheck::Cancelled; }
    if source.evidence.crop_box.x_min != 0 || source.evidence.crop_box.y_min != 0 {
        return SourcePaintCheck::CoordinateSpaceUnverified;
    }
    let mut comparisons = 0usize;
    for (unit_index, unit) in occupied.iter().enumerate() {
        if unit_index % 256 == 0 && cancelled() { return SourcePaintCheck::Cancelled; }
        if ![unit.left, unit.bottom, unit.right, unit.top].iter().all(|value| value.is_finite())
            || unit.left >= unit.right || unit.bottom >= unit.top {
            return SourcePaintCheck::InvalidUnit;
        }
        let left = unit.left.floor() as i128;
        let bottom = unit.bottom.floor() as i128;
        let right = unit.right.ceil() as i128;
        let top = unit.top.ceil() as i128;
        for (paint_index, paint) in source.evidence.contents.paints.iter().enumerate() {
            if comparisons == MAX_COMPARISONS { return SourcePaintCheck::ComparisonLimitExceeded; }
            comparisons += 1;
            if comparisons % 256 == 0 && cancelled() { return SourcePaintCheck::Cancelled; }
            let bounds = paint.paint_bounds;
            if left < bounds.x_max && right > bounds.x_min
                && bottom < bounds.y_max && top > bounds.y_min {
                return SourcePaintCheck::Overlap { paint: paint_index, unit: unit_index };
            }
        }
    }
    if cancelled() { SourcePaintCheck::Cancelled } else { SourcePaintCheck::NoObservedOverlap }
}

/// Observes an isolated writer attempt; `checked` selects the production retry mode.
/// This unbound entry does not compare source bytes with the parser input.
pub fn diagnose_boundary_writer(
    ir: &Document,
    cfg: &WriteConfig,
    specs: &[AssetSpec],
    page_objects: &core::PageObjectMap,
    mapper: &FontMapperHandle,
    flag: &CancelFlag,
    checked: bool,
) -> WriterEvidence {
    diagnose_boundary_writer_inner(ir, cfg, specs, page_objects, mapper, flag, checked, None, &[])
}

/// The caller must supply the digest and page map from the parse that created this IR.
/// Matching source bytes does not verify the IR lineage or a later production write.
pub fn diagnose_boundary_writer_bound(
    ir: &Document,
    cfg: &WriteConfig,
    specs: &[AssetSpec],
    page_objects: &core::PageObjectMap,
    mapper: &FontMapperHandle,
    flag: &CancelFlag,
    checked: bool,
    source_sha256: &[u8; 32],
    target_page_indices: &[usize],
) -> WriterEvidence {
    diagnose_boundary_writer_inner(ir, cfg, specs, page_objects, mapper, flag, checked, Some(source_sha256), target_page_indices)
}

fn diagnose_boundary_writer_inner(
    ir: &Document,
    cfg: &WriteConfig,
    specs: &[AssetSpec],
    page_objects: &core::PageObjectMap,
    mapper: &FontMapperHandle,
    flag: &CancelFlag,
    checked: bool,
    source_sha256: Option<&[u8; 32]>,
    target_page_indices: &[usize],
) -> WriterEvidence {
    let mut evidence = WriterEvidence {
        cancelled: false,
        source_bytes_match: false,
        source_pages: Vec::new(),
        source_page_issues: Vec::new(),
        source_text_pages: Vec::new(),
        pages_applied: 0,
        diagnostics: Vec::new(),
        errors: Vec::new(),
        font_bindings: Vec::new(),
        font_binding_inventory_complete: false,
        font_binding_inventory_limit: None,
        unverified: UNVERIFIED,
    };
    if source_sha256.is_some() {
        let mut seen = std::collections::HashSet::new();
        for &page_index in target_page_indices {
            if seen.insert(page_index) {
                evidence.source_text_pages.push(SourceTextPage {
                    page_index,
                    page_number: None,
                    outcome: text_uncertain("source observation unavailable"),
                });
            }
        }
    }
    if flag.is_cancelled() {
        cancel_text_observations(&mut evidence);
        return evidence;
    }
    if page_objects.is_empty() {
        for page in &mut evidence.source_text_pages {
            page.outcome = text_uncertain("source page identity unproven");
            evidence.source_page_issues.push(SourcePageIssue { page_index: page.page_index, reason: "source page identity unproven", content_offset: None });
        }
        evidence.errors.push(EvidenceIssue {
            stage: "page_object_map",
            page_number: None,
            scope: None,
            xref_id: None,
        });
        return evidence;
    }
    let faces = match specs
        .iter()
        .map(|spec| mapper.mapper.font_by_id(spec.id()))
        .collect::<Option<Vec<_>>>()
    {
        Some(faces) => faces,
        None => {
            evidence.errors.push(EvidenceIssue {
                stage: "resolve_font",
                page_number: None,
                scope: None,
                xref_id: None,
            });
            return evidence;
        }
    };
    let metadata: Vec<_> = specs.iter().map(AssetSpec::writer_metadata).collect();
    let assets: Vec<_> = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| core::FontAsset {
            id: spec.id(),
            face: &faces[index],
            metadata: &metadata[index],
        })
        .collect();
    if flag.is_cancelled() {
        cancel_text_observations(&mut evidence);
        return evidence;
    }
    let pdf = match source_sha256 {
        Some(expected) => {
            let bytes = match std::fs::read(&cfg.source) {
                Ok(bytes) => bytes,
                Err(_) => {
                    evidence.errors.push(EvidenceIssue { stage: "load_source", page_number: None, scope: None, xref_id: None });
                    return evidence;
                }
            };
            if flag.is_cancelled() {
                cancel_text_observations(&mut evidence);
                return evidence;
            }
            let actual: [u8; 32] = Sha256::digest(&bytes).into();
            if &actual != expected {
                evidence.errors.push(EvidenceIssue { stage: "source_identity_mismatch", page_number: None, scope: None, xref_id: None });
                return evidence;
            }
            let pdf = core::PdfDocument::load_mem(&bytes);
            evidence.source_bytes_match = pdf.is_ok();
            pdf
        }
        None => core::PdfDocument::load(&cfg.source),
    };
    let pdf = match pdf {
        Ok(pdf) => pdf,
        Err(_) => {
            if flag.is_cancelled() { cancel_text_observations(&mut evidence); }
            if !evidence.cancelled {
                evidence.errors.push(EvidenceIssue {
                    stage: "load_source",
                    page_number: None,
                    scope: None,
                    xref_id: None,
                });
            }
            return evidence;
        }
    };
    if evidence.source_bytes_match {
        let identity = babeldoc_parser_core::strict_page_identity::observe(&pdf, || flag.is_cancelled());
        let mut map_matches = false;
        if let Ok(pages) = &identity {
            if pages.len() == page_objects.len() {
                map_matches = true;
                for (index, expected) in pages.iter().enumerate() {
                    if flag.is_cancelled() { break; }
                    if core::page_object_with_map(&pdf, page_objects, index as i64).ok() != Some(*expected) {
                        map_matches = false;
                        break;
                    }
                }
            }
        }
        if flag.is_cancelled() {
            cancel_text_observations(&mut evidence);
            return evidence;
        }
        let view = ir.read();
        let mut seen = std::collections::HashSet::new();
        let mut text_index = 0;
        let mut retained_text_bytes = 0usize;
        let mut text_limit_reason = None;
        for &page_index in target_page_indices {
            if !seen.insert(page_index) { continue; }
            if flag.is_cancelled() {
                cancel_text_observations(&mut evidence);
                return evidence;
            }
            let current_text_index = text_index;
            text_index += 1;
            if current_text_index >= MAX_SOURCE_TEXT_PAGES && text_limit_reason.is_none() {
                text_limit_reason = Some("source text page budget");
            }
            if let Some(reason) = text_limit_reason {
                evidence.source_text_pages[current_text_index].outcome = text_limit(reason);
            }
            let number = view.document().page_at(&view, page_index)
                .and_then(|page| page.page_number(&view));
            let page_number = match number {
                Ok(Some(number)) => number,
                _ => {
                    evidence.source_page_issues.push(SourcePageIssue { page_index, reason: "source page number unavailable", content_offset: None });
                    continue;
                }
            };
            evidence.source_text_pages[current_text_index].page_number = Some(page_number);
            if !map_matches {
                evidence.source_text_pages[current_text_index].outcome = text_uncertain("source page identity unproven");
                evidence.source_page_issues.push(SourcePageIssue { page_index, reason: "source page identity unproven", content_offset: None });
                continue;
            }
            let expected_page = usize::try_from(page_number).ok()
                .and_then(|index| identity.as_ref().ok().and_then(|pages| pages.get(index))).copied();
            let page = match core::page_object_with_map(&pdf, page_objects, page_number) {
                Ok(page) => page,
                Err(_) => {
                    evidence.source_page_issues.push(SourcePageIssue { page_index, reason: "source page object unavailable", content_offset: None });
                    continue;
                }
            };
            if expected_page != Some(page) {
                evidence.source_text_pages[current_text_index].outcome = text_uncertain("source page identity unproven");
                evidence.source_page_issues.push(SourcePageIssue { page_index, reason: "source page identity unproven", content_offset: None });
                continue;
            }
            if text_limit_reason.is_none() {
                let text = babeldoc_parser_core::strict_text_source_evidence::observe_page(&pdf, page);
                if flag.is_cancelled() {
                    cancel_text_observations(&mut evidence);
                    return evidence;
                }
                evidence.source_text_pages[current_text_index].outcome = match text {
                    Ok(observation) => {
                        let total = observation.events.iter().try_fold(retained_text_bytes, |total, event| {
                            total.checked_add(event.encoded_bytes.len())
                                .and_then(|total| total.checked_add(event.resource_name.len()))
                        });
                        match total {
                            Some(total) if total <= MAX_SOURCE_TEXT_EVENT_BYTES => {
                                retained_text_bytes = total;
                                SourceTextOutcome::Observed(observation)
                            }
                            _ => {
                                text_limit_reason = Some("source text event byte budget");
                                text_limit("source text event byte budget")
                            }
                        }
                    }
                    Err(issue) => SourceTextOutcome::Uncertain(issue),
                };
            }
            match babeldoc_parser_core::strict_page_paint_evidence::observe_page(&pdf, page) {
                Ok(page_evidence) => evidence.source_pages.push(SourcePagePaint { page_index, page_number, evidence: page_evidence }),
                Err(issue) => evidence.source_page_issues.push(SourcePageIssue { page_index, reason: issue.reason, content_offset: issue.content_offset }),
            }
        }
    }
    let report = core::diagnose_in_memory_writer(
        ir,
        &pdf,
        page_objects,
        &assets,
        &cfg.build_options(checked),
        || flag.is_cancelled(),
    );
    if report.cancelled || flag.is_cancelled() {
        cancel_text_observations(&mut evidence);
    }
    evidence.pages_applied = report.pages_applied;
    evidence.font_bindings = report.font_bindings;
    evidence.font_binding_inventory_complete = report.font_binding_inventory_complete;
    evidence.font_binding_inventory_limit = report.font_binding_inventory_limit;
    evidence.errors = report
        .errors
        .into_iter()
        .map(|issue| EvidenceIssue {
            stage: issue.stage,
            page_number: issue.page_number,
            scope: issue.scope,
            xref_id: issue.xref_id,
        })
        .collect();
    evidence.diagnostics = report
        .diagnostics
        .into_iter()
        .map(|item| EvidenceDiagnostic {
            page_number: item.page_number,
            scope: item.scope,
            output_scope: item.output_scope,
            output_is_form: item.output_is_form,
            object_kind: item.object_kind,
            object_id: item.object_id,
            render_order: item.render_order,
            sub_render_order: item.sub_render_order,
            kind: item.kind,
        })
        .collect();
    if flag.is_cancelled() { cancel_text_observations(&mut evidence); }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use babeldoc_parser_core::strict_text_source_evidence::{Evidence, Reason, SourceRange, StreamEncoding, TextEvent};

    #[test]
    fn cancellation_discards_previously_retained_text() {
        let source = SourceRange { stream: (4, 0), concatenated: 0..2, stream_bytes: 0..2, encoding: StreamEncoding::Unfiltered };
        let observation = Evidence {
            page: (3, 0),
            events: vec![TextEvent {
                sequence: 0, page: (3, 0), resource_name: b"SecretResource".to_vec(),
                encoded_bytes: b"secret-encoded-bytes".to_vec(), operand: source.clone(), operator: source,
            }],
            unverified: babeldoc_parser_core::strict_text_source_evidence::UNVERIFIED,
        };
        let mut evidence = WriterEvidence {
            cancelled: false, source_bytes_match: true, source_pages: vec![], source_page_issues: vec![],
            source_text_pages: vec![
                SourceTextPage { page_index: 0, page_number: Some(0), outcome: SourceTextOutcome::Observed(observation) },
                SourceTextPage { page_index: 1, page_number: None, outcome: text_limit("source text page budget") },
            ],
            pages_applied: 0, diagnostics: vec![], errors: vec![], font_bindings: vec![],
            font_binding_inventory_complete: false, font_binding_inventory_limit: None, unverified: UNVERIFIED,
        };
        let debug = format!("{evidence:?}");
        assert!(!debug.contains("SecretResource") && !debug.contains("secret-encoded-bytes"));
        cancel_text_observations(&mut evidence);
        assert!(evidence.cancelled);
        assert!(evidence.source_text_pages.iter().all(|page| matches!(&page.outcome,
            SourceTextOutcome::Uncertain(issue) if issue.reason == Reason::Invalid("cancelled"))));
    }
}
