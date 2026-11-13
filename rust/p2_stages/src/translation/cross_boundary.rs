use std::{collections::HashSet, ops::Range, sync::LazyLock};

use serde::{Deserialize, Serialize};

use super::{
    cross_page_protocol::{validate_response, NUMERIC_EXPRESSION as NUMERIC},
    ordinary_translation::{InputPlaceholder, PlaceholderRegex, TranslateInput},
};

#[path = "cross_boundary_document.rs"]
mod document;
pub use document::{DocumentPlan, DocumentSelection, OutputPart, Resource, ResolvedParagraph};

#[path = "boundary_sentences.rs"]
pub mod sentences;

#[path = "boundary_response.rs"]
pub mod responses;

static MARKER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\[\[BABEL_[^\]]*\]\]").expect("marker pattern")
});
static SYMBOL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[\p{Sm}\p{Sc}%％‰‱^*−~]").expect("symbol pattern")
});
static COMBINING_MARK: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\p{M}").expect("combining mark pattern")
});
static GRAPHEME_EXTEND: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\p{gcb=Extend}").expect("grapheme extend pattern")
});
static GRAPHEME_PREPEND: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\p{gcb=Prepend}").expect("grapheme prepend pattern")
});
static HANGUL_PAIR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?:\p{gcb=L}[\p{gcb=L}\p{gcb=V}\p{gcb=LV}\p{gcb=LVT}]|[\p{gcb=LV}\p{gcb=V}][\p{gcb=V}\p{gcb=T}]|[\p{gcb=LVT}\p{gcb=T}]\p{gcb=T})$").expect("hangul grapheme pattern")
});
static WORD_CHAR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[\p{L}\p{N}\p{M}]").expect("word character pattern")
});
static UNSPACED_SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[\p{Han}\p{Hiragana}\p{Katakana}]").expect("unspaced script pattern")
});

fn cuts_word(text: &str, index: usize) -> bool {
    let mut before = text[..index].chars().rev();
    let mut after = text[index..].chars();
    let previous = before.next();
    let next = after.next();
    let is_word = |value: Option<char>| value.is_some_and(|ch| {
        let value = ch.to_string();
        WORD_CHAR.is_match(&value) && !UNSPACED_SCRIPT.is_match(&value)
    });
    let is_joiner = |value: Option<char>| matches!(value, Some('\'' | '’' | '-' | '‐' | '‑' | '_' | '־'));
    next.is_some_and(|ch| COMBINING_MARK.is_match(&ch.to_string()))
        || (previous == Some('\r') && next == Some('\n'))
        || next.is_some_and(|ch| GRAPHEME_EXTEND.is_match(&ch.to_string()))
        || previous.is_some_and(|ch| GRAPHEME_PREPEND.is_match(&ch.to_string()))
        || previous.zip(next).is_some_and(|(left, right)| HANGUL_PAIR.is_match(&format!("{left}{right}")))
        || matches!(previous, Some('\u{200c}' | '\u{200d}'))
        || matches!(next, Some('\u{200c}' | '\u{200d}' | '\u{1f3fb}'..='\u{1f3ff}'))
        || (matches!(previous, Some('\u{1f1e6}'..='\u{1f1ff}')) && matches!(next, Some('\u{1f1e6}'..='\u{1f1ff}')))
        || (is_word(previous) && is_word(next))
        || (is_word(previous) && is_joiner(next) && is_word(after.next()))
        || (is_joiner(previous) && is_word(before.next()) && is_word(next))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub page: usize,
    pub paragraph: usize,
}

#[derive(Clone, Debug)]
enum SourceKind {
    Plain,
    Rich(usize),
    Formula(usize),
}

#[derive(Clone, Debug)]
struct SourceSegment {
    range: Range<usize>,
    kind: SourceKind,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub location: Location,
    pub text: String,
    pub input: TranslateInput,
    segments: Vec<SourceSegment>,
}

fn push_source(text: &mut String, segments: &mut Vec<SourceSegment>, value: &str, kind: SourceKind) {
    if value.is_empty() { return; }
    let start = text.len();
    text.push_str(value);
    segments.push(SourceSegment { range: start..text.len(), kind });
}

