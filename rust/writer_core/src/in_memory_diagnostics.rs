//! Opt-in, incomplete observations of the production writer on isolated copies.
use crate::{
    BuildOptions, FontAsset, FormOutput, PageObjectMap, PageOutput, PdfDocument, WriterDiagnostic,
    WriterError, add_fonts_with_used_ids_from_page_objects, apply_page_output_with_page_objects,
    available_fonts_with_page_objects, build_page_output_with_diagnostics, used_font_ids,
    observe_font_binding, page_object_with_map, FontBindingEvidence,
};
use babeldoc_ir_core::{Document, ReadOnlyView, WriterView};
use std::collections::HashMap;

pub const IN_MEMORY_UNVERIFIED: &[&str] = &[
    "source PDF byte identity",
    "correctness of same-named font resources",
    "original drawing and clipping preservation",
    "saved, subsetted, and reopened output",
    "transaction safety or permission to release safety gates",
    "complete resource observation, including inheritance and malformed resources",
    "font IDs excluded by registration's used_font_ids, including formula-only fonts",
    "form failures hidden by a subsequent page-apply error",
];

#[derive(Debug)]
pub struct InMemoryWriterIssue {
    pub stage: &'static str,
    pub page_number: Option<i64>,
    pub scope: Option<i64>,
    pub xref_id: Option<i64>,
    pub message: String,
}

#[derive(Debug)]
pub struct InMemoryWriterReport {
    pub cancelled: bool,
    pub pages_applied: usize,
    pub diagnostics: Vec<WriterDiagnostic>,
    pub errors: Vec<InMemoryWriterIssue>,
    /// Each IR page is paired with the IDs returned by registration's
    /// `used_font_ids`. This is a per-page superset only of that collected set;
    /// formula-only fonts and other uncollected usage are not covered.
    pub font_bindings: Vec<InMemoryFontBinding>,
    /// Whether those page/registration-ID pairs were traversed without omission
    /// or cancellation; formula-only and other uncollected fonts are excluded.
    /// This does not mean any binding matched or authorize production output.
    pub font_binding_inventory_complete: bool,
    pub font_binding_inventory_limit: Option<FontBindingInventoryLimit>,
    pub unverified: &'static [&'static str],
}

#[derive(Debug)]
pub struct InMemoryFontBinding {
    pub page_index: usize,
    pub page_number: i64,
    pub font_id: String,
    pub outcome: InMemoryFontBindingOutcome,
}

#[derive(Debug)]
pub enum InMemoryFontBindingOutcome {
    Observed(FontBindingEvidence),
    NoAsset,
    AmbiguousAssets,
    ExtractionFailed,
    PageUnavailable,
    UnsupportedResourceName,
    ComparisonBudgetExceeded,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FontBindingInventoryLimit {
    Records,
    ComparisonBytes,
}

const MAX_BINDING_RECORDS: usize = 10_000;
const MAX_COMPARISON_BYTES: usize = 8 * 1024 * 1024;

fn supported_resource_name(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| (33..=126).contains(&byte)
        && !b"()<>[]{}/%#".contains(&byte))
}

impl Default for InMemoryWriterReport {
    fn default() -> Self {
        Self {
            cancelled: false,
            pages_applied: 0,
            diagnostics: Vec::new(),
            errors: Vec::new(),
            font_bindings: Vec::new(),
            font_binding_inventory_complete: false,
            font_binding_inventory_limit: None,
            unverified: IN_MEMORY_UNVERIFIED,
        }
    }
}

