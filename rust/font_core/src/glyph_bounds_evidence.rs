//! Observations of unhinted, unscaled outlines; these are not safety decisions.
use crate::FontFace;
use skrifa::instance::Size;
use skrifa::outline::{DrawError, DrawSettings, OutlineGlyphFormat, OutlinePen};
use skrifa::{FontRef, MetadataProvider, raw::types::GlyphId};

const MAX_GLYPH_BYTES: usize = 1_048_576;
const MAX_POINTS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    MissingGlyph,
    UnsupportedFont,
    UnsupportedComposite,
    Parse,
    NonFiniteCoordinate,
    LimitExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutlineObservation {
    NoOutline,
    Bounds(Bounds),
}

/// Coordinates are in font units. Divide by `units_per_em` to obtain em units.
/// Placement and combined transforms, stroke, hinting, clipping, color,
/// PDF glyph mapping and actual invocation are not verified by this observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphBoundsEvidence {
    pub glyph_id: u32,
    pub units_per_em: u16,
    pub observation: OutlineObservation,
}

/// A bounded collector of all endpoints and Bézier control points.
pub struct BoundsPen {
    bounds: Option<Bounds>,
    points: usize,
    limit: usize,
    error: Option<Failure>,
}

impl BoundsPen {
    pub fn new(limit: usize) -> Self {
        Self {
            bounds: None,
            points: 0,
            limit,
            error: None,
        }
    }

    pub fn finish(self) -> Result<OutlineObservation, Failure> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(self
            .bounds
            .map_or(OutlineObservation::NoOutline, OutlineObservation::Bounds))
    }

    fn point(&mut self, x: f32, y: f32) {
        if self.error.is_some() {
            return;
        }
        if !x.is_finite() || !y.is_finite() {
            self.error = Some(Failure::NonFiniteCoordinate);
            return;
        }
        if self.points >= self.limit {
            self.error = Some(Failure::LimitExceeded);
            return;
        }
        self.points += 1;
        match &mut self.bounds {
            Some(bounds) => {
                bounds.x_min = bounds.x_min.min(x);
                bounds.y_min = bounds.y_min.min(y);
                bounds.x_max = bounds.x_max.max(x);
                bounds.y_max = bounds.y_max.max(y);
            }
            None => {
                self.bounds = Some(Bounds {
                    x_min: x,
                    y_min: y,
                    x_max: x,
                    y_max: y,
                })
            }
        }
    }
}

impl OutlinePen for BoundsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }
    fn quad_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.point(x0, y0);
        self.point(x1, y1);
    }
    fn curve_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.point(x0, y0);
        self.point(x1, y1);
        self.point(x2, y2);
    }
    fn close(&mut self) {}
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, Failure> {
    let bytes = data
        .get(offset..offset.checked_add(2).ok_or(Failure::Parse)?)
        .ok_or(Failure::Parse)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32, Failure> {
    let bytes = data
        .get(offset..offset.checked_add(4).ok_or(Failure::Parse)?)
        .ok_or(Failure::Parse)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn table<'a>(data: &'a [u8], tag: &[u8; 4]) -> Result<Option<&'a [u8]>, Failure> {
    let count = usize::from(u16_at(data, 4)?);
    let end = 12usize
        .checked_add(count.checked_mul(16).ok_or(Failure::Parse)?)
        .ok_or(Failure::Parse)?;
    if end > data.len() {
        return Err(Failure::Parse);
    }
    let mut found = None;
    for index in 0..count {
        let record = 12 + index * 16;
        if &data[record..record + 4] == tag {
            if found.is_some() {
                return Err(Failure::Parse);
            }
            let start = u32_at(data, record + 8)? as usize;
            let length = u32_at(data, record + 12)? as usize;
            found = Some(
                data.get(start..start.checked_add(length).ok_or(Failure::Parse)?)
                    .ok_or(Failure::Parse)?,
            );
        }
    }
    Ok(found)
}

