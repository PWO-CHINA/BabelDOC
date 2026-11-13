//! Serial typesetting on M IR. Geometry trials use private numeric caches.
//! Document navigation and writes go exclusively through generated access.
use crate::{StageContext, StageError};
use babeldoc_font_core::{FontFace, FontMapper, SourceFontStyle};
use babeldoc_il_types::schema::Box;
use babeldoc_ir_core::{self as ir, Cursor, FieldValue, TreeView, TypesettingView, types};
use log::{error, warn};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
const LOG_TARGET: &str = "babeldoc.format.pdf.document_il.midend.typesetting";
type View<'a> = TreeView<'a, TypesettingView>;
type P = Cursor<types::PdfParagraph>;
type Page = Cursor<types::Page>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypesettingFailure {
    MissingStyle,
    MissingFontId,
    MissingSourceFont,
    AmbiguousFontId,
    MissingGlyph,
    MissingParagraphBox,
    MissingContent,
    LayoutExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypesettingError {
    Failure { kind: TypesettingFailure, detail: String },
    Stage(StageError),
}

impl std::fmt::Display for TypesettingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failure { detail, .. } => formatter.write_str(detail),
            Self::Stage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TypesettingError {}

impl From<StageError> for TypesettingError {
    fn from(error: StageError) -> Self {
        if let Some(kind) = typesetting_failure(&error) {
            Self::Failure { kind, detail: error.to_string() }
        } else {
            Self::Stage(error)
        }
    }
}

impl TypesettingFailure {
    fn code(self) -> &'static str {
        match self {
            Self::MissingStyle => "missing_style",
            Self::MissingFontId => "missing_font_id",
            Self::MissingSourceFont => "missing_source_font",
            Self::AmbiguousFontId => "ambiguous_font_id",
            Self::MissingGlyph => "missing_glyph",
            Self::MissingParagraphBox => "missing_paragraph_box",
            Self::MissingContent => "missing_content",
            Self::LayoutExhausted => "layout_exhausted",
        }
    }
}

pub fn typesetting_failure(error: &StageError) -> Option<TypesettingFailure> {
    let StageError::Failed(message) = error else { return None; };
    let code = message.strip_prefix("typesetting:")?.split(':').next()?;
    [
        TypesettingFailure::MissingStyle,
        TypesettingFailure::MissingFontId,
        TypesettingFailure::MissingSourceFont,
        TypesettingFailure::AmbiguousFontId,
        TypesettingFailure::MissingGlyph,
        TypesettingFailure::MissingParagraphBox,
        TypesettingFailure::MissingContent,
        TypesettingFailure::LayoutExhausted,
    ].into_iter().find(|kind| kind.code() == code)
}

fn typesetting_failed(kind: TypesettingFailure, detail: impl std::fmt::Display) -> StageError {
    StageError::failed(format!("typesetting:{}: {detail}", kind.code()))
}

fn need<T>(v: Option<T>, s: &str) -> Result<T, StageError> {
    v.ok_or_else(|| StageError::failed(s))
}
fn read_box(c: Cursor<types::Box>, v: &View<'_>) -> Result<Box, StageError> {
    Ok(Box {
        x: c.x(v)?,
        y: c.y(v)?,
        x2: c.x2(v)?,
        y2: c.y2(v)?,
    })
}
fn box_field(b: Box) -> FieldValue<types::Box> {
    ir::BoxInit {
        x: b.x,
        y: b.y,
        x2: b.x2,
        y2: b.y2,
    }
    .into_value()
}
// These caches are not IR objects: identity is retained only by opaque selections.
#[derive(Clone, Default)]
struct PdfStyle {
    font_id: Option<String>,
    font_size: Option<f64>,
    graphic_state: Option<Cursor<types::GraphicState>>,
}
#[derive(Clone, Default)]
struct VisualBbox {
    r#box: Option<std::boxed::Box<Box>>,
}
#[derive(Clone, Default)]
struct PdfCharacter {
    original: Option<Cursor<types::PdfCharacter>>,
    source_identity: Option<u64>,
    r#box: Option<std::boxed::Box<Box>>,
    visual_bbox: Option<std::boxed::Box<VisualBbox>>,
    pdf_style: Option<std::boxed::Box<PdfStyle>>,
    pdf_character_id: Option<i64>,
    synthetic_spacing: Option<bool>,
    char_unicode: Option<String>,
    scale: Option<f64>,
    vertical: Option<bool>,
    advance: Option<f64>,
    debug_info: Option<bool>,
    xobj_id: Option<i64>,
}
#[derive(Clone, Default)]
struct PdfFormula {
    original: Option<Cursor<types::PdfFormula>>,
    source_identity: Option<u64>,
    r#box: Option<std::boxed::Box<Box>>,
    pdf_character: Vec<PdfCharacter>,
    pdf_curve: Vec<PdfCurve>,
    pdf_form: Vec<PdfForm>,
    x_offset: Option<f64>,
    y_offset: Option<f64>,
    x_advance: Option<f64>,
}
#[derive(Clone)]
struct PdfCurve {
    original: Cursor<types::PdfCurve>,
    r#box: Option<std::boxed::Box<Box>>,
    relocation_transform: Vec<babeldoc_il_types::PythonValue>,
}
#[derive(Clone)]
struct PdfForm {
    original: Cursor<types::PdfForm>,
    r#box: Option<std::boxed::Box<Box>>,
    relocation_transform: Vec<babeldoc_il_types::PythonValue>,
}
fn read_character(
    c: Cursor<types::PdfCharacter>,
    v: &mut View<'_>,
) -> Result<PdfCharacter, StageError> {
    let pdf_character_id = c.pdf_character_id(v)?;
    let synthetic_spacing = c.synthetic_spacing(v)?;
    let char_unicode = c.char_unicode(v)?.clone();
    if pdf_character_id.is_none()
        && !(synthetic_spacing == Some(true)
            && matches!(char_unicode.as_deref(), Some(" " | "\n")))
    {
        return Err(StageError::failed(format!(
            "typesetting.missing_character_id: object {}",
            c.identity()
        )));
    }
    let style = c
        .pdf_style(v)?
        .map(|s| {
            Ok::<_, StageError>(std::boxed::Box::new(PdfStyle {
                font_id: s.font_id(v)?.clone(),
                font_size: s.font_size(v)?,
                graphic_state: s.graphic_state(v)?.map(|g| g.retain(v)).transpose()?,
            }))
        })
        .transpose()?;
    Ok(PdfCharacter {
        r#box: c
            .r#box(v)?
            .map(|b| read_box(b, v).map(std::boxed::Box::new))
            .transpose()?,
        visual_bbox: c
            .visual_bbox(v)?
            .map(|b| {
                Ok::<_, StageError>(std::boxed::Box::new(VisualBbox {
                    r#box: b
                        .r#box(v)?
                        .map(|b| read_box(b, v).map(std::boxed::Box::new))
                        .transpose()?,
                }))
            })
            .transpose()?,
        pdf_style: style,
        pdf_character_id,
        synthetic_spacing,
        char_unicode,
        scale: c.scale(v)?,
        vertical: c.vertical(v)?,
        advance: c.advance(v)?,
        debug_info: c.debug_info(v)?,
        xobj_id: c.xobj_id(v)?,
        original: Some(c.retain(v)?),
        source_identity: Some(c.identity()),
    })
}
fn read_formula(f: Cursor<types::PdfFormula>, v: &mut View<'_>) -> Result<PdfFormula, StageError> {
    let original = f.retain(v)?;
    let mut characters = Vec::with_capacity(f.pdf_character_len(v)?);
    for index in 0..f.pdf_character_len(v)? {
        characters.push(read_character(f.pdf_character_at(v, index)?, v)?);
    }
    let mut curves = Vec::with_capacity(f.pdf_curve_len(v)?);
    for index in 0..f.pdf_curve_len(v)? {
        let c = f.pdf_curve_at(v, index)?;
        curves.push(PdfCurve {
            r#box: c
                .r#box(v)?
                .map(|b| read_box(b, v).map(std::boxed::Box::new))
                .transpose()?,
            relocation_transform: c.relocation_transform(v)?.clone(),
            original: c.retain(v)?,
        });
    }
    let mut forms = Vec::with_capacity(f.pdf_form_len(v)?);
    for index in 0..f.pdf_form_len(v)? {
        let c = f.pdf_form_at(v, index)?;
        forms.push(PdfForm {
            r#box: c
                .r#box(v)?
                .map(|b| read_box(b, v).map(std::boxed::Box::new))
                .transpose()?,
            relocation_transform: c.relocation_transform(v)?.clone(),
            original: c.retain(v)?,
        });
    }
    Ok(PdfFormula {
        r#box: f
            .r#box(v)?
            .map(|b| read_box(b, v).map(std::boxed::Box::new))
            .transpose()?,
        pdf_character: characters,
        pdf_curve: curves,
        pdf_form: forms,
        x_offset: f.x_offset(v)?,
        y_offset: f.y_offset(v)?,
        x_advance: f.x_advance(v)?,
        original: Some(original),
        source_identity: Some(original.identity()),
    })
}
fn char_value(
    c: PdfCharacter,
    v: &mut View<'_>,
) -> Result<FieldValue<types::PdfCharacter>, StageError> {
    if let Some(original) = c.original {
        return Ok(original.share(v)?);
    }
    Ok(ir::PdfCharacterInit {
        r#box: c.r#box.map(|b| box_field(*b)),
        visual_bbox: c.visual_bbox.map(|b| {
            ir::VisualBboxInit {
                r#box: b.r#box.map(|b| box_field(*b)),
            }
            .into_value()
        }),
        pdf_style: c
            .pdf_style
            .map(|s| {
                Ok::<_, StageError>(
                    ir::PdfStyleInit {
                        font_id: s.font_id,
                        font_size: s.font_size,
                        graphic_state: s.graphic_state.map(|g| g.share(v)).transpose()?,
                    }
                    .into_value(),
                )
            })
            .transpose()?,
        pdf_character_id: c.pdf_character_id,
        synthetic_spacing: c.synthetic_spacing,
        char_unicode: c.char_unicode,
        scale: c.scale,
        vertical: c.vertical,
        advance: c.advance,
        debug_info: c.debug_info,
        xobj_id: c.xobj_id,
        ..Default::default()
    }
    .into_value())
}
fn curve_value(c: PdfCurve, v: &mut View<'_>) -> Result<FieldValue<types::PdfCurve>, StageError> {
    let mut n = c.original.copy_shallow(v)?;
    n.r#box = c.r#box.map(|b| box_field(*b));
    n.relocation_transform = c.relocation_transform;
    Ok(n.into_value())
}
fn form_value(c: PdfForm, v: &mut View<'_>) -> Result<FieldValue<types::PdfForm>, StageError> {
    let mut n = c.original.copy_shallow(v)?;
    n.r#box = c.r#box.map(|b| box_field(*b));
    n.relocation_transform = c.relocation_transform;
    Ok(n.into_value())
}
fn update_formula_data(f: &mut PdfFormula) -> Result<(), StageError> {
    let ignored = |c: &PdfCharacter| {
        c.pdf_character_id.is_none()
            || crate::shared::layout_helper::HEIGHT_NOT_USFUL_CHAR_IN_CHAR
                .iter()
                .any(|s| s.is_some_and(|s| c.char_unicode.as_deref() == Some(s)))
    };
    if f.pdf_character.is_empty() {
        return Err(StageError::failed("min() iterable argument is empty"));
    }
    let eligible = f.pdf_character.iter().any(|c| !ignored(c));
    let (mut x, mut y, mut x2, mut y2) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for c in &f.pdf_character {
        let (a, b, aa, bb) = box_coordinates(need(
            c.visual_bbox.as_deref().and_then(|v| v.r#box.as_deref()),
            "formula character has no visual box",
        )?)?;
        x = x.min(a);
        x2 = x2.max(aa);
        if !eligible || !ignored(c) {
            y = y.min(b);
            y2 = y2.max(bb)
        }
    }
    f.r#box = Some(std::boxed::Box::new(Box {
        x: Some(x),
        y: Some(y),
        x2: Some(x2),
        y2: Some(y2),
    }));
    for a in [&mut f.x_offset, &mut f.y_offset, &mut f.x_advance] {
        if a.unwrap_or_default() == 0. {
            *a = Some(0.)
        }
    }
    Ok(())
}
const LINE_BREAK_PATTERN: &str = r"^[a-zA-Z0-9\x{00c0}-\x{00ff}\x{0100}-\x{017f}\x{0180}-\x{024f}\x{1e00}-\x{1eff}\x{2c60}-\x{2c7f}\x{a720}-\x{a7ff}\x{ab30}-\x{ab6f}\x{0250}-\x{02a0}\x{0400}-\x{04ff}\x{0300}-\x{036f}\x{0500}-\x{052f}\x{0370}-\x{03ff}\x{2de0}-\x{2dff}\x{a650}-\x{a69f}\x{1200}-\x{137f}\x{1380}-\x{139f}\x{2d80}-\x{2ddf}\x{ab00}-\x{ab2f}\x{1e7e0}-\x{1e7ff}\x{0e80}-\x{0eff}\x{0d00}-\x{0d7f}\x{0a80}-\x{0aff}\x{0e00}-\x{0e7f}\x{1000}-\x{109f}\x{aa60}-\x{aa7f}\x{a9e0}-\x{a9ff}\x{116d0}-\x{116ff}\x{0b80}-\x{0bff}\x{0c00}-\x{0c7f}\x{0b00}-\x{0b7f}\x{0530}-\x{058f}\x{10a0}-\x{10ff}\x{1c90}-\x{1cbf}\x{2d00}-\x{2d2f}\x{1780}-\x{17ff}\x{19e0}-\x{19ff}\x{10b00}-\x{10b3f}\x{1d00}-\x{1d7f}\x{1400}-\x{167f}\x{0b00}-\x{0b7f}\x{0780}-\x{07bf}\x{1e900}-\x{1e95f}\x{1c80}-\x{1c8f}\x{1e030}-\x{1e08f}\x{a000}-\x{a48f}\x{a490}-\x{a4cf}'-·ʻ]+$";
#[derive(Clone)]
struct TypesettingUnit {
    pub character: Option<PdfCharacter>,
    pub formula: Option<PdfFormula>,
    pub unicode: Option<String>,
    pub font: Option<Arc<FontFace>>,
    pub original_font: Option<()>,
    pub font_size: Option<f64>,
    pub style: Option<PdfStyle>,
    pub xobj_id: Option<i64>,
    pub debug_info: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub scale: Option<f64>,
    inferred_source_space: bool,
    box_cache: Option<Box>,
    width_cache: Option<f64>,
    height_cache: Option<f64>,
    can_break_line_cache: Option<bool>,
    is_cjk_char_cache: Option<bool>,
    mixed_character_blacklist_cache: Option<bool>,
    is_space_cache: Option<bool>,
    is_hung_punctuation_cache: Option<bool>,
    cannot_end_cache: Option<bool>,
    can_passthrough_cache: Option<bool>,
}

