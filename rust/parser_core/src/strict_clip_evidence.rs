//! A deliberately small content-stream observer, not a PDF safety decision.
//!
//! The caller must supply the complete decoded page contents, in order, and an
//! independently established initial clip in the same coordinate system. This
//! module neither obtains nor validates those inputs. It accepts only integer
//! operands, invertible diagonal integer matrices, one rectangle per path, and
//! `q`, `Q`, `cm`, `w`, `re`, `W`, `W*`, `n`, rectangle fill and stroke operators.
//! Unsupported input is uncertain; even a
//! successful observation is not permission to modify or render a document.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x_min: i128,
    pub y_min: i128,
    pub x_max: i128,
    pub y_max: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipEvent {
    pub offset: usize,
    pub kind: ClipEventKind,
    pub clip: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipEventKind {
    Intersection,
    Restore,
}

/// Maximum number of intersection and graphics-state restoration events combined.
pub const MAX_CLIP_EVENTS: usize = 4096;
pub const MAX_PAINT_EVENTS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaintEvent {
    pub offset: usize,
    pub kind: PaintKind,
    pub path_bounds: Rect,
    /// Conservative un-clipped paint extent, including stroke width.
    pub paint_bounds: Rect,
    /// Intersection with the observed clip; None does not mean no paint operator occurred.
    pub bounds: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintKind { Fill, Stroke, FillStroke }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub final_clip: Option<Rect>,
    /// Clip state after each `n` completing a clip or each `Q`, in stream order.
    /// Offsets identify the first byte of the operator; the initial state is the caller's clip.
    pub events: Vec<ClipEvent>,
    /// Conservative integer bounds intersected with the clip at the paint operation.
    pub paints: Vec<PaintEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uncertainty {
    pub offset: usize,
    pub reason: &'static str,
}

#[derive(Clone, Copy)]
struct State {
    sx: i128,
    sy: i128,
    tx: i128,
    ty: i128,
    width: i128,
    clip: Option<Rect>,
}

fn intersect(left: Option<Rect>, right: Rect) -> Option<Rect> {
    let left = left?;
    let result = Rect {
        x_min: left.x_min.max(right.x_min),
        y_min: left.y_min.max(right.y_min),
        x_max: left.x_max.min(right.x_max),
        y_max: left.y_max.min(right.y_max),
    };
    (result.x_min < result.x_max && result.y_min < result.y_max).then_some(result)
}

fn whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn reserve<T>(items: &mut Vec<T>, additional: usize, offset: usize) -> Result<(), Uncertainty> {
    items.try_reserve(additional).map_err(|_| Uncertainty { offset, reason: "allocation failed" })
}

fn record_event(events: &mut Vec<ClipEvent>, event: ClipEvent) -> Result<(), Uncertainty> {
    if events.len() >= MAX_CLIP_EVENTS {
        return Err(Uncertainty { offset: event.offset, reason: "clip event limit exceeded" });
    }
    reserve(events, 1, event.offset)?;
    events.push(event);
    Ok(())
}

/// Observe a complete decoded stream sequence; any error invalidates all evidence.
pub fn observe(contents: &[u8], initial_clip: Rect) -> Result<Evidence, Uncertainty> {
    let fail = |offset, reason| Uncertainty { offset, reason };
    if initial_clip.x_min >= initial_clip.x_max || initial_clip.y_min >= initial_clip.y_max {
        return Err(fail(0, "invalid initial clip"));
    }
    if contents.len() > 16 * 1024 * 1024 {
        return Err(fail(0, "content size limit exceeded"));
    }
    let mut state = State { sx: 1, sy: 1, tx: 0, ty: 0, width: 1, clip: Some(initial_clip) };
    let mut stack = Vec::new();
    let mut operands = Vec::<i128>::new();
    let mut path: Option<Rect> = None;
    let mut pending = false;
    let mut events = Vec::new();
    let mut paints = Vec::new();
    let mut cursor = 0;
    while cursor < contents.len() {
        if whitespace(contents[cursor]) { cursor += 1; continue; }
        if contents[cursor] == b'%' {
            while cursor < contents.len() && !matches!(contents[cursor], b'\r' | b'\n') { cursor += 1; }
            continue;
        }
        let offset = cursor;
        while cursor < contents.len() && !whitespace(contents[cursor]) && contents[cursor] != b'%' { cursor += 1; }
        let token = &contents[offset..cursor];
        if token.first().is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')) {
            if pending { return Err(fail(offset, "operand after clipping operator")); }
            let text = std::str::from_utf8(token).map_err(|_| fail(offset, "invalid number"))?;
            let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(fail(offset, "unsupported or invalid number"));
            }
            let value = text.parse::<i64>().map_err(|_| fail(offset, "integer out of range"))?;
            if operands.len() == 6 { return Err(fail(offset, "too many operands")); }
            reserve(&mut operands, 1, offset)?;
            operands.push(value.into());
            continue;
        }
        if pending && token != b"n" { return Err(fail(offset, "clip must be followed by n")); }
        let expected = match token {
            b"q" | b"Q" | b"W" | b"W*" | b"n" | b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => 0,
            b"w" => 1,
            b"cm" => 6,
            b"re" => 4,
            _ => return Err(fail(offset, "unsupported or invalid operator")),
        };
        if operands.len() != expected { return Err(fail(offset, "invalid operand count")); }
        match token {
            b"q" => {
                if stack.len() >= 256 { return Err(fail(offset, "graphics stack limit exceeded")); }
                reserve(&mut stack, 1, offset)?;
                stack.push(state);
            }
            b"Q" => {
                if path.is_some() { return Err(fail(offset, "graphics restore with active path")); }
                state = stack.pop().ok_or_else(|| fail(offset, "graphics stack underflow"))?;
                record_event(&mut events, ClipEvent { offset, kind: ClipEventKind::Restore, clip: state.clip })?;
            }
            b"cm" => {
                if path.is_some() { return Err(fail(offset, "matrix change with active path")); }
                if operands[1] != 0 || operands[2] != 0 || operands[0] == 0 || operands[3] == 0 {
                    return Err(fail(offset, "unsupported or singular matrix"));
                }
                let arithmetic = || fail(offset, "matrix arithmetic overflow");
                state.tx = state.sx.checked_mul(operands[4]).and_then(|value| value.checked_add(state.tx)).ok_or_else(arithmetic)?;
                state.ty = state.sy.checked_mul(operands[5]).and_then(|value| value.checked_add(state.ty)).ok_or_else(arithmetic)?;
                state.sx = state.sx.checked_mul(operands[0]).ok_or_else(arithmetic)?;
                state.sy = state.sy.checked_mul(operands[3]).ok_or_else(arithmetic)?;
            }
            b"w" => {
                if operands[0] < 0 { return Err(fail(offset, "negative stroke width")); }
                state.width = operands[0];
            }
            b"re" => {
                if path.is_some() { return Err(fail(offset, "multiple rectangles in path")); }
                let arithmetic = || fail(offset, "rectangle arithmetic overflow");
                let x = operands[0]; let y = operands[1];
                let x_end = x.checked_add(operands[2]).ok_or_else(arithmetic)?;
                let y_end = y.checked_add(operands[3]).ok_or_else(arithmetic)?;
                let transform = |value: i128, scale: i128, translate: i128| {
                    value.checked_mul(scale).and_then(|value| value.checked_add(translate)).ok_or_else(arithmetic)
                };
                let x1 = transform(x, state.sx, state.tx)?; let x2 = transform(x_end, state.sx, state.tx)?;
                let y1 = transform(y, state.sy, state.ty)?; let y2 = transform(y_end, state.sy, state.ty)?;
                path = Some(Rect { x_min: x1.min(x2), y_min: y1.min(y2), x_max: x1.max(x2), y_max: y1.max(y2) });
            }
            b"W" | b"W*" => {
                if path.is_none() { return Err(fail(offset, "clip without rectangle")); }
                pending = true;
            }
            b"n" => {
                if pending {
                    state.clip = intersect(state.clip, path.expect("pending clip has a rectangle"));
                    record_event(&mut events, ClipEvent { offset, kind: ClipEventKind::Intersection, clip: state.clip })?;
                    pending = false;
                }
                path = None;
            }
            b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => {
                let rectangle = path.take().ok_or_else(|| fail(offset, "paint without rectangle"))?;
                let kind = match token {
                    b"f" | b"F" | b"f*" => PaintKind::Fill,
                    b"S" | b"s" => PaintKind::Stroke,
                    _ => PaintKind::FillStroke,
                };
                let paint_bounds = if kind == PaintKind::Fill { rectangle } else {
                    if state.width == 0 { return Err(fail(offset, "hairline stroke extent is device-dependent")); }
                    if rectangle.x_min == rectangle.x_max || rectangle.y_min == rectangle.y_max {
                        return Err(fail(offset, "degenerate rectangle stroke unsupported"));
                    }
                    let arithmetic = || fail(offset, "stroke arithmetic overflow");
                    let half = |scale: i128| scale.checked_abs().and_then(|value| value.checked_mul(state.width))
                        .and_then(|value| value.checked_add(1)).map(|value| value / 2).ok_or_else(arithmetic);
                    let x = half(state.sx)?;
                    let y = half(state.sy)?;
                    Rect {
                        x_min: rectangle.x_min.checked_sub(x).ok_or_else(arithmetic)?,
                        y_min: rectangle.y_min.checked_sub(y).ok_or_else(arithmetic)?,
                        x_max: rectangle.x_max.checked_add(x).ok_or_else(arithmetic)?,
                        y_max: rectangle.y_max.checked_add(y).ok_or_else(arithmetic)?,
                    }
                };
                if paints.len() >= MAX_PAINT_EVENTS { return Err(fail(offset, "paint event limit exceeded")); }
                reserve(&mut paints, 1, offset)?;
                paints.push(PaintEvent { offset, kind, path_bounds: rectangle, paint_bounds, bounds: intersect(state.clip, paint_bounds) });
            }
            _ => unreachable!(),
        }
        operands.clear();
    }
    if !operands.is_empty() || pending || path.is_some() || !stack.is_empty() {
        return Err(fail(contents.len(), "incomplete operands, path, clip or graphics stack"));
    }
    Ok(Evidence { final_clip: state.clip, events, paints })
}

#[cfg(test)]
mod tests {
    use super::reserve;

    #[test]
    fn allocation_failure_is_uncertain() {
        let error = reserve(&mut Vec::<u64>::new(), usize::MAX, 17).unwrap_err();
        assert_eq!(error.offset, 17);
        assert_eq!(error.reason, "allocation failed");
    }
}