/// Never writes files or changes either input. An empty report is not a safety proof.
/// Cancellation is checked at registration callbacks and between observation items
/// and page stages; no strict cancellation latency bound is guaranteed.
pub fn diagnose_in_memory_writer(
    ir: &Document,
    pdf: &PdfDocument,
    page_objects: &PageObjectMap,
    assets: &[FontAsset<'_>],
    options: &BuildOptions,
    mut cancelled: impl FnMut() -> bool,
) -> InMemoryWriterReport {
    let mut report = InMemoryWriterReport {
        unverified: IN_MEMORY_UNVERIFIED,
        ..Default::default()
    };
    if cancelled() {
        report.cancelled = true;
        return report;
    }
    let mut ir = ir.fork();
    let mut pdf = pdf.clone();
    let used = match used_font_ids(&ir.view::<WriterView>()) {
        Ok(used) => used,
        Err(error) => {
            record(&mut report, "collect_fonts", None, None, None, error);
            return report;
        }
    };
    let mut was_cancelled = false;
    let registration = add_fonts_with_used_ids_from_page_objects(
        &mut pdf,
        &mut ir.view::<WriterView>(),
        assets,
        &used,
        page_objects,
        |_| {
            if cancelled() {
                was_cancelled = true;
                Err(WriterError::Invalid("cancelled".into()))
            } else {
                Ok(())
            }
        },
    );
    if was_cancelled {
        report.cancelled = true;
        return report;
    }
    if let Err(error) = registration {
        record(&mut report, "register_fonts", None, None, None, error);
        return report;
    }
    let view = ir.view::<ReadOnlyView>();
    let mut asset_by_id = HashMap::new();
    for asset in assets.iter().filter(|asset| used.contains(asset.id)) {
        asset_by_id.entry(asset.id).and_modify(|entry| *entry = None).or_insert(Some(asset));
    }
    let mut used_ids: Vec<_> = used.iter().collect();
    used_ids.sort_unstable();
    let mut embeddings: HashMap<&str, Option<&[u8]>> = HashMap::new();
    let mut comparison_bytes = 0usize;
    let mut complete = true;
    let observation_pages = match view.document().page_iter(&view) {
        Ok(pages) => pages,
        Err(error) => {
            record(&mut report, "font_binding_pages", None, None, None, error);
            return report;
        }
    };
    'observation: for (page_index, page) in observation_pages.enumerate() {
        if cancelled() { report.cancelled = true; return report; }
        let number = match page.and_then(|page| page.page_number(&view)) {
            Ok(Some(number)) => number,
            _ => { complete = false; continue; }
        };
        let page_object = page_object_with_map(&pdf, page_objects, number).ok();
        for id in &used_ids {
            if cancelled() { report.cancelled = true; return report; }
            if report.font_bindings.len() == MAX_BINDING_RECORDS {
                complete = false;
                report.font_binding_inventory_limit = Some(FontBindingInventoryLimit::Records);
                break 'observation;
            }
            let outcome = match asset_by_id.get(id.as_str()) {
                None => InMemoryFontBindingOutcome::NoAsset,
                Some(None) => InMemoryFontBindingOutcome::AmbiguousAssets,
                Some(Some(_)) if !supported_resource_name(id) => InMemoryFontBindingOutcome::UnsupportedResourceName,
                Some(Some(asset)) => match page_object {
                    None => InMemoryFontBindingOutcome::PageUnavailable,
                    Some(page_object) => match *embeddings.entry(id.as_str()).or_insert_with(|| asset.face.pdf_embedding().ok().map(|embedding| embedding.data)) {
                        None => InMemoryFontBindingOutcome::ExtractionFailed,
                        Some(data) if data.len() > MAX_COMPARISON_BYTES.saturating_sub(comparison_bytes) => {
                            complete = false;
                            report.font_binding_inventory_limit = Some(FontBindingInventoryLimit::ComparisonBytes);
                            InMemoryFontBindingOutcome::ComparisonBudgetExceeded
                        }
                        Some(data) => {
                            comparison_bytes += data.len();
                            InMemoryFontBindingOutcome::Observed(observe_font_binding(&pdf, page_object, id.as_bytes(), data))
                        }
                    },
                },
            };
            report.font_bindings.push(InMemoryFontBinding { page_index, page_number: number, font_id: (*id).clone(), outcome });
            if report.font_binding_inventory_limit.is_some() {
                if cancelled() { report.cancelled = true; return report; }
                break 'observation;
            }
        }
    }
    report.font_binding_inventory_complete = complete;
    let mut seen_form_xrefs = HashMap::new();
    let pages = match view.document().page_iter(&view) {
        Ok(pages) => pages,
        Err(error) => {
            record(&mut report, "pages", None, None, None, error);
            return report;
        }
    };
    for page in pages {
        if cancelled() {
            report.cancelled = true;
            break;
        }
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                record(&mut report, "page", None, None, None, error);
                break;
            }
        };
        let number = match page.page_number(&view) {
            Ok(Some(number)) => number,
            Ok(None) => {
                record(&mut report, "page", None, None, None, "missing page number");
                continue;
            }
            Err(error) => {
                record(&mut report, "page", None, None, None, error);
                continue;
            }
        };
        let forms = (|| -> crate::Result<Vec<FormOutput>> {
            page.pdf_xobject_iter(&view)?
                .map(|form| {
                    let form = form?;
                    Ok(FormOutput {
                        xobj_id: form.xobj_id(&view)?,
                        xref_id: form
                            .xref_id(&view)?
                            .ok_or_else(|| WriterError::Invalid("missing xref".into()))?,
                        stream: Vec::new(),
                    })
                })
                .collect()
        })();
        let forms = match forms {
            Ok(forms) => forms,
            Err(error) => {
                record(
                    &mut report,
                    "form_metadata",
                    Some(number),
                    None,
                    None,
                    error,
                );
                continue;
            }
        };
        for form in &forms {
            if let Some((first_page, first_scope)) = seen_form_xrefs.get(&form.xref_id) {
                record(
                    &mut report,
                    "shared_form_xref",
                    Some(number),
                    form.xobj_id,
                    Some(form.xref_id),
                    format!("form xref was already used on page {first_page}, scope {first_scope:?}"),
                );
            } else {
                seen_form_xrefs.insert(form.xref_id, (number, form.xobj_id));
            }
        }
        let empty = PageOutput {
            page_number: number,
            stream: Vec::new(),
            forms,
            counts: Default::default(),
        };
        let available = match available_fonts_with_page_objects(&pdf, &empty, page_objects) {
            Ok(available) => available,
            Err(error) => {
                record(
                    &mut report,
                    "observe_resources",
                    Some(number),
                    None,
                    None,
                    error,
                );
                continue;
            }
        };
        if cancelled() {
            report.cancelled = true;
            break;
        }
        let mut options = options.clone();
        if options.check_font_exists {
            options.available = Some(available.clone());
        }
        let (output, diagnostics) =
            match build_page_output_with_diagnostics(&view, &page, &options, &available) {
                Ok(result) => result,
                Err(error) => {
                    record(&mut report, "build", Some(number), None, None, error);
                    continue;
                }
            };
        report.diagnostics.extend(diagnostics);
        if cancelled() {
            report.cancelled = true;
            break;
        }
        match apply_page_output_with_page_objects(&mut pdf, &output, page_objects) {
            Ok(counts) => {
                report.pages_applied += 1;
                for (xref, error) in counts.form_failures {
                    for form in output.forms.iter().filter(|form| form.xref_id == xref) {
                        record(
                            &mut report,
                            "apply_form",
                            Some(number),
                            form.xobj_id,
                            Some(xref),
                            &error,
                        );
                    }
                }
            }
            Err(error) => record(&mut report, "apply_page", Some(number), None, None, error),
        }
    }
    if !report.cancelled && cancelled() {
        report.cancelled = true;
    }
    if report.cancelled {
        report.font_binding_inventory_complete = false;
    }
    report
}

fn record(
    report: &mut InMemoryWriterReport,
    stage: &'static str,
    page_number: Option<i64>,
    scope: Option<i64>,
    xref_id: Option<i64>,
    error: impl std::fmt::Display,
) {
    report.errors.push(InMemoryWriterIssue {
        stage,
        page_number,
        scope,
        xref_id,
        message: error.to_string(),
    });
}