impl TypesettingUnit {
    pub fn from_character(character: PdfCharacter, debug_info: bool) -> Self {
        Self::empty(
            Some(character),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            debug_info,
        )
    }
    pub fn from_formula(formula: PdfFormula) -> Self {
        Self::empty(
            None,
            Some(formula),
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
    }
    fn from_unicode(
        unicode: String,
        font: Arc<FontFace>,
        original_font: Option<()>,
        font_size: f64,
        style: PdfStyle,
        xobj_id: Option<i64>,
        debug_info: bool,
    ) -> Self {
        Self::empty(
            None,
            None,
            Some(unicode),
            Some(font),
            original_font,
            Some(font_size),
            Some(style),
            xobj_id,
            debug_info,
        )
    }
    fn empty(
        character: Option<PdfCharacter>,
        formula: Option<PdfFormula>,
        unicode: Option<String>,
        font: Option<Arc<FontFace>>,
        original_font: Option<()>,
        font_size: Option<f64>,
        style: Option<PdfStyle>,
        xobj_id: Option<i64>,
        debug_info: bool,
    ) -> Self {
        Self {
            character,
            formula,
            unicode,
            font,
            original_font,
            font_size,
            style,
            xobj_id,
            debug_info,
            x: None,
            y: None,
            scale: None,
            inferred_source_space: false,
            box_cache: None,
            width_cache: None,
            height_cache: None,
            can_break_line_cache: None,
            is_cjk_char_cache: None,
            mixed_character_blacklist_cache: None,
            is_space_cache: None,
            is_hung_punctuation_cache: None,
            cannot_end_cache: None,
            can_passthrough_cache: None,
        }
    }
    pub fn try_get_unicode(&self) -> Option<&str> {
        if let Some(ch) = &self.character {
            ch.char_unicode.as_deref()
        } else if self.formula.is_some() {
            None
        } else {
            self.unicode.as_deref()
        }
    }
    fn source_word_wrap(left: &PdfCharacter, right: &PdfCharacter) -> bool {
        let latin = |text: Option<&str>| text.is_some_and(|text| {
            !text.is_empty() && text.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || ('\u{00c0}'..='\u{024f}').contains(&character)
                    || ('\u{fb00}'..='\u{fb06}').contains(&character)
            })
        });
        if !latin(left.char_unicode.as_deref()) || !latin(right.char_unicode.as_deref())
            || left.synthetic_spacing == Some(true) || right.synthetic_spacing == Some(true)
            || left.vertical.unwrap_or(false) || right.vertical.unwrap_or(false)
            || left.xobj_id != right.xobj_id
        {
            return false;
        }
        let (Some(first), Some(second), Some(first_style), Some(second_style)) = (
            left.r#box.as_deref(), right.r#box.as_deref(), left.pdf_style.as_deref(), right.pdf_style.as_deref(),
        ) else { return false; };
        let (Some(first_x), Some(first_y), Some(first_x2), Some(first_y2),
             Some(second_x), Some(second_y), Some(second_x2), Some(second_y2),
             Some(size), Some(second_size)) = (
            first.x, first.y, first.x2, first.y2, second.x, second.y, second.x2, second.y2,
            first_style.font_size, second_style.font_size,
        ) else { return false; };
        let values = [first_x, first_y, first_x2, first_y2, second_x, second_y, second_x2, second_y2, size, second_size];
        values.iter().all(|value| value.is_finite())
            && size > 0.0 && second_size > 0.0
            && first_x2 > first_x && second_x2 > second_x && first_y2 > first_y && second_y2 > second_y
            && first_style.font_id.is_some() && first_style.font_id == second_style.font_id
            && (size / second_size - 1.0).abs() <= 0.05
            && matches!(left.scale, None | Some(1.0)) && matches!(right.scale, None | Some(1.0))
            && (0.55..=2.5).contains(&((first_y - second_y) / size))
            && second_y2 <= first_y + size * 0.15
            && first_x2 - second_x >= size * 2.0
    }
    fn source_word_space(left: &PdfCharacter) -> TypesettingUnit {
        let style = left.pdf_style.clone();
        let size = style.as_deref().and_then(|style| style.font_size).unwrap_or_default();
        let mut space = TypesettingUnit::from_character(PdfCharacter {
            r#box: Some(std::boxed::Box::new(Box {
                x: Some(0.0), y: Some(0.0), x2: Some(size * 0.25), y2: Some(size),
            })),
            pdf_style: style,
            synthetic_spacing: Some(true),
            char_unicode: Some(" ".into()),
            advance: Some(size * 0.25),
            xobj_id: left.xobj_id,
            ..Default::default()
        }, false);
        space.inferred_source_space = true;
        space
    }
    fn cached<T: Copy>(cache: &mut Option<T>, value: impl FnOnce() -> T) -> T {
        *cache.get_or_insert_with(value)
    }
    pub fn calc_mixed_character_blacklist(&self) -> bool {
        matches!(
            self.try_get_unicode(),
            Some("。" | "，" | "：" | "？" | "！")
        )
    }
    pub fn mixed_character_blacklist(&mut self) -> bool {
        let value = self.calc_mixed_character_blacklist();
        Self::cached(&mut self.mixed_character_blacklist_cache, || value)
    }
    pub fn calc_can_break_line(&self) -> bool {
        let Some(text) = self.try_get_unicode() else {
            return true;
        };
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        !RE.get_or_init(|| Regex::new(LINE_BREAK_PATTERN).expect("literal line-break regex"))
            .is_match(text)
    }
    pub fn can_break_line(&mut self) -> bool {
        let value = self.calc_can_break_line();
        Self::cached(&mut self.can_break_line_cache, || value)
    }
    pub fn calc_is_cjk_char(&self) -> bool {
        if self.formula.is_some() {
            return false;
        }
        let Some(text) = self.try_get_unicode() else {
            return false;
        };
        if text.contains("(cid") || text.chars().count() != 1 {
            return false;
        }
        let ch = text.chars().next().unwrap();
        if "（）【】《》〔〕〈〉〖〗「」『』、。：？！，".contains(ch) {
            return true;
        }
        let cp = ch as u32;
        (0x3000..=0x303f).contains(&cp)
            || (0x3040..=0x309f).contains(&cp)
            || (0x30a0..=0x30ff).contains(&cp)
            || (0x3100..=0x312f).contains(&cp)
            || (0xac00..=0xd7af).contains(&cp)
            || (0x1100..=0x11ff).contains(&cp)
            || (0x3130..=0x318f).contains(&cp)
            || (0xa960..=0xa97f).contains(&cp)
            || (0xd7b0..=0xd7ff).contains(&cp)
            || (0x3190..=0x319f).contains(&cp)
            || (0x3200..=0x32ff).contains(&cp)
            || (0x3300..=0x33ff).contains(&cp)
            || (0x3400..=0x4dbf).contains(&cp)
            || (0x4e00..=0x9fff).contains(&cp)
            || (0x20000..=0x2a6df).contains(&cp)
            || (0x2a700..=0x2b73f).contains(&cp)
            || (0x2b740..=0x2b81f).contains(&cp)
            || (0x2b820..=0x2ceaf).contains(&cp)
            || (0x2ceb0..=0x2ebef).contains(&cp)
            || (0x2ebf0..=0x2ee5f).contains(&cp)
            || (0x30000..=0x3134f).contains(&cp)
            || (0x31350..=0x323af).contains(&cp)
            || (0xff01..=0xff60).contains(&cp)
            || (0xffe0..=0xffe6).contains(&cp)
            || (0x2e80..=0x2eff).contains(&cp)
            || (0x31c0..=0x31ef).contains(&cp)
            || (0x2f00..=0x2fdf).contains(&cp)
            || (0xfe10..=0xfe1f).contains(&cp)
            || (0xfe30..=0xfe4f).contains(&cp)
    }
    pub fn is_cjk_char(&mut self) -> bool {
        let value = self.calc_is_cjk_char();
        Self::cached(&mut self.is_cjk_char_cache, || value)
    }
    pub fn calc_is_space(&self) -> bool {
        self.formula.is_none() && self.try_get_unicode() == Some(" ")
    }
    pub fn is_space(&mut self) -> bool {
        let value = self.calc_is_space();
        Self::cached(&mut self.is_space_cache, || value)
    }
    pub fn calc_is_hung_punctuation(&self) -> bool {
        self.formula.is_none()
            && self.try_get_unicode().is_some_and(|s| {
                matches!(
                    s,
                    "," | "."
                        | ":"
                        | ";"
                        | "?"
                        | "!"
                        | "，"
                        | "。"
                        | "．"
                        | "、"
                        | "："
                        | "；"
                        | "！"
                        | "‼"
                        | "？"
                        | "⁇"
                        | "”"
                        | "’"
                        | "」"
                        | "』"
                        | ")"
                        | "]"
                        | "}"
                        | "）"
                        | "〕"
                        | "〉"
                        | "】"
                        | "〗"
                        | "］"
                        | "｝"
                        | "》"
                        | "～"
                        | "-"
                        | "–"
                        | "—"
                        | "·"
                        | "・"
                        | "‧"
                        | "/"
                        | "／"
                        | "⁄"
                )
            })
    }
    pub fn is_hung_punctuation(&mut self) -> bool {
        let value = self.calc_is_hung_punctuation();
        Self::cached(&mut self.is_hung_punctuation_cache, || value)
    }
    pub fn calc_is_cannot_appear_in_line_end_punctuation(&self) -> bool {
        self.formula.is_none()
            && self.try_get_unicode().is_some_and(|s| {
                matches!(
                    s,
                    "“" | "‘"
                        | "「"
                        | "『"
                        | "("
                        | "["
                        | "{"
                        | "（"
                        | "〔"
                        | "〈"
                        | "《"
                        | "〖"
                        | "〘"
                        | "〚"
                )
            })
    }
    pub fn is_cannot_appear_in_line_end_punctuation(&mut self) -> bool {
        let value = self.calc_is_cannot_appear_in_line_end_punctuation();
        Self::cached(&mut self.cannot_end_cache, || value)
    }
    pub fn calc_can_passthrough(&self) -> bool {
        self.unicode.is_none()
    }
    pub fn can_passthrough(&mut self) -> bool {
        let value = self.calc_can_passthrough();
        Self::cached(&mut self.can_passthrough_cache, || value)
    }
    pub fn box_value(&mut self) -> Result<Box, StageError> {
        if let Some(b) = &self.box_cache {
            return Ok(b.clone());
        }
        let b = if let Some(ch) = &self.character {
            let mut b = ch
                .r#box
                .as_deref()
                .cloned()
                .ok_or_else(|| StageError::Failed("character has no box".into()))?;
            if let Some(v) = ch.visual_bbox.as_deref().and_then(|v| v.r#box.as_deref()) {
                b.y = v.y;
                b.y2 = v.y2;
            }
            b
        } else if let Some(f) = &self.formula {
            f.r#box
                .as_deref()
                .cloned()
                .ok_or_else(|| StageError::Failed("formula has no box".into()))?
        } else {
            let width = self
                .font
                .as_ref()
                .and_then(|f| {
                    f.char_lengths(
                        self.unicode.as_deref().unwrap_or_default(),
                        self.font_size.unwrap_or_default(),
                    )
                    .first()
                    .copied()
                })
                .ok_or_else(|| StageError::Failed("font char_lengths returned no widths".into()))?;
            if self.x.is_none() || self.y.is_none() || self.scale.is_none() {
                Box {
                    x: Some(0.0),
                    y: Some(0.0),
                    x2: Some(width),
                    y2: self.font_size,
                }
            } else {
                let x = self.x.expect("checked");
                let y = self.y.expect("checked");
                Box {
                    x: Some(x),
                    y: Some(y),
                    x2: Some(x + width),
                    y2: Some(y + self.font_size.unwrap_or_default()),
                }
            }
        };
        self.box_cache = Some(b.clone());
        Ok(b)
    }
    pub fn width(&mut self) -> Result<f64, StageError> {
        if let Some(v) = self.width_cache {
            return Ok(v);
        }
        let b = self.box_value()?;
        let (x, _, x2, _) = box_coordinates(&b)?;
        let v = x2 - x;
        self.width_cache = Some(v);
        Ok(v)
    }
    pub fn height(&mut self) -> Result<f64, StageError> {
        if let Some(v) = self.height_cache {
            return Ok(v);
        }
        let b = self.box_value()?;
        let (_, y, _, y2) = box_coordinates(&b)?;
        let v = y2 - y;
        self.height_cache = Some(v);
        Ok(v)
    }
    pub fn try_resue_cache(&mut self, old: &TypesettingUnit) {
        self.is_cjk_char_cache = old.is_cjk_char_cache;
        self.can_break_line_cache = old.can_break_line_cache;
        self.is_space_cache = old.is_space_cache;
        self.is_hung_punctuation_cache = old.is_hung_punctuation_cache;
        self.cannot_end_cache = old.cannot_end_cache;
        self.can_passthrough_cache = old.can_passthrough_cache;
        self.mixed_character_blacklist_cache = old.mixed_character_blacklist_cache;
    }
    pub fn passthrough(&self) -> (Vec<PdfCharacter>, Vec<PdfCurve>, Vec<PdfForm>) {
        if let Some(c) = &self.character {
            (vec![c.clone()], vec![], vec![])
        } else if let Some(f) = &self.formula {
            (
                f.pdf_character.clone(),
                f.pdf_curve.clone(),
                f.pdf_form.clone(),
            )
        } else {
            error!(target: LOG_TARGET, "Cannot passthrough unicode. TypesettingUnit: {self}. ");
            error!(target: LOG_TARGET, "Cannot passthrough unicode. TypesettingUnit: {self}. ");
            (vec![], vec![], vec![])
        }
    }
    pub fn relocate(
        &mut self,
        x: f64,
        y: f64,
        scale: f64,
        debug_info: bool,
    ) -> Result<TypesettingUnit, StageError> {
        if self.character.is_some() {
            let width = self.width()?;
            let height = self.height()?;
            let ch = self.character.as_ref().unwrap();
            let source_style = ch.pdf_style.as_deref().ok_or_else(|| {
                StageError::Failed("'NoneType' object has no attribute 'font_id'".into())
            })?;
            let source_size = source_style.font_size.ok_or_else(|| {
                StageError::Failed(
                    "unsupported operand type(s) for *: 'NoneType' and 'float'".into(),
                )
            })?;
            let new = PdfCharacter {
                pdf_character_id: ch.pdf_character_id,
                synthetic_spacing: ch.synthetic_spacing,
                char_unicode: ch.char_unicode.clone(),
                r#box: Some(std::boxed::Box::new(Box {
                    x: Some(x),
                    y: Some(y),
                    x2: Some(x + width * scale),
                    y2: Some(y + height * scale),
                })),
                pdf_style: Some(std::boxed::Box::new(PdfStyle {
                    font_id: source_style.font_id.clone(),
                    font_size: Some(source_size * scale),
                    graphic_state: source_style.graphic_state.clone(),
                })),
                scale: Some(scale),
                vertical: ch.vertical,
                advance: ch.advance.filter(|v| *v != 0.0).map(|v| v * scale),
                debug_info: Some(debug_info),
                xobj_id: ch.xobj_id,
                ..Default::default()
            };
            let mut u = TypesettingUnit::from_character(new, debug_info);
            u.try_resue_cache(self);
            Ok(u)
        } else if let Some(f) = &self.formula {
            let fx = f
                .r#box
                .as_deref()
                .and_then(|b| b.x)
                .ok_or_else(|| StageError::Failed("formula box has no x".into()))?;
            let fy = f
                .r#box
                .as_deref()
                .and_then(|b| b.y)
                .ok_or_else(|| StageError::Failed("formula box has no y".into()))?;
            let xo = f.x_offset.ok_or_else(|| {
                StageError::Failed(
                    "unsupported operand type(s) for +: 'float' and 'NoneType'".into(),
                )
            })?;
            let yo = f.y_offset.ok_or_else(|| {
                StageError::Failed(
                    "unsupported operand type(s) for +: 'float' and 'NoneType'".into(),
                )
            })?;
            let x_advance = f.x_advance.ok_or_else(|| {
                StageError::Failed(
                    "unsupported operand type(s) for *: 'NoneType' and 'float'".into(),
                )
            })?;
            let mut chars = Vec::new();
            for ch in &f.pdf_character {
                let b = ch
                    .r#box
                    .as_deref()
                    .ok_or_else(|| StageError::Failed("formula character has no box".into()))?;
                let vb = ch
                    .visual_bbox
                    .as_deref()
                    .and_then(|v| v.r#box.as_deref())
                    .ok_or_else(|| {
                        StageError::Failed("formula character has no visual box".into())
                    })?;
                let (bx, by, bx2, by2) = box_coordinates(b)?;
                let (vx, vy, vx2, vy2) = box_coordinates(vb)?;
                let source_style = ch.pdf_style.as_deref().ok_or_else(|| {
                    StageError::Failed("'NoneType' object has no attribute 'font_id'".into())
                })?;
                let source_size = source_style.font_size.ok_or_else(|| {
                    StageError::Failed(
                        "unsupported operand type(s) for *: 'NoneType' and 'float'".into(),
                    )
                })?;
                let relocated_box = Box {
                    x: Some(x + (bx - fx + xo) * scale),
                    y: Some(y + (by - fy + yo) * scale),
                    x2: Some(x + (bx - fx + (bx2 - bx) + xo) * scale),
                    y2: Some(y + (by - fy + (by2 - by) + yo) * scale),
                };
                let visual_box = Box {
                    x: Some(x + (vx - fx + xo) * scale),
                    y: Some(y + (vy - fy + yo) * scale),
                    x2: Some(x + (vx - fx + (vx2 - vx) + xo) * scale),
                    y2: Some(y + (vy - fy + (vy2 - vy) + yo) * scale),
                };
                let nc = PdfCharacter {
                    source_identity: ch.source_identity,
                    pdf_character_id: ch.pdf_character_id,
                    synthetic_spacing: ch.synthetic_spacing,
                    char_unicode: ch.char_unicode.clone(),
                    r#box: Some(std::boxed::Box::new(relocated_box)),
                    visual_bbox: Some(std::boxed::Box::new(VisualBbox {
                        r#box: Some(std::boxed::Box::new(visual_box)),
                    })),
                    pdf_style: Some(std::boxed::Box::new(PdfStyle {
                        font_id: source_style.font_id.clone(),
                        font_size: Some(source_size * scale),
                        graphic_state: source_style.graphic_state.clone(),
                    })),
                    scale: Some(scale),
                    vertical: ch.vertical,
                    advance: ch.advance.filter(|v| *v != 0.0).map(|v| v * scale),
                    xobj_id: ch.xobj_id,
                    ..Default::default()
                };
                chars.push(nc);
            }
            let (minx, miny, maxx, maxy) = bounds(
                chars
                    .iter()
                    .filter_map(|c| c.visual_bbox.as_deref()?.r#box.as_deref()),
            )?;
            let mut nf = PdfFormula {
                source_identity: f.source_identity,
                r#box: Some(std::boxed::Box::new(Box {
                    x: Some(minx),
                    y: Some(miny),
                    x2: Some(maxx),
                    y2: Some(maxy),
                })),
                pdf_character: chars,
                x_offset: Some(xo * scale),
                y_offset: Some(yo * scale),
                x_advance: Some(x_advance * scale),
                ..Default::default()
            };
            for curve in &f.pdf_curve {
                nf.pdf_curve
                    .push(relocate_curve(curve, fx, fy, x, y, scale, xo, yo)?);
            }
            for form in &f.pdf_form {
                nf.pdf_form
                    .push(relocate_form(form, fx, fy, x, y, scale, xo, yo)?);
            }
            update_formula_data(&mut nf).map_err(|e| StageError::Failed(e.to_string()))?;
            let mut u = TypesettingUnit::from_formula(nf);
            u.try_resue_cache(self);
            Ok(u)
        } else {
            let mut u = TypesettingUnit::from_unicode(
                self.unicode.clone().unwrap_or_default(),
                self.font
                    .clone()
                    .ok_or_else(|| StageError::Failed("unicode unit has no mapped font".into()))?,
                self.original_font.clone(),
                self.font_size.unwrap_or_default() * scale,
                self.style
                    .clone()
                    .ok_or_else(|| StageError::Failed("unicode unit has no style".into()))?,
                self.xobj_id,
                self.debug_info,
            );
            u.x = Some(x);
            u.y = Some(y);
            u.scale = Some(scale);
            u.try_resue_cache(self);
            Ok(u)
        }
    }
    fn boundary_space(&self, x: f64, y: f64, height: f64) -> TypesettingUnit {
        let mut space = TypesettingUnit::from_character(PdfCharacter {
            r#box: Some(std::boxed::Box::new(Box {
                x: Some(x), y: Some(y), x2: Some(x), y2: Some(y + height),
            })),
            pdf_style: self.character.as_ref().and_then(|character| character.pdf_style.clone())
                .or_else(|| self.style.clone().map(std::boxed::Box::new)),
            synthetic_spacing: Some(true),
            char_unicode: Some(" ".into()),
            advance: Some(0.0),
            xobj_id: self.xobj_id.or_else(|| self.character.as_ref().and_then(|character| character.xobj_id)),
            ..Default::default()
        }, self.debug_info);
        space.inferred_source_space = self.inferred_source_space;
        space
    }
    pub fn render(
        &mut self,
    ) -> Result<(Vec<PdfCharacter>, Vec<PdfCurve>, Vec<PdfForm>), StageError> {
        if self.can_passthrough() {
            return Ok(self.passthrough());
        }
        let x = self.x.ok_or_else(|| {
            StageError::Failed("x position must be set, should be set by relocate".into())
        })?;
        let y = self.y.ok_or_else(|| {
            StageError::Failed("y position must be set, should be set by relocate".into())
        })?;
        let scale = self.scale.ok_or_else(|| {
            StageError::Failed("scale must be set, should be set by relocate".into())
        })?;
        let width = self.width()?;
        let size = self.font_size.unwrap_or_default();
        let font = self
            .font
            .as_ref()
            .ok_or_else(|| StageError::Failed("unicode unit has no mapped font".into()))?;
        Ok((
            vec![PdfCharacter {
                pdf_character_id: Some(
                    font.has_glyph(
                        self.unicode
                            .as_deref()
                            .unwrap_or_default()
                            .chars()
                            .next()
                            .unwrap_or('\0') as u32,
                    ) as i64,
                ),
                char_unicode: self.unicode.clone(),
                r#box: Some(std::boxed::Box::new(Box {
                    x: Some(x),
                    y: Some(y),
                    x2: Some(x + width),
                    y2: Some(y + size),
                })),
                pdf_style: Some(std::boxed::Box::new(PdfStyle {
                    font_id: Some(font.id().to_owned()),
                    font_size: Some(size),
                    graphic_state: self.style.as_ref().and_then(|s| s.graphic_state.clone()),
                })),
                scale: Some(scale),
                vertical: Some(false),
                advance: Some(width),
                xobj_id: self.xobj_id,
                debug_info: Some(self.debug_info),
                ..Default::default()
            }],
            vec![],
            vec![],
        ))
    }
}