impl FontFace {
    /// Observes the explicitly supplied glyph ID, including ID 0 (`.notdef`).
    /// ID 0 is a valid glyph selection here and is not evidence that a character
    /// mapped successfully. An ID outside `numGlyphs` is reported as missing.
    pub fn glyph_bounds_evidence(&self, glyph_id: u32) -> Result<GlyphBoundsEvidence, Failure> {
        if glyph_id >= self.glyph_count {
            return Err(Failure::MissingGlyph);
        }
        let data = &self.data;
        if data.get(..4) != Some(&[0, 1, 0, 0]) {
            return Err(Failure::UnsupportedFont);
        }
        for tag in [
            b"fvar", b"gvar", b"avar", b"HVAR", b"VVAR", b"MVAR", b"VARC", b"CFF ", b"CFF2",
            b"COLR", b"CPAL", b"SVG ", b"CBDT", b"CBLC", b"EBDT", b"EBLC", b"sbix", b"bdat",
            b"bloc",
        ] {
            if table(data, tag)?.is_some() {
                return Err(Failure::UnsupportedFont);
            }
        }
        let head = table(data, b"head")?.ok_or(Failure::Parse)?;
        let units_per_em = u16_at(head, 18)?;
        if !(16..=16384).contains(&units_per_em) {
            return Err(Failure::Parse);
        }
        let loca = table(data, b"loca")?.ok_or(Failure::Parse)?;
        let glyf = table(data, b"glyf")?.ok_or(Failure::Parse)?;
        let index = glyph_id as usize;
        let loca_format = u16_at(head, 50)?;
        let entry_size = match loca_format {
            0 => 2usize,
            1 => 4usize,
            _ => return Err(Failure::Parse),
        };
        let maxp = table(data, b"maxp")?.ok_or(Failure::Parse)?;
        let num_glyphs = u32::from(u16_at(maxp, 4)?);
        if num_glyphs != self.glyph_count {
            return Err(Failure::Parse);
        }
        let required = (num_glyphs as usize)
            .checked_add(1)
            .and_then(|count| count.checked_mul(entry_size))
            .ok_or(Failure::Parse)?;
        if loca.len() < required {
            return Err(Failure::Parse);
        }
        let (start, end) = match loca_format {
            0 => (
                usize::from(u16_at(loca, index * 2)?) * 2,
                usize::from(u16_at(loca, (index + 1) * 2)?) * 2,
            ),
            1 => (
                u32_at(loca, index * 4)? as usize,
                u32_at(loca, (index + 1) * 4)? as usize,
            ),
            _ => return Err(Failure::Parse),
        };
        let bytes = glyf.get(start..end).ok_or(Failure::Parse)?;
        if bytes.len() > MAX_GLYPH_BYTES {
            return Err(Failure::LimitExceeded);
        }
        if bytes.is_empty() {
            return Ok(GlyphBoundsEvidence {
                glyph_id,
                units_per_em,
                observation: OutlineObservation::NoOutline,
            });
        }
        if bytes.len() < 10 {
            return Err(Failure::Parse);
        }
        let contours = i16::from_be_bytes([bytes[0], bytes[1]]);
        if contours < 0 {
            return Err(Failure::UnsupportedComposite);
        }
        if contours == 0 {
            return Err(Failure::UnsupportedFont);
        }
        let mut previous = None;
        for index in 0..contours as usize {
            let end_point = u16_at(bytes, 10 + index * 2)?;
            if previous.is_some_and(|value| end_point <= value) {
                return Err(Failure::Parse);
            }
            previous = Some(end_point);
        }
        if usize::from(previous.ok_or(Failure::Parse)?) >= MAX_POINTS / 2 {
            return Err(Failure::LimitExceeded);
        }
        let font = FontRef::new(data).map_err(|_| Failure::Parse)?;
        let outlines = font.outline_glyphs();
        if outlines.format() != Some(OutlineGlyphFormat::Glyf) {
            return Err(Failure::UnsupportedFont);
        }
        let glyph = outlines.get(GlyphId::new(glyph_id)).ok_or(Failure::Parse)?;
        let mut pen = BoundsPen::new(MAX_POINTS);
        glyph
            .draw(DrawSettings::unhinted(Size::unscaled(), &[][..]), &mut pen)
            .map_err(|error| match error {
                DrawError::InsufficientMemory
                | DrawError::TooManyPoints(_)
                | DrawError::RecursionLimitExceeded(_) => Failure::LimitExceeded,
                _ => Failure::Parse,
            })?;
        let observation = pen.finish()?;
        if contours > 0 && observation == OutlineObservation::NoOutline {
            return Err(Failure::Parse);
        }
        Ok(GlyphBoundsEvidence {
            glyph_id,
            units_per_em,
            observation,
        })
    }
}
