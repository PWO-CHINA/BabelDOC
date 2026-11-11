use babeldoc_ir_core::{self as ir, AccessError, Document, TranslationView};
use std::{io::Write, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};

static TRACKING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct PreparedTracking {
    temporary: PathBuf,
    destination: PathBuf,
}

impl PreparedTracking {
    pub(crate) fn prepare(destination: &Path, contents: &str) -> std::io::Result<Self> {
        if let Some(parent) = destination.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        loop {
            let sequence = TRACKING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut name = destination.as_os_str().to_owned();
            name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
            let temporary = PathBuf::from(name);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary) {
                Ok(mut file) => {
                    let prepared = Self { temporary, destination: destination.to_owned() };
                    let result = file.write_all(contents.as_bytes()).and_then(|_| file.flush());
                    drop(file);
                    result?;
                    return Ok(prepared);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn publish(self) -> std::io::Result<()> {
        std::fs::rename(&self.temporary, &self.destination)
    }
}

impl Drop for PreparedTracking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temporary);
    }
}

pub(crate) struct StageTransaction<'a> {
    original: &'a mut Document,
    candidate: Option<Document>,
}

impl<'a> StageTransaction<'a> {
    pub(crate) fn new(original: &'a mut Document, enabled: bool) -> Self {
        let candidate = enabled.then(|| original.fork());
        Self { original, candidate }
    }

    pub(crate) fn document(&mut self) -> &mut Document {
        match &mut self.candidate {
            Some(candidate) => candidate,
            None => self.original,
        }
    }

    pub(crate) fn commit(self) {
        if let Some(candidate) = self.candidate {
            *self.original = candidate;
        }
    }

    pub(crate) fn commit_with_tracking(self, tracking: Option<PreparedTracking>) -> std::io::Result<()> {
        if let Some(tracking) = tracking {
            tracking.publish()?;
        }
        self.commit();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParagraphLocation {
    pub page: usize,
    pub paragraph: usize,
}

pub(crate) struct Candidate {
    pub location: ParagraphLocation,
    pub unicode: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TrialOutcome {
    InvalidCandidate,
    ApplyFailed(AccessError),
    Cancelled,
    UnsupportedComposition,
    LayoutFailed,
    RejectedInsufficientEvidence,
}

pub(crate) enum LayoutOutcome {
    Completed,
    Cancelled,
    Failed,
}

pub(crate) fn trial(
    original: &Document,
    left: Candidate,
    right: Candidate,
    layout: impl FnOnce(&mut Document) -> LayoutOutcome,
) -> TrialOutcome {
    if left.location == right.location || left.unicode.is_empty() || right.unicode.is_empty() {
        return TrialOutcome::InvalidCandidate;
    }
    let supported = (|| {
        let view = original.read();
        for location in [left.location, right.location] {
            let paragraph = view.document().page_at(&view, location.page)?.pdf_paragraph_at(&view, location.paragraph)?;
            let Some(style) = paragraph.pdf_style(&view)? else { return Ok(false) };
            if style.font_id(&view)?.as_deref().is_none_or(str::is_empty)
                || style.font_size(&view)?.is_none_or(|size| !size.is_finite() || size <= 0.0) {
                return Ok(false);
            }
            if paragraph.pdf_paragraph_composition_len(&view)? == 0 {
                return Ok(false);
            }
            for composition in paragraph.pdf_paragraph_composition_iter(&view)? {
                let composition = composition?;
                if composition.pdf_same_style_unicode_characters(&view)?.is_none()
                    || composition.pdf_formula(&view)?.is_some()
                    || composition.pdf_same_style_characters(&view)?.is_some()
                    || composition.pdf_character(&view)?.is_some()
                    || composition.pdf_line(&view)?.is_some() {
                    return Ok::<bool, AccessError>(false);
                }
            }
        }
        Ok(true)
    })();
    match supported {
        Ok(true) => {},
        Ok(false) => return TrialOutcome::UnsupportedComposition,
        Err(error) => return TrialOutcome::ApplyFailed(error),
    }
    let mut candidate = original.fork();
    let applied = (|| {
        let mut view = candidate.view::<TranslationView>();
        for item in [left, right] {
            let paragraph = view.document().page_at(&view, item.location.page)?
                .pdf_paragraph_at(&view, item.location.paragraph)?;
            let style = paragraph.pdf_style(&view)?.map(|style| style.share(&mut view)).transpose()?;
            let characters = ir::PdfSameStyleUnicodeCharactersInit {
                pdf_style: style,
                unicode: Some(item.unicode.clone()),
                debug_info: None,
            }.into_value();
            paragraph.set_unicode(&mut view, Some(item.unicode))?;
            paragraph.set_pdf_paragraph_composition(&mut view, vec![ir::PdfParagraphCompositionInit {
                pdf_same_style_unicode_characters: Some(characters),
                ..Default::default()
            }.into_value()])?;
        }
        Ok::<(), AccessError>(())
    })();
    if let Err(error) = applied {
        return TrialOutcome::ApplyFailed(error);
    }
    match layout(&mut candidate) {
        LayoutOutcome::Completed => TrialOutcome::RejectedInsufficientEvidence,
        LayoutOutcome::Cancelled => TrialOutcome::Cancelled,
        LayoutOutcome::Failed => TrialOutcome::LayoutFailed,
    }
}

#[cfg(test)]
#[path = "../tests/cross_page_transaction/mod.rs"]
mod tests;