impl std::fmt::Display for TypesettingUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.try_get_unicode().unwrap_or_default())
    }
}

fn bounds<'a>(boxes: impl Iterator<Item = &'a Box>) -> Result<(f64, f64, f64, f64), StageError> {
    let mut b = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let mut n = 0;
    for x in boxes {
        b.0 = b.0.min(x.x.unwrap_or_default());
        b.1 = b.1.min(x.y.unwrap_or_default());
        b.2 = b.2.max(x.x2.unwrap_or_default());
        b.3 = b.3.max(x.y2.unwrap_or_default());
        n += 1
    }
    if n == 0 {
        Err(StageError::Failed("min() arg is an empty sequence".into()))
    } else {
        Ok(b)
    }
}

fn box_coordinates(box_: &Box) -> Result<(f64, f64, f64, f64), StageError> {
    Ok((
        box_.x
            .ok_or_else(|| StageError::Failed("box.x is None".into()))?,
        box_.y
            .ok_or_else(|| StageError::Failed("box.y is None".into()))?,
        box_.x2
            .ok_or_else(|| StageError::Failed("box.x2 is None".into()))?,
        box_.y2
            .ok_or_else(|| StageError::Failed("box.y2 is None".into()))?,
    ))
}

fn same_box(left: &Box, right: &Box) -> bool {
    left.x == right.x && left.y == right.y && left.x2 == right.x2 && left.y2 == right.y2
}
fn relocation_matrix(tx: f64, ty: f64, s: f64) -> Vec<babeldoc_il_types::PythonValue> {
    [s, 0.0, 0.0, s, tx, ty]
        .into_iter()
        .map(|v| babeldoc_il_types::PythonValue::from(v))
        .collect()
}
fn relocate_curve(
    c: &PdfCurve,
    fx: f64,
    fy: f64,
    x: f64,
    y: f64,
    s: f64,
    xo: f64,
    yo: f64,
) -> Result<PdfCurve, StageError> {
    let mut n = c.clone();
    if let Some(b) = c.r#box.as_deref() {
        n.r#box = Some(std::boxed::Box::new(Box {
            x: Some(x + (b.x.unwrap_or_default() - fx + xo) * s),
            y: Some(y + (b.y.unwrap_or_default() - fy + yo) * s),
            x2: Some(
                x + (b.x.unwrap_or_default() - fx
                    + (b.x2.unwrap_or_default() - b.x.unwrap_or_default())
                    + xo)
                    * s,
            ),
            y2: Some(
                y + (b.y.unwrap_or_default() - fy
                    + (b.y2.unwrap_or_default() - b.y.unwrap_or_default())
                    + yo)
                    * s,
            ),
        }));
    }
    let tx = x + xo * s - fx * s;
    let ty = y + yo * s - fy * s;
    n.relocation_transform = relocation_matrix(tx, ty, s);
    Ok(n)
}
fn relocate_form(
    f: &PdfForm,
    fx: f64,
    fy: f64,
    x: f64,
    y: f64,
    s: f64,
    xo: f64,
    yo: f64,
) -> Result<PdfForm, StageError> {
    let mut n = f.clone();
    if let Some(b) = f.r#box.as_deref() {
        n.r#box = Some(std::boxed::Box::new(Box {
            x: Some(x + (b.x.unwrap_or_default() - fx + xo) * s),
            y: Some(y + (b.y.unwrap_or_default() - fy + yo) * s),
            x2: Some(
                x + (b.x.unwrap_or_default() - fx
                    + (b.x2.unwrap_or_default() - b.x.unwrap_or_default())
                    + xo)
                    * s,
            ),
            y2: Some(
                y + (b.y.unwrap_or_default() - fy
                    + (b.y2.unwrap_or_default() - b.y.unwrap_or_default())
                    + yo)
                    * s,
            ),
        }));
    }
    let tx = x + xo * s - fx * s;
    let ty = y + yo * s - fy * s;
    n.relocation_transform = relocation_matrix(tx, ty, s);
    Ok(n)
}