impl Source {
    pub fn new<R: PlaceholderRegex>(
        location: Location, input: &TranslateInput, patterns: &[u64], regex: &mut R,
    ) -> Result<Option<Self>, R::Error> {
        if !input.original_tokens.is_empty() || input.text.contains("[[BABEL_")
            || !validate_response(regex, input, &input.text, patterns)? {
            return Ok(None);
        }
        let mut text = String::new();
        let mut segments = Vec::new();
        let mut cursor = 0;
        for (index, placeholder) in input.placeholders.iter().enumerate() {
            let (start_token, end_token, kind) = match placeholder {
                InputPlaceholder::Formula(formula) => (formula.token.as_str(), None, SourceKind::Formula(index)),
                InputPlaceholder::Rich(rich) => (rich.left.as_str(), Some(rich.right.as_str()), SourceKind::Rich(index)),
            };
            let Some(offset) = input.text[cursor..].find(start_token) else { return Ok(None) };
            let start = cursor + offset;
            push_source(&mut text, &mut segments, &input.text[cursor..start], SourceKind::Plain);
            let content_start = start + start_token.len();
            if let Some(end_token) = end_token {
                let Some(offset) = input.text[content_start..].find(end_token) else { return Ok(None) };
                let end = content_start + offset;
                push_source(&mut text, &mut segments, &input.text[content_start..end], kind);
                cursor = end + end_token.len();
            } else {
                let token = format!("[[BABEL_F_{}_{}_{}]]", location.page, location.paragraph, index);
                push_source(&mut text, &mut segments, &token, kind);
                cursor = content_start;
            }
        }
        push_source(&mut text, &mut segments, &input.text[cursor..], SourceKind::Plain);
        Ok(Some(Self { location, text, input: input.clone(), segments }))
    }

    pub fn formulas(&self) -> Vec<(String, String)> {
        self.segments.iter().filter_map(|segment| match segment.kind {
            SourceKind::Formula(index) => {
                let InputPlaceholder::Formula(formula) = &self.input.placeholders[index] else { unreachable!() };
                Some((self.text[segment.range.clone()].to_owned(), formula.source_chars.clone()))
            }
            _ => None,
        }).collect()
    }

