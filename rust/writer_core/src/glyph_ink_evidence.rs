//! Conditional geometry observations only; never a safety decision.
use babeldoc_font_core::glyph_bounds_evidence::{Failure, GlyphBoundsEvidence, OutlineObservation};

/// All fields are caller assertions, not inspected PDF graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InkStateAssertions {
    pub horizontal_unit_text_matrix: bool,
    pub default_horizontal_scale_and_rise: bool,
    pub fill_only_no_stroke: bool,
    pub no_other_transform: bool,
    pub simple_static_unhinted_truetype: bool,
    /// Caller has established that every coordinate and intermediate used by
    /// the outline extraction is an integer or half-integer in [-32768, 32768],
    /// and all operations preserve those exact values. Checking the returned
    /// f32 bounds alone cannot establish this provenance.
    pub exact_half_unit_extraction: bool,
}

pub struct GlyphInkInput<'a> {
    /// Exact tokens captured from the writer, including the serialized `cm` translation.
    pub x: &'a str,
    pub y: &'a str,
    pub font_size: &'a str,
    pub crop_translation_x: &'a str,
    pub crop_translation_y: &'a str,
    pub confirmed_units_per_em: u16,
    pub glyph: Result<GlyphBoundsEvidence, Failure>,
    pub state: InkStateAssertions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphInkUncertain {
    UnsupportedOrUnknownState,
    Glyph(Failure),
    MissingMapping,
    UnitsPerEm,
    InvalidNumber,
    InvalidBounds,
    Overflow,
}

/// Coordinates are integer millionths of a PDF-space unit, rounded outwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphInkBox {
    pub x_min: i128,
    pub y_min: i128,
    pub x_max: i128,
    pub y_max: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphInkObservation {
    NoOutline,
    UnclippedBounds(GlyphInkBox),
}

/// These limitations apply even when the numerical calculation succeeds.
pub const GLYPH_INK_UNVERIFIED: &[&str] = &[
    "caller assertions and captured tokens are not independently verified",
    "font resource identity, character mapping and actual invocation are not verified",
    "graphics state, transforms, clipping, hinting, rendering and saving are not verified",
    "this observation is not connected to a safety gate",
];

fn decimal(value: &str) -> Result<i128, GlyphInkUncertain> {
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    let (whole, fraction) = digits
        .split_once('.')
        .ok_or(GlyphInkUncertain::InvalidNumber)?;
    if whole.is_empty()
        || fraction.len() != 6
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(GlyphInkUncertain::InvalidNumber);
    }
    let mut result = 0i128;
    for digit in whole.bytes().chain(fraction.bytes()) {
        result = result
            .checked_mul(10)
            .and_then(|v| v.checked_add(i128::from(digit - b'0')))
            .ok_or(GlyphInkUncertain::Overflow)?;
    }
    if negative {
        result.checked_neg().ok_or(GlyphInkUncertain::Overflow)
    } else {
        Ok(result)
    }
}

// Within this range binary32 represents every integer and half-integer exactly.
// This checks the returned value, not whether its source was actually unrounded:
// that requires the caller's exact extraction assertion as well.
fn half_units(value: f32) -> Result<i128, GlyphInkUncertain> {
    if !value.is_finite() || value.abs() > 32768.0 {
        return Err(GlyphInkUncertain::InvalidBounds);
    }
    let doubled = value * 2.0;
    if doubled.fract() != 0.0 {
        return Err(GlyphInkUncertain::InvalidBounds);
    }
    Ok(doubled as i128)
}

fn edge(
    origin: i128,
    size: i128,
    half: i128,
    units: u16,
    upper: bool,
) -> Result<i128, GlyphInkUncertain> {
    let denominator = i128::from(units) * 2;
    let numerator = size.checked_mul(half).ok_or(GlyphInkUncertain::Overflow)?;
    let floor = numerator.div_euclid(denominator);
    let rounded = if upper && numerator.rem_euclid(denominator) != 0 {
        floor.checked_add(1).ok_or(GlyphInkUncertain::Overflow)?
    } else {
        floor
    };
    origin
        .checked_add(rounded)
        .ok_or(GlyphInkUncertain::Overflow)
}

/// Calculates a conditional enclosure, without inspecting the stream or font resource.
pub fn observe_glyph_ink(
    input: GlyphInkInput<'_>,
) -> Result<GlyphInkObservation, GlyphInkUncertain> {
    let state = input.state;
    if !(state.horizontal_unit_text_matrix
        && state.default_horizontal_scale_and_rise
        && state.fill_only_no_stroke
        && state.no_other_transform
        && state.simple_static_unhinted_truetype
        && state.exact_half_unit_extraction)
    {
        return Err(GlyphInkUncertain::UnsupportedOrUnknownState);
    }
    let glyph = input.glyph.map_err(GlyphInkUncertain::Glyph)?;
    if glyph.glyph_id == 0 {
        return Err(GlyphInkUncertain::MissingMapping);
    }
    if !(16..=16384).contains(&input.confirmed_units_per_em)
        || glyph.units_per_em != input.confirmed_units_per_em
    {
        return Err(GlyphInkUncertain::UnitsPerEm);
    }
    let x = decimal(input.x)?
        .checked_add(decimal(input.crop_translation_x)?)
        .ok_or(GlyphInkUncertain::Overflow)?;
    let y = decimal(input.y)?
        .checked_add(decimal(input.crop_translation_y)?)
        .ok_or(GlyphInkUncertain::Overflow)?;
    let size = decimal(input.font_size)?;
    if size <= 0 {
        return Err(GlyphInkUncertain::InvalidNumber);
    }
    let bounds = match glyph.observation {
        OutlineObservation::NoOutline => return Ok(GlyphInkObservation::NoOutline),
        OutlineObservation::Bounds(bounds) => bounds,
    };
    let (xmin, ymin, xmax, ymax) = (
        half_units(bounds.x_min)?,
        half_units(bounds.y_min)?,
        half_units(bounds.x_max)?,
        half_units(bounds.y_max)?,
    );
    if xmin > xmax || ymin > ymax {
        return Err(GlyphInkUncertain::InvalidBounds);
    }
    Ok(GlyphInkObservation::UnclippedBounds(GlyphInkBox {
        x_min: edge(x, size, xmin, glyph.units_per_em, false)?,
        y_min: edge(y, size, ymin, glyph.units_per_em, false)?,
        x_max: edge(x, size, xmax, glyph.units_per_em, true)?,
        y_max: edge(y, size, ymax, glyph.units_per_em, true)?,
    }))
}