// Python font selection calls bool() on these runtime bool/int fields.
fn runtime_font_flag(value: &Option<babeldoc_il_types::RuntimeBool>) -> bool {
    match value {
        Some(babeldoc_il_types::RuntimeBool::Bool(value)) => *value,
        Some(babeldoc_il_types::RuntimeBool::Int(value)) => *value != 0,
        None => false,
    }
}
#[derive(Default)]
struct FontTable {
    fonts: HashMap<(Option<i64>, String), SourceFontStyle>,
    xobjects: HashSet<i64>,
}
pub struct Typesetting {
    mapper: Arc<FontMapper>,
    is_cjk: bool,
    progress: bool,
    font_source_styles: HashMap<String, SourceFontStyle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewRejection {
    TargetMissing,
    TypesettingFailed,
    CaptureFailed,
    MissingGlyph,
    InvalidGlyphEvidence,
    InvalidSpacingEvidence,
    GlyphResourceUnavailable,
    GlyphMismatch,
    TextEvidenceUnavailable,
    TextMismatch,
    MissingOrInvalidTargetBounds,
    OutsideOriginalTargetBounds,
    FormulaIdentityUnavailable,
    FormulaIdentityMismatch,
    InvalidFormulaEvidence,
    ObstacleAndClipEvidenceUnavailable,
}

#[derive(Clone, Debug)]
pub struct PreviewGlyph {
    pub text: Option<String>,
    pub glyph_id: Option<i64>,
    pub font_size: Option<f64>,
    pub font_id: Option<String>,
    pub xobj_id: Option<i64>,
    pub bounds: Option<crate::layout_audit::Bounds>,
}

#[derive(Clone, Debug)]
pub enum PreviewUnit {
    Glyph(PreviewGlyph),
    Spacing { bounds: Option<crate::layout_audit::Bounds> },
    Formula { bounds: Option<crate::layout_audit::Bounds> },
    Unknown,
}

#[derive(Clone, Debug)]
pub struct TypesettingPreview {
    pub original_bounds: Option<crate::layout_audit::Bounds>,
    pub glyphs: Vec<PreviewGlyph>,
    pub units: Vec<PreviewUnit>,
    pub formulas: Vec<PreviewFormula>,
    pub formula_identities_match: Option<bool>,
    pub rejections: Vec<PreviewRejection>,
}

#[derive(Clone, Debug)]
pub struct PreviewFormula {
    pub source_identity: Option<u64>,
    pub character_source_identities: Vec<Option<u64>>,
    pub bounds: Option<crate::layout_audit::Bounds>,
    pub character_bounds: Vec<Option<crate::layout_audit::Bounds>>,
    pub curve_bounds: Vec<Option<crate::layout_audit::Bounds>>,
    pub form_bounds: Vec<Option<crate::layout_audit::Bounds>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FormulaSignature {
    source_identity: Option<u64>,
    characters: Vec<(Option<u64>, Option<i64>, Option<String>, Option<String>, Option<i64>, Option<u64>, Option<bool>, Option<bool>)>,
    curves: Vec<u64>,
    forms: Vec<u64>,
}

#[derive(Default)]
struct FormulaCapture {
    expected: Vec<FormulaSignature>,
    rendered: Vec<FormulaSignature>,
    placements: Vec<PreviewFormula>,
    complete: bool,
    render_failed: bool,
    output: Vec<(usize, bool, bool)>,
}

#[derive(Default)]
struct RenderedTrial {
    characters: Vec<PdfCharacter>,
    curves: Vec<PdfCurve>,
    forms: Vec<PdfForm>,
    rendered: Vec<FormulaSignature>,
    placements: Vec<PreviewFormula>,
    output: Vec<(usize, bool, bool)>,
    verification_failed: bool,
}

fn render_trial(placed: Vec<TypesettingUnit>, capture: bool) -> Result<RenderedTrial, StageError> {
    let mut trial = RenderedTrial::default();
    for mut unit in placed {
        let formula = capture.then(|| unit.formula.as_ref()).flatten()
            .map(|formula| (formula_signature(formula), formula_placement(formula)));
        let (characters, curves, forms) = unit.render()?;
        if capture {
            trial.output.push((characters.len(), formula.is_some(), unit.inferred_source_space));
            if let Some((signature, placement)) = formula {
                let actual = formula_parts_signature(signature.source_identity, &characters, &curves, &forms);
                trial.verification_failed |= actual != signature;
                trial.rendered.push(actual);
                trial.placements.push(placement);
            }
        }
        trial.characters.extend(characters);
        trial.curves.extend(curves);
        trial.forms.extend(forms);
    }
    Ok(trial)
}

fn formula_parts_signature(
    source_identity: Option<u64>, characters: &[PdfCharacter], curves: &[PdfCurve], forms: &[PdfForm],
) -> FormulaSignature {
    FormulaSignature {
        source_identity,
        characters: characters.iter().map(|character| (
            character.source_identity,
            character.pdf_character_id,
            character.char_unicode.clone(),
            character.pdf_style.as_ref().and_then(|style| style.font_id.clone()),
            character.xobj_id,
            character.pdf_style.as_ref().and_then(|style| style.graphic_state.as_ref().map(Cursor::identity)),
            character.vertical,
            character.synthetic_spacing,
        )).collect(),
        curves: curves.iter().map(|curve| curve.original.identity()).collect(),
        forms: forms.iter().map(|form| form.original.identity()).collect(),
    }
}

fn formula_signature(formula: &PdfFormula) -> FormulaSignature {
    formula_parts_signature(formula.source_identity, &formula.pdf_character, &formula.pdf_curve, &formula.pdf_form)
}

fn preview_bounds_with_spacing(value: Option<&Box>, spacing: bool) -> Option<crate::layout_audit::Bounds> {
    let value = value?;
    let bounds = crate::layout_audit::Bounds {
        left: value.x?, bottom: value.y?, right: value.x2?, top: value.y2?,
    };
    ([bounds.left, bounds.bottom, bounds.right, bounds.top].iter().all(|value| value.is_finite())
        && (bounds.left < bounds.right || spacing && bounds.left == bounds.right)
        && bounds.bottom < bounds.top).then_some(bounds)
}

fn preview_bounds(value: Option<&Box>) -> Option<crate::layout_audit::Bounds> {
    preview_bounds_with_spacing(value, false)
}

fn formula_placement(formula: &PdfFormula) -> PreviewFormula {
    PreviewFormula {
        source_identity: formula.source_identity,
        character_source_identities: formula.pdf_character.iter().map(|character| character.source_identity).collect(),
        bounds: preview_bounds(formula.r#box.as_deref()),
        character_bounds: formula.pdf_character.iter().map(|character| preview_bounds_with_spacing(
            character.r#box.as_deref(),
            character.synthetic_spacing == Some(true) && character.pdf_character_id.is_none()
                && matches!(character.char_unicode.as_deref(), Some(" " | "\n")),
        )).collect(),
        curve_bounds: formula.pdf_curve.iter().map(|curve| preview_bounds(curve.r#box.as_deref())).collect(),
        form_bounds: formula.pdf_form.iter().map(|form| preview_bounds(form.r#box.as_deref())).collect(),
    }
}

impl Typesetting {
    pub fn new(
        lang: &str,
        mapper: Arc<FontMapper>,
        progress: bool,
        font_source_styles: HashMap<String, SourceFontStyle>,
    ) -> Self {
        let l = lang.to_uppercase();
        Self {
            mapper,
            is_cjk: ["ZH", "JA", "JP", "KR", "CN", "HK", "TW"]
                .iter()
                .any(|s| l.contains(s)),
            progress,
            font_source_styles,
        }
    }
    fn font_map(&self, page: &Page, v: &View<'_>) -> Result<FontTable, StageError> {
        let read = |f: &Cursor<types::PdfFont>| -> Result<SourceFontStyle, StageError> {
            Ok(SourceFontStyle {
                bold: runtime_font_flag(f.bold(v)?),
                italic: runtime_font_flag(f.italic(v)?),
                monospaced: runtime_font_flag(f.monospace(v)?),
                serif: runtime_font_flag(f.serif(v)?),
            })
        };
        let mut out = FontTable::default();
        let mut fonts = vec![];
        for f in page.pdf_font_iter(v)? {
            let f = f?;
            if let Some(id) = f.font_id(v)?.clone() {
                fonts.push((id, read(&f)?))
            }
        }
        for (id, s) in &fonts {
            out.fonts.insert((None, id.clone()), *s);
        }
        for obj in page.pdf_xobject_iter(v)? {
            let obj = obj?;
            if let Some(id) = obj.xobj_id(v)? {
                out.xobjects.insert(id);
                for (fid, s) in &fonts {
                    out.fonts.insert((Some(id), fid.clone()), *s);
                }
                for f in obj.pdf_font_iter(v)? {
                    let f = f?;
                    if let Some(fid) = f.font_id(v)?.clone() {
                        out.fonts.insert((Some(id), fid), read(&f)?);
                    }
                }
            }
        }
        Ok(out)
    }
    fn create_units(
        &self,
        p: &P,
        fonts: &FontTable,
        v: &mut View<'_>,
    ) -> Result<Vec<TypesettingUnit>, StageError> {
        let mut out = vec![];
        let xobj = p.xobj_id(v)?;
        for child_index in 0..p.pdf_paragraph_composition_len(v)? {
            let comp = p.pdf_paragraph_composition_at(v, child_index)?;
            if let Some(line) = comp.pdf_line(v)? {
                for child_index in 0..line.pdf_character_len(v)? {
                    let c = line.pdf_character_at(v, child_index)?;
                    out.push(TypesettingUnit::from_character(
                        read_character(c, v)?,
                        false,
                    ))
                }
            } else if let Some(character) = comp.pdf_character(v)? {
                let debug = character.debug_info(v)?.unwrap_or(false);
                out.push(TypesettingUnit::from_character(read_character(character, v)?, debug));
            } else if let Some(same) = comp.pdf_same_style_characters(v)? {
                let mut previous: Option<PdfCharacter> = None;
                for child_index in 0..same.pdf_character_len(v)? {
                    let c = same.pdf_character_at(v, child_index)?;
                    let character = read_character(c, v)?;
                    if previous.as_ref().is_some_and(|left| TypesettingUnit::source_word_wrap(left, &character)) {
                        out.push(TypesettingUnit::source_word_space(previous.as_ref().unwrap()));
                    }
                    previous = Some(character.clone());
                    out.push(TypesettingUnit::from_character(character, false));
                }
            } else if let Some(unis) = comp.pdf_same_style_unicode_characters(v)? {
                let unicode = unis.unicode(v)?.clone().ok_or_else(|| {
                    typesetting_failed(TypesettingFailure::MissingContent, format!("composition {child_index} has no unicode"))
                })?;
                if unicode.chars().all(|character| character == '\n') {
                    continue;
                }
                let style = unis.pdf_style(v)?.ok_or_else(|| {
                    typesetting_failed(TypesettingFailure::MissingStyle, format!("composition {child_index}"))
                })?;
                let id = style.font_id(v)?.clone().ok_or_else(|| {
                    typesetting_failed(TypesettingFailure::MissingFontId, format!("composition {child_index}"))
                })?;
                let asset = self.mapper.font_by_id(&id);
                let source_scope = xobj.filter(|scope| fonts.xobjects.contains(scope));
                let sf = if let Some(face) = asset {
                    let t = face.traits();
                    let explicit = self.font_source_styles
                        .get(&id)
                        // `base` aliases a loaded face; its spelling does not
                        // describe the source font's serif/weight flags.
                        .or_else(|| self.font_source_styles.get(face.id()))
                        .copied();
                    let lower_id = face.id().to_lowercase();
                    let inferred = SourceFontStyle {
                        bold: t.bold,
                        italic: t.italic,
                        monospaced: t.monospaced,
                        serif: lower_id.contains("serif") && !lower_id.contains("sans"),
                    };
                    if explicit.is_none() && fonts.fonts.get(&(source_scope, id.clone())).is_some_and(|source| *source != inferred) {
                        return Err(typesetting_failed(TypesettingFailure::AmbiguousFontId, format!("font id {id:?}, scope {source_scope:?} has conflicting source and mapper styles")));
                    }
                    explicit.unwrap_or(inferred)
                } else {
                    *fonts
                        .fonts
                        .get(&(source_scope, id.clone()))
                        .ok_or_else(|| typesetting_failed(TypesettingFailure::MissingSourceFont, format!("font id {id:?}, scope {xobj:?}")))?
                };
                let size = style.font_size(v)?.unwrap_or_default();
                if size == 0. {
                    return Err(StageError::failed(
                        "Font size must be provided when unicode is provided",
                    ));
                }
                let cached = PdfStyle {
                    font_id: Some(id),
                    font_size: Some(size),
                    graphic_state: style.graphic_state(v)?.map(|g| g.retain(v)).transpose()?,
                };
                for ch in unicode.chars().filter(|c| *c != '\n') {
                    let font = self.mapper.map(sf, ch).ok_or_else(|| {
                        typesetting_failed(TypesettingFailure::MissingGlyph, format!("U+{:04X}, source font {:?}", ch as u32, cached.font_id))
                    })?;
                    out.push(TypesettingUnit::from_unicode(
                        ch.to_string(),
                        font,
                        None,
                        size,
                        cached.clone(),
                        xobj,
                        unis.debug_info(v)?.unwrap_or(false),
                    ));
                }
            } else if let Some(f) = comp.pdf_formula(v)? {
                out.push(TypesettingUnit::from_formula(read_formula(f, v)?))
            }
        }
        if out.is_empty() && p.unicode(v)?.as_deref().is_some_and(|text| text.chars().any(|character| !character.is_whitespace())) {
            return Err(typesetting_failed(TypesettingFailure::MissingContent, "nonempty paragraph has no typesetting units"));
        }
        if out.iter().all(TypesettingUnit::calc_can_passthrough) {
            out.retain(|unit| unit.character.as_ref().is_none_or(|character| {
                character.original.is_some() || character.synthetic_spacing != Some(true)
            }));
        }
        Ok(out)
    }
    fn layout(
        &self,
        units: &mut [TypesettingUnit],
        b: &Box,
        scale: f64,
        line_skip: f64,
        first_line_indent: bool,
        english: bool,
    ) -> Result<(Vec<TypesettingUnit>, bool), StageError> {
        let mut sizes = Vec::new();
        for u in units.iter() {
            if let Some(s) = u.font_size.filter(|x| *x != 0.0) {
                sizes.push(s)
            }
            if let Some(s) = u
                .character
                .as_ref()
                .and_then(|c| c.pdf_style.as_deref())
                .and_then(|s| s.font_size)
                .filter(|x| *x != 0.0)
            {
                sizes.push(s)
            }
        }
        sizes.sort_by(f64::total_cmp);
        let dominant = mode(&sizes)?;
        let space = self
            .mapper
            .base_font()
            .char_lengths("你", dominant * scale)
            .first()
            .copied()
            .ok_or_else(|| StageError::Failed("font char_lengths returned no widths".into()))?
            * 0.5;
        sizes.clear();
        for u in units.iter_mut() {
            sizes.push(u.height()?)
        }
        let avg = if sizes.is_empty() {
            0.0
        } else {
            mode(&sizes).unwrap_or_else(|_| sizes.iter().sum::<f64>() / sizes.len() as f64) * scale
        };
        let (bx, by, right, top) = box_coordinates(b)?;
        let mut x = bx;
        let mut y = top - avg;
        let mut line_height: f64 = 0.;
        let mut line_heights = sizes;
        line_heights.clear();
        let mut out: Vec<TypesettingUnit> = Vec::new();
        let mut fit = true;
        if first_line_indent {
            x += space * 4.
        }
        for i in 0..units.len() {
            let w = units[i].width()? * scale;
            let h = units[i].height()? * scale;
            if x == bx && units[i].is_space() {
                out.push(units[i].boundary_space(x, y, h));
                continue;
            }
            if units[i].character.as_ref().is_some_and(|character| {
                character.synthetic_spacing == Some(true)
                    && character.pdf_character_id.is_none() && character.char_unicode.as_deref() == Some(" ")
                    && (character.original.is_none() || w == 0.0)
            }) {
                let w = if w == 0.0 {
                    units[i].character.as_ref().and_then(|character| character.pdf_style.as_deref())
                        .and_then(|style| style.font_size).unwrap_or_default() * 0.25 * scale
                } else { w };
                if x + w > right {
                    x = bx;
                    if line_heights.is_empty() {
                        return Ok((vec![], false));
                    }
                    let maxh = line_heights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let modeh = mode(&line_heights)?;
                    y -= (dominant * scale * line_skip).max(modeh * line_skip).max(maxh * 1.05);
                    line_height = h;
                    line_heights.clear();
                    if y < by { fit = false; }
                    out.push(units[i].boundary_space(x, y, h));
                } else {
                    out.push(units[i].boundary_space(x, y, h));
                    x += w;
                }
                continue;
            }
            if let Some(prev) = out.last_mut() {
                let cjk = prev.is_cjk_char() ^ units[i].is_cjk_char();
                let pb = prev.box_value()?;
                let same = if pb.y.is_some_and(|v| v != 0.0) {
                    let previous_y2 = pb
                        .y2
                        .ok_or_else(|| StageError::Failed("box.y2 is None".into()))?;
                    previous_y2 >= y - 0.1 && previous_y2 <= y + line_height + 0.1
                } else {
                    false
                };
                let blacklist =
                    prev.mixed_character_blacklist() || units[i].mixed_character_blacklist();
                if cjk
                    && same
                    && !blacklist
                    && x > bx
                    && units[i].try_get_unicode() != Some(" ")
                    && prev.try_get_unicode() != Some(" ")
                    && !matches!(
                        prev.try_get_unicode(),
                        Some("。" | "！" | "？" | "；" | "：" | "，")
                    )
                {
                    x += space * 0.5
                }
            }
            let before = if english {
                width_before(&mut units[i..], scale)?
            } else {
                0.
            };
            if !units[i].is_hung_punctuation()
                && (x + w > right
                    || (english && x + w + before > right)
                    || (units[i].is_cannot_appear_in_line_end_punctuation() && x + w * 2. > right))
            {
                x = bx;
                if line_heights.is_empty() {
                    return Ok((vec![], false));
                }
                let maxh = line_heights
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                let modeh = mode(&line_heights)?;
                y -= (dominant * scale * line_skip)
                    .max(modeh * line_skip)
                    .max(maxh * 1.05);
                line_height = 0.;
                line_heights.clear();
                if y < by {
                    fit = false
                }
                if units[i].is_space() {
                    line_height = line_height.max(h);
                    out.push(units[i].boundary_space(x, y, h));
                    continue;
                }
            }
            let debug_info = units[i].debug_info;
            let mut relocated = units[i].relocate(x, y, scale, debug_info)?;
            let rb = relocated.box_value()?;
            let previous_x = x;
            x = rb.x2.unwrap_or_default();
            if previous_x > x {
                warn!(target: LOG_TARGET, "坐标回绕！！！TypesettingUnit: {}，", units[i]);
            }
            if !units[i].is_space() {
                line_heights.push(h)
            }
            out.push(relocated);
        }
        Ok((out, fit))
    }
    fn conflict(
        &self,
        b: &Box,
        page: &Page,
        exclude: usize,
        bottom: bool,
        v: &View<'_>,
    ) -> Result<f64, StageError> {
        let crop = read_box(
            need(
                need(page.cropbox(v)?, "page has no cropbox")?.r#box(v)?,
                "cropbox has no box",
            )?,
            v,
        )?;
        let mut limit = if bottom {
            need(crop.y, "cropbox.box.y is None")? * 1.1
        } else {
            need(crop.x2, "cropbox.box.x2 is None")? * 0.9
        };
        let mut boxes = vec![];
        for (i, p) in page.pdf_paragraph_iter(v)?.enumerate() {
            let p = p?;
            if i == exclude {
                continue;
            }
            if let Some(c) = p.r#box(v)? {
                let c = read_box(c, v)?;
                if !same_box(&c, b) {
                    boxes.push(c)
                }
            }
        }
        for c in page.pdf_character_iter(v)? {
            let c = c?;
            boxes.push(read_box(need(c.r#box(v)?, "character has no box")?, v)?)
        }
        for c in page.pdf_figure_iter(v)? {
            let c = c?;
            boxes.push(read_box(need(c.r#box(v)?, "figure has no box")?, v)?)
        }
        let (cx, cy, cx2, cy2) = box_coordinates(b)?;
        for b in boxes {
            let (x, y, x2, y2) = box_coordinates(&b)?;
            if bottom {
                if y2 < cy && x < cx2 && x2 > cx {
                    limit = limit.max(y2)
                }
            } else if x > cx && y < cy2 && y2 > cy {
                limit = limit.min(x)
            }
        }
        Ok(limit)
    }
    fn optimal(
        &self,
        p: &P,
        page: &Page,
        index: usize,
        units: &mut [TypesettingUnit],
        initial: f64,
        english: bool,
        apply: bool,
        adjusted_box: Option<&Box>,
        v: &mut View<'_>,
        mut capture: Option<&mut FormulaCapture>,
    ) -> Result<f64, StageError> {
        if !initial.is_finite() || initial < 0.1 {
            return Err(typesetting_failed(TypesettingFailure::LayoutExhausted, format!("invalid initial scale {initial}")));
        }
        let mut b = if let Some(b) = adjusted_box {
            b.clone()
        } else {
            let b = p.r#box(v)?.ok_or_else(|| {
                typesetting_failed(TypesettingFailure::MissingParagraphBox, "paragraph requires layout")
            })?;
            read_box(b, v)?
        };
        let mut scale = initial;
        let skip = if self.is_cjk { 1.5 } else { 1.3 };
        let mut expand = 0;
        let mut box_expanded = adjusted_box.is_some();
        let mut last_failure = None;
        while scale >= 0.1 {
            match self.layout(
                units,
                &b,
                scale,
                skip,
                p.first_line_indent(v)?.unwrap_or(false),
                english,
            ) {
                Ok((placed, true)) => {
                    if apply {
                        let trial = match render_trial(placed, capture.is_some()) {
                            Ok(trial) => trial,
                            Err(error) => {
                                last_failure = Some(error.to_string());
                                scale -= if scale > 0.6 { 0.05 } else { 0.1 };
                                continue;
                            }
                        };
                        let mut compositions = Vec::with_capacity(trial.characters.len());
                        for character in trial.characters {
                            compositions.push(ir::PdfParagraphCompositionInit {
                                pdf_character: Some(char_value(character, v)?),
                                ..Default::default()
                            }.into_value());
                        }
                        let curves = trial.curves.into_iter().map(|curve| curve_value(curve, v)).collect::<Result<Vec<_>, _>>()?;
                        let forms = trial.forms.into_iter().map(|form| form_value(form, v)).collect::<Result<Vec<_>, _>>()?;
                        if box_expanded {
                            p.set_box(v, Some(box_field(b.clone())))?;
                        }
                        p.set_scale(v, Some(scale))?;
                        p.set_pdf_paragraph_composition(v, compositions)?;
                        for curve in curves {
                            page.push_pdf_curve(v, curve)?;
                        }
                        for form in forms {
                            page.push_pdf_form(v, form)?;
                        }
                        if let Some(capture) = capture.as_deref_mut() {
                            capture.render_failed = trial.verification_failed;
                            capture.rendered = trial.rendered;
                            capture.placements = trial.placements;
                            capture.output = trial.output;
                            capture.complete = true;
                        }
                    }
                    return Ok(scale);
                }
                Ok((_, false)) => last_failure = Some("paragraph does not fit".into()),
                Err(error) => last_failure = Some(error.to_string()),
            }
            scale -= if scale > 0.6 { 0.05 } else { 0.1 };
            if scale < 0.7 {
                let mut expanded = false;
                if expand == 0 {
                    if let Ok(y) = self.conflict(&b, page, index, true, v) {
                        let y = y + 2.;
                        if y < b.y.unwrap_or_default() {
                            b.y = Some(y);
                            box_expanded = true;
                            expanded = true
                        }
                    }
                    expand = 1;
                } else if expand == 1 {
                    if let Ok(x) = self.conflict(&b, page, index, false, v) {
                        let x = x - 5.;
                        if x > b.x2.unwrap_or_default() {
                            b.x2 = Some(x);
                            box_expanded = true;
                            expanded = true
                        }
                    }
                    expand = 2;
                }
                if expanded {
                    continue;
                }
                if expand < 2 {
                    scale = 1.
                }
            }
        }
        if english {
            self.optimal(p, page, index, units, initial, false, apply, adjusted_box, v, capture)
        } else {
            if let Some(capture) = capture {
                capture.render_failed = true;
            }
            Err(typesetting_failed(TypesettingFailure::LayoutExhausted, last_failure.unwrap_or_else(|| format!("invalid initial scale {initial}"))))
        }
    }
    fn render_paragraph(
        &self,
        p: &P,
        page: &Page,
        index: usize,
        fonts: &FontTable,
        adjusted_box: Option<&Box>,
        v: &mut View<'_>,
        mut capture: Option<&mut FormulaCapture>,
    ) -> Result<(), StageError> {
        let mut units = self.create_units(p, fonts, v)?;
        if let Some(capture) = capture.as_deref_mut() {
            capture.expected = units.iter().filter_map(|unit| unit.formula.as_ref().map(formula_signature)).collect();
        }
        if units.is_empty() {
            if let Some(capture) = capture { capture.complete = true; }
            return Ok(());
        }
        if units.iter_mut().all(TypesettingUnit::can_passthrough) {
            let mut comps = vec![];
            for u in units {
                if let Some(f) = u.formula {
                    if let Some(capture) = capture.as_deref_mut() {
                        capture.output.push((1, true, false));
                        capture.rendered.push(formula_signature(&f));
                        capture.placements.push(formula_placement(&f));
                    }
                    let f = need(f.original, "passthrough formula lost selection")?;
                    comps.push(
                        ir::PdfParagraphCompositionInit {
                            pdf_formula: Some(f.share(v)?),
                            ..Default::default()
                        }
                        .into_value(),
                    );
                } else if let Some(c) = u.character {
                    if let Some(capture) = capture.as_deref_mut() { capture.output.push((1, false, false)); }
                    comps.push(
                        ir::PdfParagraphCompositionInit {
                            pdf_character: Some(char_value(c, v)?),
                            ..Default::default()
                        }
                        .into_value(),
                    );
                }
            }
            if let Some(b) = adjusted_box {
                p.set_box(v, Some(box_field(b.clone())))?;
            }
            p.set_scale(v, Some(1.))?;
            p.set_pdf_paragraph_composition(v, comps)?;
            if let Some(capture) = capture { capture.complete = true; }
            return Ok(());
        }
        let initial = p.optimal_scale(v)?.unwrap_or(1.);
        self.optimal(p, page, index, &mut units, initial, true, true, adjusted_box, v, capture)?;
        if let Some(order) = p.render_order(v)? {
            let mut sub = 1;
            for child_index in 0..p.pdf_paragraph_composition_len(v)? {
                let comp = p.pdf_paragraph_composition_at(v, child_index)?;
                if let Some(c) = comp.pdf_character(v)? {
                    c.set_render_order(v, Some(order))?;
                    c.set_sub_render_order(v, Some(sub))?;
                    sub += 1
                }
            }
        }
        Ok(())
    }
    fn preprocess(
        &self,
        pages: &[Page],
        v: &mut View<'_>,
        ctx: &mut StageContext<'_>,
        audit_checkpoints: bool,
    ) -> Result<(), StageError> {
        let mut weighted_scales = HashMap::<u64, (f64, usize)>::new();
        let mut nan_min = None;
        let mut has_weighted_scale = false;
        for page in pages.iter() {
            if audit_checkpoints { ctx.check_cancelled()?; }
            ctx.report_progress(1, Some(pages.len() as u64 * 2))?;
            let fonts = self.font_map(page, v)?;
            for index in 0..page.pdf_paragraph_len(v)? {
                if audit_checkpoints { ctx.check_cancelled()?; }
                let p = page.pdf_paragraph_at(v, index)?;
                let result = (|| {
                    let mut units = self.create_units(&p, &fonts, v)?;
                    let count = units
                        .iter()
                        .map(|u| u.formula.as_ref().map_or(1, |f| f.pdf_character.len()))
                        .sum::<usize>();
                    if units.is_empty() {
                        return Ok::<_, StageError>(0);
                    }
                    let scale = if units.iter_mut().all(TypesettingUnit::can_passthrough) {
                        1.
                    } else {
                        self.optimal(&p, page, index, &mut units, 1., true, false, None, v, None)?
                    };
                    p.set_optimal_scale(v, Some(scale))?;
                    Ok::<_, StageError>(count)
                })();
                let count = match result {
                    Ok(n) => n,
                    Err(e) => {
                        if matches!(typesetting_failure(&e), Some(
                            TypesettingFailure::MissingStyle
                            | TypesettingFailure::MissingFontId
                            | TypesettingFailure::MissingSourceFont
                            | TypesettingFailure::AmbiguousFontId
                            | TypesettingFailure::MissingGlyph
                            | TypesettingFailure::MissingContent
                        )) {
                            return Err(e);
                        }
                        warn!(target:LOG_TARGET,"预处理段落时出错：{e}");
                        p.set_optimal_scale(v, Some(1.))?;
                        0
                    }
                };
                if let Some(scale) = p.optimal_scale(v)? {
                    if count > 0 {
                        has_weighted_scale = true;
                        if scale.is_nan() {
                            nan_min = Some(match nan_min {
                                None => scale,
                                Some(previous) if scale.total_cmp(&previous).is_lt() => scale,
                                Some(previous) => previous,
                            });
                        } else {
                            let key = if scale == 0.0 { 0 } else { scale.to_bits() };
                            let entry = weighted_scales.entry(key).or_insert((scale, 0));
                            entry.1 += count;
                        }
                    }
                }
            }
        }
        if !has_weighted_scale {
            error!(target:LOG_TARGET,"document_scales is empty, there seems no paragraph in this PDF");
        } else {
            // `modes` compared values with `==`: NaNs never matched themselves,
            // so they lose to any non-NaN value and otherwise total-order to the limit.
            let max_count = weighted_scales.values().map(|(_, count)| *count).max();
            let limit = max_count
                .and_then(|max| {
                    weighted_scales
                        .values()
                        .filter(|(_, count)| *count == max)
                        .map(|(scale, _)| *scale)
                        .min_by(f64::total_cmp)
                })
                .or(nan_min)
                .expect("a non-empty weighted scale distribution has a mode");
            for page in pages {
                if audit_checkpoints { ctx.check_cancelled()?; }
                for child_index in 0..page.pdf_paragraph_len(v)? {
                    if audit_checkpoints { ctx.check_cancelled()?; }
                    let p = page.pdf_paragraph_at(v, child_index)?;
                    if p.optimal_scale(v)?.is_some_and(|s| s > limit) {
                        p.set_optimal_scale(v, Some(limit))?
                    }
                }
            }
        }
        Ok(())
    }
    fn render_page(
        &self, page: &Page, v: &mut View<'_>,
        mut captures: Option<(usize, &mut HashMap<(usize, usize), FormulaCapture>)>,
    ) -> Result<(), StageError> {
        let fonts = self.font_map(page, v)?;
        let paragraph_len = page.pdf_paragraph_len(v)?;
        let mut indexed = vec![];
        let mut adjusted_boxes = HashMap::new();
        let mut changed_boxes = HashSet::new();
        for i in 0..paragraph_len {
            let p = page.pdf_paragraph_at(v, i)?;
            if let Some(b) = p.r#box(v)? {
                let b = read_box(b, v)?;
                if let Ok(coords) = box_coordinates(&b) {
                    indexed.push((i, coords));
                    adjusted_boxes.insert(i, b);
                }
            }
        }
        let adjustment = (|| -> Result<(), StageError> {
            for (i, _) in &indexed {
                let upper = box_coordinates(&adjusted_boxes[i])?;
                let gap = if upper.3 - upper.1 < 36. { 0.5 } else { 3. };
                let query = (upper.0, upper.1 - gap, upper.2, upper.1);
                let mut max: Option<f64> = None;
                for (j, c) in &indexed {
                    if j == i
                        || !(c.0 <= query.2 && c.2 >= query.0 && c.1 <= query.3 && c.3 >= query.1)
                    {
                        continue;
                    }
                    let lower = box_coordinates(&adjusted_boxes[j])?;
                    if !(lower.2 < upper.0 || lower.0 > upper.2) {
                        max = Some(max.map_or(lower.3, |m| m.max(lower.3)))
                    }
                }
                if let Some(y) = max {
                    let y = y + gap;
                    if y < upper.3 {
                        adjusted_boxes.get_mut(i).expect("indexed paragraph box").y = Some(y);
                        changed_boxes.insert(*i);
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = adjustment {
            warn!(target:LOG_TARGET,"Failed to adjust paragraph positions: {e}")
        }
        for index in 0..paragraph_len {
            let p = page.pdf_paragraph_at(v, index)?;
            let capture = captures.as_mut().and_then(|(page_index, captures)| captures.get_mut(&(*page_index, index)));
            let adjusted_box = changed_boxes.contains(&index).then(|| &adjusted_boxes[&index]);
            self.render_paragraph(&p, page, index, &fonts, adjusted_box, v, capture)?
        }
        Ok(())
    }
    pub fn process(
        &self,
        d: &mut ir::Document,
        ctx: &mut StageContext<'_>,
    ) -> Result<(), StageError> {
        let mut v = d.view::<TypesettingView>();
        let pages = v.document().page_iter(&v)?.collect::<Result<Vec<_>, _>>()?;
        if self.progress {
            self.preprocess(&pages, &mut v, ctx, false)?
        }
        for p in pages.iter() {
            ctx.check_cancelled()?;
            self.render_page(p, &mut v, None)?;
            if self.progress {
                ctx.report_progress(1, Some(pages.len() as u64 * 2))?;
            }
        }
        drop(pages);
        drop(v);
        d.compact();
        Ok(())
    }

    pub fn process_typed(
        &self,
        document: &mut ir::Document,
        context: &mut StageContext<'_>,
    ) -> Result<(), TypesettingError> {
        self.process(document, context).map_err(TypesettingError::from)
    }

    pub fn audit_fork(
        &self,
        document: &ir::Document,
        targets: &[(usize, usize)],
        ctx: &mut StageContext<'_>,
    ) -> Result<Vec<TypesettingPreview>, StageError> {
        self.audit_fork_with_document(document, targets, ctx).map(|(previews, _)| previews)
    }

    /// Returns the exact typeset fork used to produce the previews when capture succeeds.
    /// Preview rejections still apply; a returned document is not evidence of writer safety.
    pub fn audit_fork_with_document(
        &self,
        document: &ir::Document,
        targets: &[(usize, usize)],
        ctx: &mut StageContext<'_>,
    ) -> Result<(Vec<TypesettingPreview>, Option<ir::Document>), StageError> {
        let empty = || TypesettingPreview {
            original_bounds: None,
            glyphs: Vec::new(),
            units: Vec::new(),
            formulas: Vec::new(),
            formula_identities_match: None,
            rejections: vec![PreviewRejection::FormulaIdentityUnavailable, PreviewRejection::ObstacleAndClipEvidenceUnavailable],
        };
        let mut results: Vec<_> = targets.iter().map(|_| empty()).collect();
        ctx.check_cancelled()?;
        if targets.is_empty() { return Ok((results, None)); }
        let original = match document.artifact() {
            Ok(artifact) => artifact,
            Err(_) => {
                for result in &mut results { result.rejections.push(PreviewRejection::CaptureFailed); }
                return Ok((results, None));
            }
        };
        let read_bounds = |value: &serde_json::Value, spacing: bool| -> Option<crate::layout_audit::Bounds> {
            let bounds = crate::layout_audit::Bounds {
                left: value.get("x")?.as_f64()?, bottom: value.get("y")?.as_f64()?,
                right: value.get("x2")?.as_f64()?, top: value.get("y2")?.as_f64()?,
            };
            ([bounds.left, bounds.bottom, bounds.right, bounds.top].iter().all(|value| value.is_finite())
                && (bounds.left < bounds.right || spacing && bounds.left == bounds.right)
                && bounds.bottom < bounds.top).then_some(bounds)
        };
        for (&(page_index, paragraph_index), result) in targets.iter().zip(&mut results) {
            let paragraph = original.get("document").and_then(|value| value.get("page"))
                .and_then(|value| value.get(page_index)).and_then(|value| value.get("pdf_paragraph"))
                .and_then(|value| value.get(paragraph_index));
            if let Some(paragraph) = paragraph {
                result.original_bounds = paragraph.get("box").and_then(|bounds| read_bounds(bounds, false));
                if result.original_bounds.is_none() { result.rejections.push(PreviewRejection::MissingOrInvalidTargetBounds); }
            } else {
                result.rejections.push(PreviewRejection::TargetMissing);
            }
        }
        if !self.progress {
            for result in &mut results { result.rejections.push(PreviewRejection::CaptureFailed); }
            return Ok((results, None));
        }
        let mut fork = document.fork();
        let mut captures: HashMap<_, FormulaCapture> = targets.iter().copied().map(|target| (target, FormulaCapture::default())).collect();
        let mut formula_references = HashMap::<u64, usize>::new();
        let mut character_references = HashMap::<u64, usize>::new();
        let mut curve_references = HashMap::<u64, usize>::new();
        let mut form_references = HashMap::<u64, usize>::new();
        let run = (|| {
            let mut view = fork.view::<TypesettingView>();
            let pages = view.document().page_iter(&view)?.collect::<Result<Vec<_>, _>>()?;
            for page in &pages {
                ctx.check_cancelled()?;
                for index in 0..page.pdf_character_len(&view)? {
                    *character_references.entry(page.pdf_character_at(&view, index)?.identity()).or_default() += 1;
                }
                for curve_index in 0..page.pdf_curve_len(&view)? {
                    let identity = page.pdf_curve_at(&view, curve_index)?.identity();
                    *curve_references.entry(identity).or_default() += 1;
                }
                for form_index in 0..page.pdf_form_len(&view)? {
                    let identity = page.pdf_form_at(&view, form_index)?.identity();
                    *form_references.entry(identity).or_default() += 1;
                }
                for paragraph_index in 0..page.pdf_paragraph_len(&view)? {
                    ctx.check_cancelled()?;
                    let paragraph = page.pdf_paragraph_at(&view, paragraph_index)?;
                    for composition_index in 0..paragraph.pdf_paragraph_composition_len(&view)? {
                        let composition = paragraph.pdf_paragraph_composition_at(&view, composition_index)?;
                        if let Some(character) = composition.pdf_character(&view)? {
                            *character_references.entry(character.identity()).or_default() += 1;
                        }
                        if let Some(line) = composition.pdf_line(&view)? {
                            for index in 0..line.pdf_character_len(&view)? {
                                *character_references.entry(line.pdf_character_at(&view, index)?.identity()).or_default() += 1;
                            }
                        }
                        if let Some(group) = composition.pdf_same_style_characters(&view)? {
                            for index in 0..group.pdf_character_len(&view)? {
                                *character_references.entry(group.pdf_character_at(&view, index)?.identity()).or_default() += 1;
                            }
                        }
                        if let Some(formula) = composition.pdf_formula(&view)? {
                            *formula_references.entry(formula.identity()).or_default() += 1;
                            for index in 0..formula.pdf_character_len(&view)? {
                                *character_references.entry(formula.pdf_character_at(&view, index)?.identity()).or_default() += 1;
                            }
                            for curve_index in 0..formula.pdf_curve_len(&view)? {
                                let identity = formula.pdf_curve_at(&view, curve_index)?.identity();
                                *curve_references.entry(identity).or_default() += 1;
                            }
                            for form_index in 0..formula.pdf_form_len(&view)? {
                                let identity = formula.pdf_form_at(&view, form_index)?.identity();
                                *form_references.entry(identity).or_default() += 1;
                            }
                        }
                    }
                }
            }
            self.preprocess(&pages, &mut view, ctx, true)?;
            for (page_index, page) in pages.iter().enumerate() {
                ctx.check_cancelled()?;
                self.render_page(page, &mut view, Some((page_index, &mut captures)))?;
                ctx.report_progress(1, Some(pages.len() as u64 * 2))?;
            }
            drop(pages);
            drop(view);
            fork.compact();
            Ok::<_, StageError>(())
        })();
        if let Err(error) = run {
            if error == StageError::Cancelled { return Err(error); }
            for result in &mut results { result.rejections.push(PreviewRejection::TypesettingFailed); }
            return Ok((results, None));
        }
        let artifact = match fork.artifact() {
            Ok(artifact) => artifact,
            Err(_) => {
                for result in &mut results { result.rejections.push(PreviewRejection::CaptureFailed); }
                return Ok((results, None));
            }
        };
        for (&(page_index, paragraph_index), result) in targets.iter().zip(&mut results) {
        if let Some(capture) = captures.get(&(page_index, paragraph_index)) {
            result.formulas = capture.placements.clone();
            if capture.complete {
                let mut identities = HashSet::new();
                let mut character_identities = HashSet::new();
                let matched = !capture.render_failed && capture.expected == capture.rendered
                    && capture.expected.iter().all(|formula| formula.source_identity.is_some_and(|id| identities.insert(id) && formula_references.get(&id) == Some(&1))
                        && formula.characters.iter().all(|character| character.0.is_some_and(|id| character_identities.insert(id) && character_references.get(&id) == Some(&1)))
                        && formula.curves.iter().all(|identity| curve_references.get(identity) == Some(&1))
                        && formula.forms.iter().all(|identity| form_references.get(identity) == Some(&1)));
                result.formula_identities_match = Some(matched);
                if !matched { result.rejections.push(PreviewRejection::FormulaIdentityMismatch); }
            }
            for formula in &result.formulas {
                for bounds in std::iter::once(&formula.bounds)
                    .chain(&formula.character_bounds).chain(&formula.curve_bounds).chain(&formula.form_bounds) {
                    match (result.original_bounds, bounds) {
                        (_, None) => result.rejections.push(PreviewRejection::InvalidFormulaEvidence),
                        (Some(target), Some(bounds)) if bounds.left < target.left || bounds.bottom < target.bottom
                            || bounds.right > target.right || bounds.top > target.top => {
                            result.rejections.push(PreviewRejection::OutsideOriginalTargetBounds);
                        }
                        _ => {}
                    }
                }
            }
        }
        let Some(paragraph) = artifact.get("document").and_then(|value| value.get("page"))
            .and_then(|value| value.get(page_index)).and_then(|value| value.get("pdf_paragraph"))
            .and_then(|value| value.get(paragraph_index)) else {
            if !result.rejections.contains(&PreviewRejection::TargetMissing) { result.rejections.push(PreviewRejection::TargetMissing); }
            continue;
        };
        let Some(compositions) = paragraph.get("pdf_paragraph_composition").and_then(|value| value.as_array()) else {
            result.rejections.push(PreviewRejection::CaptureFailed);
            continue;
        };
        let source = original.get("document").and_then(|value| value.get("page"))
            .and_then(|value| value.get(page_index)).and_then(|value| value.get("pdf_paragraph"))
            .and_then(|value| value.get(paragraph_index));
        let expected = source.and_then(|value| value.get("pdf_paragraph_composition"))
            .and_then(|value| value.as_array()).and_then(|parts| {
                let mut text = Vec::new();
                for part in parts {
                    let supported = ["pdf_same_style_unicode_characters", "pdf_character", "pdf_line", "pdf_same_style_characters", "pdf_formula"];
                    if supported.iter().filter(|key| part.get(**key).is_some_and(|value| !value.is_null())).count() != 1 { return None; }
                    if let Some(unicode) = part.get("pdf_same_style_unicode_characters").filter(|value| !value.is_null()) {
                        text.extend(unicode.get("unicode")?.as_str()?.chars().filter(|character| *character != '\n').map(Some));
                    } else if let Some(character) = part.get("pdf_character").filter(|value| !value.is_null()) {
                        text.extend(character.get("char_unicode")?.as_str()?.chars().map(Some));
                    } else if let Some(group) = part.get("pdf_line").filter(|value| !value.is_null()).or_else(|| part.get("pdf_same_style_characters").filter(|value| !value.is_null())) {
                        for character in group.get("pdf_character")?.as_array()? {
                            text.extend(character.get("char_unicode")?.as_str()?.chars().map(Some));
                        }
                    } else if part.get("pdf_formula").is_some_and(|value| !value.is_null()) {
                        text.push(None);
                    } else {
                        return None;
                    }
                }
                Some(text)
            });
        let mut composition_glyphs = Vec::new();
        let mut composition_spacing = Vec::new();
        for composition in compositions {
            let Some(character) = composition.get("pdf_character").filter(|value| !value.is_null()) else {
                if composition.get("pdf_formula").filter(|value| !value.is_null()).is_none() {
                    result.rejections.push(PreviewRejection::CaptureFailed);
                }
                composition_glyphs.push(None);
                composition_spacing.push(None);
                continue;
            };
            let spacing = character.get("synthetic_spacing").and_then(|value| value.as_bool()) == Some(true)
                && character.get("pdf_character_id").is_none_or(|value| value.is_null())
                && matches!(character.get("char_unicode").and_then(|value| value.as_str()), Some(" " | "\n"));
            let bounds = character.get("box").and_then(|bounds| read_bounds(bounds, spacing));
            if spacing {
                if bounds.is_none() { result.rejections.push(PreviewRejection::InvalidSpacingEvidence); }
                if let (Some(target), Some(bounds)) = (result.original_bounds, bounds) {
                    if bounds.left < target.left || bounds.bottom < target.bottom || bounds.right > target.right || bounds.top > target.top {
                        result.rejections.push(PreviewRejection::OutsideOriginalTargetBounds);
                    }
                }
                composition_glyphs.push(None);
                composition_spacing.push(Some(bounds));
                continue;
            }
            composition_spacing.push(None);
            let glyph = PreviewGlyph {
                text: character.get("char_unicode").and_then(|value| value.as_str()).map(str::to_owned),
                glyph_id: character.get("pdf_character_id").and_then(|value| value.as_i64()),
                font_size: character.get("pdf_style").and_then(|value| value.get("font_size")).and_then(|value| value.as_f64()),
                font_id: character.get("pdf_style").and_then(|value| value.get("font_id")).and_then(|value| value.as_str()).map(str::to_owned),
                xobj_id: character.get("xobj_id").and_then(|value| value.as_i64()),
                bounds,
            };
            if glyph.glyph_id.is_none_or(|id| id <= 0) { result.rejections.push(PreviewRejection::MissingGlyph); }
            result.rejections.push(PreviewRejection::GlyphResourceUnavailable);
            match (glyph.font_id.as_deref().and_then(|id| self.mapper.font_by_id(id)), glyph.text.as_deref(), glyph.glyph_id) {
                (Some(font), Some(text), Some(id)) if glyph.xobj_id.is_none() && text.chars().count() == 1 => {
                    if i64::from(font.has_glyph(text.chars().next().unwrap() as u32)) != id {
                        result.rejections.push(PreviewRejection::GlyphMismatch);
                    }
                }
                _ => (),
            }
            if glyph.text.as_ref().is_none_or(String::is_empty) || glyph.font_size.is_none_or(|size| !size.is_finite() || size <= 0.0) || glyph.bounds.is_none() {
                result.rejections.push(PreviewRejection::InvalidGlyphEvidence);
            }
            if let (Some(target), Some(bounds)) = (result.original_bounds, glyph.bounds) {
                if bounds.left < target.left || bounds.bottom < target.bottom || bounds.right > target.right || bounds.top > target.top {
                    result.rejections.push(PreviewRejection::OutsideOriginalTargetBounds);
                }
            }
            composition_glyphs.push(Some(glyph));
        }
        let empty_slice = compositions.is_empty()
            && source.and_then(|paragraph| paragraph.get("unicode")).and_then(|value| value.as_str()) == Some("")
            && expected.as_ref().is_some_and(Vec::is_empty);
        if !empty_slice && composition_glyphs.iter().all(Option::is_none)
            && composition_spacing.iter().all(Option::is_none) { result.rejections.push(PreviewRejection::InvalidGlyphEvidence); }
        let mut observed = Vec::new();
        let mut inferred_spacing_positions = Vec::new();
        let mut position = 0;
        let mut formula_index = 0;
        let capture = captures.get(&(page_index, paragraph_index));
        let mut complete = capture.is_some_and(|capture| capture.complete && !capture.render_failed && result.formula_identities_match == Some(true));
        if let Some(capture) = capture {
            for &(count, is_formula, inferred_source_space) in &capture.output {
                if position + count > compositions.len() { complete = false; break; }
                let slice = &compositions[position..position + count];
                if is_formula {
                    let Some(signature) = capture.rendered.get(formula_index) else { complete = false; break; };
                    let Some(formula) = result.formulas.get(formula_index) else { complete = false; break; };
                    result.units.push(PreviewUnit::Formula { bounds: formula.bounds });
                    formula_index += 1;
                    let passthrough = count == 1 && slice[0].get("pdf_formula").is_some_and(|value| !value.is_null());
                    if !passthrough && (count != signature.characters.len() || slice.iter().zip(&signature.characters).any(|(part, character)| {
                        let Some(glyph) = part.get("pdf_character").filter(|value| !value.is_null()) else { return true; };
                        glyph.get("char_unicode").and_then(|value| value.as_str()) != character.2.as_deref()
                            || glyph.get("pdf_character_id").and_then(|value| value.as_i64()) != character.1
                            || glyph.get("pdf_style").and_then(|value| value.get("font_id")).and_then(|value| value.as_str()) != character.3.as_deref()
                            || glyph.get("xobj_id").and_then(|value| value.as_i64()) != character.4
                            || glyph.get("synthetic_spacing").and_then(|value| value.as_bool()) != character.7
                    })) { complete = false; }
                    observed.push(None);
                } else {
                    for (offset, part) in slice.iter().enumerate() {
                        if let Some(glyph) = &composition_glyphs[position + offset] {
                            result.units.push(PreviewUnit::Glyph(glyph.clone()));
                            result.glyphs.push(glyph.clone());
                        } else if let Some(bounds) = composition_spacing[position + offset] {
                            result.units.push(PreviewUnit::Spacing { bounds });
                        } else {
                            result.units.push(PreviewUnit::Unknown);
                            complete = false;
                        }
                        if let Some(text) = part.get("pdf_character").and_then(|value| value.get("char_unicode")).and_then(|value| value.as_str()) {
                            if inferred_source_space && composition_spacing[position + offset].is_some() && text == " " {
                                inferred_spacing_positions.push(observed.len());
                            }
                            observed.extend(text.chars().map(Some));
                        } else { complete = false; }
                    }
                }
                position += count;
            }
            if position != compositions.len() || formula_index != capture.rendered.len() { complete = false; }
        }
        if !complete { result.rejections.push(PreviewRejection::CaptureFailed); }
        match expected {
            Some(expected) => {
                let mut observed_index = 0;
                let mut expected_index = 0;
                while observed_index < observed.len() {
                    if observed.get(observed_index) == expected.get(expected_index) {
                        observed_index += 1;
                        expected_index += 1;
                    } else if inferred_spacing_positions.binary_search(&observed_index).is_ok() {
                        observed_index += 1;
                    } else {
                        break;
                    }
                }
                if observed_index != observed.len() || expected_index != expected.len() {
                    result.rejections.push(PreviewRejection::TextMismatch);
                    result.rejections.push(PreviewRejection::CaptureFailed);
                }
            }
            None => { result.rejections.push(PreviewRejection::TextEvidenceUnavailable); result.rejections.push(PreviewRejection::CaptureFailed); },
        }
        }
        ctx.check_cancelled()?;
        let capture_failed = captures.values().any(|capture| capture.render_failed)
            || results.iter().any(|result| result.rejections.contains(&PreviewRejection::CaptureFailed) || result.rejections.contains(&PreviewRejection::TargetMissing));
        Ok((results, if capture_failed { None } else { Some(fork) }))
    }
}
fn width_before(units: &mut [TypesettingUnit], scale: f64) -> Result<f64, StageError> {
    if units.is_empty() || units[0].can_break_line() {
        return Ok(0.);
    }
    let mut width = 0.;
    for u in units {
        if u.can_break_line() {
            return Ok(width * scale);
        }
        width += u.width()?
    }
    Ok(width * scale)
}
fn mode(values: &[f64]) -> Result<f64, StageError> {
    if values.is_empty() {
        return Err(StageError::Failed(
            "statistics.mode: no mode for empty data".into(),
        ));
    }
    let mut counts = Vec::<(f64, usize)>::new();
    for v in values {
        if let Some(e) = counts.iter_mut().find(|(x, _)| x == v) {
            e.1 += 1
        } else {
            counts.push((*v, 1))
        }
    }
    let max = counts.iter().map(|e| e.1).max().unwrap();
    Ok(counts.into_iter().find(|e| e.1 == max).unwrap().0)
}
#[cfg(test)]
mod typesetting_tests;

#[cfg(test)]
mod render_trial_tests {
    use super::*;

    #[test]
    fn failed_later_unit_leaves_formula_objects_uncommitted() {
        let node = |data| ir::Edge::Inline(std::boxed::Box::new(ir::Node::new(data)));
        let mut document = ir::Document::from_parts(ir::Node::new(ir::NodeData::Document(ir::DataDocument {
            page: vec![node(ir::NodeData::Page(ir::DataPage {
                pdf_curve: vec![node(ir::NodeData::PdfCurve(Default::default()))],
                pdf_form: vec![node(ir::NodeData::PdfForm(Default::default()))],
                ..Default::default()
            }))], ..Default::default()
        })), Default::default(), Default::default()).unwrap();
        let formula = {
            let mut view = document.view::<TypesettingView>();
            let page = view.document().page_at(&view, 0).unwrap();
            let curve = page.pdf_curve_at(&view, 0).unwrap().retain(&mut view).unwrap();
            let form = page.pdf_form_at(&view, 0).unwrap().retain(&mut view).unwrap();
            TypesettingUnit::from_formula(PdfFormula {
                pdf_curve: vec![PdfCurve { original: curve, r#box: None, relocation_transform: vec![] }],
                pdf_form: vec![PdfForm { original: form, r#box: None, relocation_transform: vec![] }],
                ..Default::default()
            })
        };
        let before = document.artifact().unwrap();
        let malformed = TypesettingUnit::empty(None, None, Some("A".into()), None, None, Some(10.0), None, None, false);
        assert!(render_trial(vec![formula.clone(), malformed], true).is_err());
        assert_eq!(before, document.artifact().unwrap());
        let retry = render_trial(vec![formula], true).unwrap();
        assert_eq!((retry.curves.len(), retry.forms.len()), (1, 1));
        assert_eq!(before, document.artifact().unwrap());
    }
}