    pub fn valid_range(&self, range: &Range<usize>) -> bool {
        let mut numeric_text = self.text.clone();
        for segment in &self.segments {
            if matches!(segment.kind, SourceKind::Formula(_)) {
                numeric_text.replace_range(segment.range.clone(), &" ".repeat(segment.range.len()));
            }
        }
        range.start < range.end && range.end <= self.text.len()
            && self.text.is_char_boundary(range.start) && self.text.is_char_boundary(range.end)
            && !cuts_word(&self.text, range.start) && !cuts_word(&self.text, range.end)
            && !self.segments.iter().any(|segment| matches!(segment.kind, SourceKind::Formula(_))
                && segment.range.start < range.end && segment.range.end > range.start
                && (range.start > segment.range.start || range.end < segment.range.end))
            && !NUMERIC.find_iter(&numeric_text).any(|found|
                (found.start() < range.start && range.start < found.end())
                || (found.start() < range.end && range.end < found.end()))
            && document::expression_range_is_complete(&numeric_text, range)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRange {
    pub page: usize,
    pub paragraph: usize,
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum Decision {
    None,
    Uncertain,
    Selected { ranges: Vec<SelectionRange>, joins: Vec<Join> },
}

impl<'de> Deserialize<'de> for Decision {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
        enum WireDecision {
            None {},
            Uncertain {},
            Selected { ranges: Vec<SelectionRange>, joins: Vec<Join> },
        }
        Ok(match WireDecision::deserialize(deserializer)? {
            WireDecision::None {} => Self::None,
            WireDecision::Uncertain {} => Self::Uncertain,
            WireDecision::Selected { ranges, joins } => Self::Selected { ranges, joins },
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Join { Space, None, Dehyphenate }

#[derive(Clone, Debug)]
pub struct SelectedPart {
    pub source: Source,
    pub range: Range<usize>,
}

pub fn select(
    decision: Decision, sources: &[Source], left: &HashSet<Location>, right: &HashSet<Location>,
) -> Result<Option<(Vec<SelectedPart>, Vec<Join>)>, &'static str> {
    let (ranges, joins) = match decision {
        Decision::None => return Ok(None),
        Decision::Uncertain => return Err("candidate selection is uncertain"),
        Decision::Selected { ranges, joins } => (ranges, joins),
    };
    if ranges.len() < 2 || joins.len() + 1 != ranges.len() { return Err("invalid selection length"); }
    let mut selected = Vec::new();
    let mut previous = None;
    let mut seen_right = false;
    for (position, item) in ranges.iter().enumerate() {
        let location = Location { page: item.page, paragraph: item.paragraph };
        let Some(index) = sources.iter().position(|source| source.location == location) else { return Err("unknown object"); };
        if previous.is_some_and(|previous| index <= previous) { return Err("objects are repeated or out of order"); }
        previous = Some(index);
        if right.contains(&location) { seen_right = true; }
        else if !left.contains(&location) || seen_right { return Err("objects are outside the boundary order"); }
        if item.text.is_empty() || item.text.trim() != item.text { return Err("empty or untrimmed range"); }
        let source = &sources[index];
        let matches = source.text.match_indices(&item.text).filter(|(start, _)|
            (position == 0 || source.text[..*start].trim().is_empty())
                && (position + 1 == ranges.len() || source.text[*start + item.text.len()..].trim().is_empty()))
            .map(|(start, _)| start).collect::<Vec<_>>();
        let [start] = matches.as_slice() else { return Err("range does not uniquely match the boundary"); };
        let start = *start;
        let range = start..start + item.text.len();
        if !source.valid_range(&range) { return Err("range cuts a protected formula"); }
        if position > 0 && !source.text[..start].trim().is_empty()
            || position + 1 < ranges.len() && !source.text[range.end..].trim().is_empty() {
            return Err("range leaves intervening source text");
        }
        selected.push(SelectedPart { source: source.clone(), range });
    }
    if !left.contains(&selected[0].source.location) || !right.contains(&selected.last().unwrap().source.location) {
        return Err("selection does not cross the boundary");
    }
    Ok(Some((selected, joins)))
}

#[derive(Clone, Debug)]
struct Protected {
    token: String,
    original: String,
    owner: usize,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub items: Vec<String>,
    parts: Vec<SelectedPart>,
    protected: Vec<Vec<Protected>>,
    weights: Vec<usize>,
    sentence_index: usize,
}

fn encode_range(source: &Source, range: Range<usize>, owner: usize, item: usize, protected: &mut Vec<Protected>) -> String {
    let mut result = String::new();
    for segment in &source.segments {
        let start = segment.range.start.max(range.start);
        let end = segment.range.end.min(range.end);
        if start >= end { continue; }
        let value = &source.text[start..end];
        match segment.kind {
            SourceKind::Plain => result.push_str(value),
            SourceKind::Formula(index) => {
                let InputPlaceholder::Formula(formula) = &source.input.placeholders[index] else { unreachable!() };
                let token = format!("[[BABEL_{item}_{owner}_{index}_F]]");
                result.push_str(&token);
                protected.push(Protected { token, original: formula.token.clone(), owner });
            }
            SourceKind::Rich(index) => {
                let InputPlaceholder::Rich(rich) = &source.input.placeholders[index] else { unreachable!() };
                for (side, original) in [("L", &rich.left), ("R", &rich.right)] {
                    let token = format!("[[BABEL_{item}_{owner}_{index}_{side}]]");
                    result.push_str(&token);
                    if side == "L" { result.push_str(value); }
                    protected.push(Protected { token, original: original.clone(), owner });
                }
            }
        }
    }
    result
}

/// Items are the optional prefix, the joined sentence, then the optional suffix.
/// Translate every item independently and return responses in the same order.
pub fn build_plan(parts: Vec<SelectedPart>, joins: Vec<Join>) -> Result<Plan, &'static str> {
    if parts.len() < 2 || joins.len() + 1 != parts.len() { return Err("invalid selection length"); }
    if parts.iter().any(|part| !part.source.valid_range(&part.range)) { return Err("invalid source range"); }
    let mut locations = HashSet::new();
    if parts.iter().any(|part| !locations.insert(part.source.location)) { return Err("repeated object"); }
    if parts.iter().enumerate().any(|(index, part)|
        (index > 0 && !part.source.text[..part.range.start].trim().is_empty())
        || (index + 1 < parts.len() && !part.source.text[part.range.end..].trim().is_empty())) {
        return Err("range leaves intervening source text");
    }
    let mut items = Vec::new();
    let mut protected = Vec::new();
    let first = &parts[0];
    if first.range.start > 0 {
        let mut tokens = Vec::new();
        let value = encode_range(&first.source, 0..first.range.start, 0, items.len(), &mut tokens);
        if !value.trim().is_empty() { items.push(value); protected.push(tokens); }
    }
    let sentence_index = items.len();
    let mut tokens = Vec::new();
    let mut sentence = String::new();
    let mut weights = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 && matches!(joins[index - 1], Join::Space) { sentence.push(' '); }
        let piece = encode_range(&part.source, part.range.clone(), index, sentence_index, &mut tokens);
        weights.push(MARKER.replace_all(&piece, "").chars().filter(|ch| !ch.is_whitespace()).count().max(1));
        sentence.push_str(&piece);
    }
    items.push(sentence);
    protected.push(tokens);
    let last_index = parts.len() - 1;
    let last = &parts[last_index];
    if last.range.end < last.source.text.len() {
        let mut tokens = Vec::new();
        let value = encode_range(&last.source, last.range.end..last.source.text.len(), last_index, items.len(), &mut tokens);
        if !value.trim().is_empty() { items.push(value); protected.push(tokens); }
    }
    Ok(Plan { items, parts, protected, weights, sentence_index })
}

fn invariants(text: &str) -> (Vec<String>, Vec<char>) {
    let text = MARKER.replace_all(text, " ");
    let mut numbers = NUMERIC.find_iter(&text).map(|m| m.as_str().to_owned()).collect::<Vec<_>>();
    numbers.sort();
    let without_numbers = NUMERIC.replace_all(&text, " ");
    let mut symbols = SYMBOL.find_iter(&without_numbers).flat_map(|value| value.as_str().chars()).collect::<Vec<_>>();
    symbols.sort();
    (numbers, symbols)
}

fn validate_item(input: &str, output: &str, tokens: &[Protected]) -> bool {
    if output.trim().is_empty() || output.chars().any(|ch| ch.is_control() && !ch.is_whitespace()) { return false; }
    let actual = MARKER.find_iter(output).map(|m| m.as_str()).collect::<Vec<_>>();
    if actual != tokens.iter().map(|token| token.token.as_str()).collect::<Vec<_>>() { return false; }
    if output.matches("[[BABEL_").count() != actual.len() { return false; }
    let unmarked = MARKER.replace_all(output, "");
    if unmarked.to_ascii_lowercase().contains("babel_") || unmarked.contains("[[") || unmarked.contains("]]") { return false; }
    for pair in tokens.windows(2) {
        if pair[0].token.ends_with("_L]]") && pair[1].token.ends_with("_R]]") {
            let start = output.find(&pair[0].token).unwrap() + pair[0].token.len();
            let end = output.find(&pair[1].token).unwrap();
            if output[start..end].trim().is_empty() || output[start..end].contains('\n') { return false; }
        }
    }
    invariants(input) == invariants(output)
}

fn content_boundary(output: &str, trailing: bool) -> usize {
    let mut boundary = None;
    let mut cursor = 0;
    for marker in MARKER.find_iter(output) {
        let mut visible = output[cursor..marker.start()].char_indices().filter(|(_, ch)| !ch.is_whitespace());
        if trailing {
            if let Some((index, ch)) = visible.last() { boundary = Some(cursor + index + ch.len_utf8()); }
        } else if let Some((index, _)) = visible.next() {
            return cursor + index;
        }
        if marker.as_str().ends_with("_F]]") {
            if trailing { boundary = Some(marker.end()); } else { return marker.start(); }
        }
        cursor = marker.end();
    }
    let mut visible = output[cursor..].char_indices().filter(|(_, ch)| !ch.is_whitespace());
    if trailing {
        visible.last().map(|(index, ch)| cursor + index + ch.len_utf8()).or(boundary).unwrap_or(0)
    } else {
        visible.next().map(|(index, _)| cursor + index).unwrap_or(output.len())
    }
}

fn restore_boundary_whitespace(output: &str, whitespace: &str, trailing: bool, styled: bool) -> String {
    let boundary = content_boundary(output, trailing);
    let (before, after) = output.split_at(boundary);
    if trailing {
        if styled {
            if let Some(marker) = MARKER.find(after) {
                return format!("{before}{whitespace}{}", &after[marker.start()..]);
            }
        }
        format!("{before}{}{whitespace}", after.trim_end_matches(char::is_whitespace))
    } else {
        if styled {
            if let Some(marker) = MARKER.find_iter(before).last() {
                return format!("{}{whitespace}{after}", &before[..marker.end()]);
            }
        }
        format!("{whitespace}{}{after}", before.trim_start_matches(char::is_whitespace))
    }
}

fn restore_item_boundary(output: &str, original: &str, whitespace: &str, trailing: bool, styled: bool) -> String {
    let output_boundary = content_boundary(output, trailing);
    let original_boundary = content_boundary(original, trailing);
    let output_edge = if trailing { &output[output_boundary..] } else { &output[..output_boundary] };
    let original_edge = if trailing { &original[original_boundary..] } else { &original[..original_boundary] };
    let output_markers = MARKER.find_iter(output_edge).collect::<Vec<_>>();
    let original_markers = MARKER.find_iter(original_edge).collect::<Vec<_>>();
    if output_markers.is_empty() && original_markers.is_empty() {
        return restore_boundary_whitespace(output, whitespace, trailing, styled);
    }
    let unmatched_output = output_markers.len().saturating_sub(original_markers.len());
    let unmatched_original = original_markers.len().saturating_sub(output_markers.len());
    let mut result = String::new();
    if trailing {
        result.push_str(&output[..output_boundary]);
        if unmatched_output > 0 { result.push_str(&output_edge[..output_markers[unmatched_output - 1].end()]); }
    }
    let mut cursor = 0;
    for (index, marker) in original_markers.iter().enumerate() {
        result.push_str(&original_edge[cursor..marker.start()]);
        let keep_marker = if trailing { index >= unmatched_original } else { index < original_markers.len() - unmatched_original };
        if keep_marker { result.push_str(marker.as_str()); }
        cursor = marker.end();
    }
    result.push_str(&original_edge[cursor..]);
    if !trailing {
        if unmatched_output > 0 { result.push_str(&output_edge[output_markers[output_markers.len() - unmatched_output].start()..]); }
        result.push_str(&output[output_boundary..]);
    }
    result
}

/// Reconstructs all outputs together; an error means none may be written back.
pub fn reflow(plan: &Plan, responses: &[String]) -> Result<Vec<(Location, String)>, &'static str> {
    reflow_with_cancellation(plan, responses, || false)
}

pub fn reflow_with_cancellation(
    plan: &Plan,
    responses: &[String],
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<(Location, String)>, &'static str> {
    if cancelled() { return Err("reflow cancelled"); }
    let mut work = 0usize;
    macro_rules! check_cancelled {
        () => {{
            work += 1;
            if work == 256 {
                work = 0;
                if cancelled() { return Err("reflow cancelled"); }
            }
        }};
    }
    if responses.len() != plan.items.len() { return Err("invalid response count"); }
    for ((input, output), tokens) in plan.items.iter().zip(responses).zip(&plan.protected) {
        check_cancelled!();
        if !validate_item(input, output, tokens) { return Err("unsafe response"); }
    }
    if cancelled() { return Err("reflow cancelled"); }
    let sentence_index = plan.sentence_index;
    let prefix = (sentence_index == 1).then(|| {
        let source = &plan.parts[0].source;
        let end = plan.parts[0].range.start;
        let original = &source.text[..end];
        let start = original.trim_end().len();
        let styled = source.segments.iter().any(|segment| segment.range.contains(&start) && matches!(segment.kind, SourceKind::Rich(_)));
        restore_item_boundary(&responses[0], &plan.items[0], &original[start..], true, styled)
    });
    let suffix = (sentence_index + 1 < responses.len()).then(|| {
        let last = plan.parts.last().unwrap();
        let start = last.range.end;
        let original = &last.source.text[start..];
        let end = original.len() - original.trim_start().len();
        let styled = end > 0 && last.source.segments.iter().any(|segment| segment.range.contains(&start) && matches!(segment.kind, SourceKind::Rich(_)));
        restore_item_boundary(&responses[sentence_index + 1], &plan.items[sentence_index + 1], &original[..end], false, styled)
    });
    let sentence = &responses[sentence_index];
    let tokens = &plan.protected[sentence_index];
    let mut positions = Vec::new();
    for (position, _) in sentence.char_indices() {
        check_cancelled!();
        positions.try_reserve(1).map_err(|_| "reflow allocation failed")?;
        positions.push(position);
    }
    positions.try_reserve(1).map_err(|_| "reflow allocation failed")?;
    positions.push(sentence.len());
    let mut markers = Vec::new();
    for found in MARKER.find_iter(sentence) {
        check_cancelled!();
        markers.try_reserve(1).map_err(|_| "reflow allocation failed")?;
        markers.push((found.start(), found.end()));
    }
    let mut numbers = Vec::new();
    for found in NUMERIC.find_iter(sentence) {
        check_cancelled!();
        numbers.try_reserve(1).map_err(|_| "reflow allocation failed")?;
        numbers.push((found.start(), found.end()));
    }
    let mut visible = vec![0usize; positions.len()];
    let mut content = vec![0usize; positions.len()];
    let mut whitespace = vec![0usize; positions.len()];
    let mut safe_boundary = vec![false; positions.len()];
    let mut marker_index = 0;
    let mut number_index = 0;
    for index in 0..positions.len() - 1 {
        check_cancelled!();
        let position = positions[index];
        while marker_index < markers.len() && markers[marker_index].1 <= position { marker_index += 1; }
        while number_index < numbers.len() && numbers[number_index].1 <= position { number_index += 1; }
        let marker = markers.get(marker_index).filter(|&&(start, end)| start <= position && position < end);
        let inside_marker = marker.is_some_and(|&(start, _)| start < position);
        let inside_number = numbers.get(number_index).is_some_and(|&(start, end)| start < position && position < end);
        safe_boundary[index] = position > 0 && !inside_marker && !inside_number && !cuts_word(sentence, position);
        visible[index + 1] = visible[index];
        content[index + 1] = content[index];
        whitespace[index + 1] = whitespace[index] + usize::from(sentence[position..].chars().next().unwrap().is_whitespace());
        if let Some(&(start, _)) = marker {
            if position == start && tokens[marker_index].token.ends_with("_F]]") {
                content[index + 1] += 1;
            }
        } else if !sentence[position..].chars().next().unwrap().is_whitespace() {
            visible[index + 1] += 1;
            content[index + 1] += 1;
        }
    }
    let count = positions.len();
    let part_count = plan.parts.len();
    let mut feasible = Vec::new();
    feasible.try_reserve_exact(part_count.checked_add(1).ok_or("reflow allocation failed")?)
        .map_err(|_| "reflow allocation failed")?;
    for _ in 0..=part_count {
        check_cancelled!();
        let mut row = Vec::new();
        row.try_reserve_exact(count).map_err(|_| "reflow allocation failed")?;
        row.resize(count, false);
        feasible.push(row);
    }
    feasible[part_count][count - 1] = true;
    let mut owner_ends = vec![0usize; part_count];
    let mut owner_starts = vec![sentence.len(); part_count];
    for (&(start, _), token) in markers.iter().zip(tokens) {
        check_cancelled!();
        owner_ends[token.owner] = owner_ends[token.owner].max(start + token.token.len());
        owner_starts[token.owner] = owner_starts[token.owner].min(start);
    }
    for owner in 1..part_count { check_cancelled!(); owner_ends[owner] = owner_ends[owner].max(owner_ends[owner - 1]); }
    for owner in (0..part_count - 1).rev() { check_cancelled!(); owner_starts[owner] = owner_starts[owner].min(owner_starts[owner + 1]); }
    for owner in (0..part_count).rev() {
        let mut maximum_content = None::<usize>;
        for index in (0..count - 1).rev() {
            check_cancelled!();
            if feasible[owner + 1][index + 1] {
                maximum_content = Some(maximum_content.unwrap_or(0).max(content[index + 1]));
            }
            let position = positions[index];
            let valid_start = if owner == 0 { index == 0 } else {
                safe_boundary[index] && position >= owner_ends[owner - 1] && position <= owner_starts[owner]
            };
            feasible[owner][index] = valid_start && maximum_content.is_some_and(|end| end > content[index]);
        }
    }
    if !feasible[0][0] { return Err("no safe split"); }
    let mut cuts = vec![0];
    let mut previous_index = 0;
    let total_weight = plan.weights.iter().try_fold(0usize, |total, weight| total.checked_add(*weight))
        .ok_or("reflow size limit exceeded")?;
    let mut cumulative = 0usize;
    for owner in 0..part_count - 1 {
        check_cancelled!();
        cumulative = cumulative.checked_add(plan.weights[owner]).ok_or("reflow size limit exceeded")?;
        let target = usize::try_from((visible[count - 1] as u128)
            .checked_mul(cumulative as u128).ok_or("reflow size limit exceeded")? / total_weight as u128)
            .map_err(|_| "reflow size limit exceeded")?;
        let mut best = None;
        for index in previous_index + 1..count - 1 {
            check_cancelled!();
            if feasible[owner + 1][index] && content[index] > content[previous_index] {
                let distance = visible[index].abs_diff(target);
                if best.is_none_or(|(previous_index, previous)| distance < previous ||
                    (distance == previous && whitespace[index] - whitespace[previous_index] == index - previous_index)) {
                    best = Some((index, distance));
                }
            }
        }
        let Some((index, _)) = best else { return Err("no safe split"); };
        cuts.push(positions[index]);
        previous_index = index;
    }
    cuts.push(sentence.len());
    if cuts.windows(2).any(|pair| {
        let part = &sentence[pair[0]..pair[1]];
        MARKER.replace_all(part, "").trim().is_empty()
            && !tokens.iter().any(|token| token.token.ends_with("_F]]") && part.contains(&token.token))
    }) {
        return Err("empty reflow part");
    }
    let mut output = Vec::new();
    for (index, part) in plan.parts.iter().enumerate() {
        check_cancelled!();
        let mut value = String::new();
        if index == 0 && sentence_index == 1 { value.push_str(prefix.as_ref().unwrap()); }
        else if index == 0 { value.push_str(&part.source.text[..part.range.start]); }
        value.push_str(&sentence[cuts[index]..cuts[index + 1]]);
        if index + 1 == plan.parts.len() && sentence_index + 1 < responses.len() { value.push_str(suffix.as_ref().unwrap()); }
        else if index + 1 == plan.parts.len() { value.push_str(&part.source.text[part.range.end..]); }
        for token in plan.protected.iter().flatten().filter(|token| token.owner == index) { value = value.replace(&token.token, &token.original); }
        for placeholder in &part.source.input.placeholders {
            if let InputPlaceholder::Rich(rich) = placeholder {
                value = value.replace(&format!("{}{}", rich.right, rich.left), "");
            }
        }
        if value.contains("[[BABEL_") { return Err("marker assigned to wrong object"); }
        for placeholder in &part.source.input.placeholders {
            match placeholder {
                InputPlaceholder::Formula(formula) if value.matches(&formula.token).count() != part.source.input.text.matches(&formula.token).count() => return Err("formula identity changed"),
                InputPlaceholder::Rich(rich) if value.matches(&rich.left).count() != part.source.input.text.matches(&rich.left).count() || value.matches(&rich.right).count() != part.source.input.text.matches(&rich.right).count() => return Err("style identity changed"),
                _ => {}
            }
        }
        output.push((part.source.location, value));
    }
    if cancelled() { return Err("reflow cancelled"); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translation::ordinary_translation::{FormulaInput, RichInput};

    fn plain(paragraph: usize, text: &str, range: Range<usize>) -> SelectedPart {
        SelectedPart { source: Source {
            location: Location { page: 0, paragraph }, text: text.into(),
            input: TranslateInput { text: text.into(), placeholders: vec![], original_tokens: vec![] },
            segments: vec![SourceSegment { range: 0..text.len(), kind: SourceKind::Plain }],
        }, range }
    }

    #[test]
    fn three_objects_preserve_surrounding_sentences_and_numbers() {
        let parts = vec![plain(0, "Earlier. The value", 9..18), plain(1, "is +3.5%", 0..8), plain(2, "today. Later.", 0..6)];
        let plan = build_plan(parts, vec![Join::Space, Join::Space]).unwrap();
        assert_eq!(plan.items.len(), 3);
        let output = reflow(&plan, &plan.items).unwrap();
        assert!(output[0].1.starts_with("Earlier."));
        assert!(output[2].1.ends_with(" Later."));
        assert_eq!(output.iter().map(|item| item.1.as_str()).collect::<String>(), "Earlier. The value is +3.5% today. Later.");
        let mut bad = plan.items.clone();
        bad[1] = bad[1].replace("+3.5%", "-3.5%");
        assert!(reflow(&plan, &bad).is_err());
    }

    #[test]
    fn duplicate_original_tokens_are_scoped_by_object() {
        let make = |paragraph| {
            let input = TranslateInput { text: "<s>word</s>{v}".into(), placeholders: vec![
                InputPlaceholder::Rich(RichInput { id: 0, composition_index: 0, left: "<s>".into(), right: "</s>".into(), left_pattern: String::new(), right_pattern: String::new(), source_text: "word".into() }),
                InputPlaceholder::Formula(FormulaInput { id: 0, composition_index: 1, token: "{v}".into(), pattern: String::new(), source_chars: "x".into(), hint_source: false }),
            ], original_tokens: vec![] };
            let token = format!("[[BABEL_F_0_{paragraph}_1]]");
            let text = format!("word{token}");
            let end = text.len();
            SelectedPart { source: Source { location: Location { page: 0, paragraph }, text, input, segments: vec![
                SourceSegment { range: 0..4, kind: SourceKind::Rich(0) },
                SourceSegment { range: 4..end, kind: SourceKind::Formula(1) },
            ] }, range: 0..end }
        };
        let plan = build_plan(vec![make(0), make(1)], vec![Join::Space]).unwrap();
        let output = reflow(&plan, &plan.items).unwrap();
        assert_eq!(output[0].1.trim(), "<s>word</s>{v}");
        assert_eq!(output[1].1.trim(), "<s>word</s>{v}");
        let mut bad = plan.items.clone();
        bad[0] = bad[0].replace("_0_0_F]]", "_1_0_F]]");
        assert!(reflow(&plan, &bad).is_err());
    }

    #[test]
    fn decision_json_is_strict() {
        assert!(serde_json::from_str::<Decision>(r#"{"status":"uncertain"}"#).is_ok());
        assert!(serde_json::from_str::<Decision>(r#"{"status":"none","extra":true}"#).is_err());
        assert!(serde_json::from_str::<Decision>(r#"{"status":"selected","ranges":[],"joins":[],"extra":1}"#).is_err());
        assert!(serde_json::from_str::<Decision>(r#"{"status":"none"} trailing"#).is_err());
    }
}
