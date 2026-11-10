use std::{ops::Range, sync::LazyLock};
use super::ordinary_translation::{InputPlaceholder, PlaceholderRegex, TranslateInput};

static SENTENCE_TERMINATOR: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[\p{Sentence_Terminal}…]").expect("literal sentence terminator regex"));

fn numeric_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = None;
    for (offset, character) in text.char_indices() {
        if character.is_numeric() {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            spans.push(begin..offset);
        }
    }
    if let Some(begin) = start { spans.push(begin..text.len()); }
    spans
}

/// Checks exact token counts, translator-specific patterns, and rich-text pairing.
/// Match offsets returned by PlaceholderRegex are Unicode character offsets.
pub fn validate_response<R: PlaceholderRegex>(
    regex: &mut R,
    input: &TranslateInput,
    output: &str,
    overall_patterns: &[u64],
) -> Result<bool, R::Error> {
    use std::collections::{HashMap, HashSet};
    if output.trim().is_empty() || overall_patterns.len() != 3 {
        return Ok(false);
    }
    let mut expected = HashMap::<String, usize>::new();
    let mut roles = HashMap::<String, u8>::new();
    let mut pairs = Vec::new();
    let mut token_patterns = Vec::new();
    for placeholder in &input.placeholders {
        let values = match placeholder {
            InputPlaceholder::Formula(formula) => vec![(&formula.token, &formula.pattern, 0)],
            InputPlaceholder::Rich(rich) => {
                pairs.push((&rich.left, &rich.right));
                vec![(&rich.left, &rich.left_pattern, 1), (&rich.right, &rich.right_pattern, 2)]
            }
        };
        for (token, pattern, role) in values {
            if token.is_empty() || roles.get(token).is_some_and(|previous| *previous != 2 || role != 2) {
                return Ok(false);
            }
            roles.insert(token.clone(), role);
            *expected.entry(token.clone()).or_default() += 1;
            token_patterns.push((token, pattern));
        }
    }
    for (token, pattern) in token_patterns {
        let compiled = regex.compile(pattern, true)?;
        let count = expected[token];
        for text in [&input.text, output] {
            let matches = regex.find_all(compiled, text)?;
            if matches.len() != count || matches.iter().any(|found| found.full != *token) {
                return Ok(false);
            }
        }
    }
    for (token, count) in &input.original_tokens {
        if token.is_empty() || *count == 0 || expected.contains_key(token)
            || input.text.matches(token).count() != *count || output.matches(token).count() != *count {
            return Ok(false);
        }
        expected.insert(token.clone(), *count);
    }
    let mut sequences = Vec::new();
    for text in [&input.text, output] {
        let mut spans = Vec::<(usize, usize, String)>::new();
        let mut seen = HashMap::<String, usize>::new();
        let mut unique = HashSet::new();
        for pattern in overall_patterns {
            for found in regex.find_all(*pattern, text)? {
                if found.start >= found.end || !expected.contains_key(&found.full) {
                    return Ok(false);
                }
                if unique.insert((found.start, found.end, found.full.clone())) {
                    *seen.entry(found.full.clone()).or_default() += 1;
                    spans.push((found.start, found.end, found.full));
                }
            }
        }
        if seen != expected { return Ok(false); }
        spans.sort_by_key(|span| span.0);
        if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) { return Ok(false); }
        for (left, right) in &pairs {
            let Some(left_index) = spans.iter().position(|span| &span.2 == *left) else { return Ok(false); };
            if spans.get(left_index + 1).is_none_or(|span| &span.2 != *right) { return Ok(false); }
            let Some(start) = char_offset(text, spans[left_index].1) else { return Ok(false) };
            let Some(end) = char_offset(text, spans[left_index + 1].0) else { return Ok(false) };
            if text[start..end].contains('\n') { return Ok(false); }
        }
        sequences.push(spans.into_iter().map(|span| span.2).collect::<Vec<_>>());
    }
    Ok(sequences[0] == sequences[1])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossPageInputs {
    pub left: TranslateInput,
    pub right: TranslateInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WholeSentenceMove {
    pub inputs: CrossPageInputs,
    pub sentence: String,
    pub following: Option<TranslateInput>,
    sentence_right: Option<TranslateInput>,
    following_joins_style: bool,
    left_weight: usize,
    right_weight: usize,
    left_styled: bool,
    right_styled: bool,
}

pub fn plan_whole_sentence_on_right<R: PlaceholderRegex>(
    left: &TranslateInput,
    right: &TranslateInput,
    left_protected: &[Range<usize>],
    document: &serde_json::Value,
    left_location: (usize, usize),
    right_location: (usize, usize),
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<WholeSentenceMove>, R::Error> {
    let style_equivalent = super::cross_page_style::whole_sentence_boundary_styles_equivalent(
        document, left_location.0, left_location.1, left, right_location.0, right_location.1, right,
    );
    if !style_equivalent { return Ok(None); }
    plan_whole_sentence_reflow(left, right, left_protected, patterns, regex)
}

pub fn plan_whole_sentence_reflow<R: PlaceholderRegex>(
    left: &TranslateInput,
    right: &TranslateInput,
    left_protected: &[Range<usize>],
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<WholeSentenceMove>, R::Error> {
    if !left.original_tokens.is_empty() || !right.original_tokens.is_empty()
        || !validate_response(regex, left, &left.text, patterns)?
        || !validate_response(regex, right, &right.text, patterns)? { return Ok(None); }
    if left.placeholders.is_empty() && right.placeholders.is_empty() {
        let Some((tail, continuation)) = super::cross_page::context_sentence_parts(&left.text, &right.text) else { return Ok(None) };
        if right.text.strip_prefix(continuation).is_none_or(|remaining| !remaining.trim().is_empty()) { return Ok(None); }
        let Some(sentence) = super::cross_page::extract_context_sentence(&left.text, &right.text) else { return Ok(None) };
        if tail.ends_with('-') || right.text.starts_with(char::is_whitespace) { return Ok(None); }
        let Some(prefix) = left.text.trim_end().strip_suffix(tail) else { return Ok(None) };
        if !prefix.chars().any(char::is_alphanumeric) { return Ok(None); }
        let start = prefix.len();
        if left_protected.iter().any(|range| range.start >= range.end || range.end > left.text.len()
            || !left.text.is_char_boundary(range.start) || !left.text.is_char_boundary(range.end)
            || (range.start < left.text.len() && range.end > start)) { return Ok(None); }
        let mut moved_left = left.clone();
        moved_left.text = prefix.to_owned();
        let mut moved_right = right.clone();
        moved_right.text = format!("{tail} {}", right.text);
        if !validate_response(regex, &moved_left, &moved_left.text, patterns)?
            || !validate_response(regex, &moved_right, &moved_right.text, patterns)? { return Ok(None); }
        return Ok(Some(WholeSentenceMove {
            inputs: CrossPageInputs { left: moved_left, right: moved_right }, sentence,
            following: None, sentence_right: None, following_joins_style: false,
            left_weight: tail.chars().count(), right_weight: continuation.chars().count(), left_styled: false, right_styled: false,
        }));
    }
    let (Some(InputPlaceholder::Rich(left_rich)), Some(InputPlaceholder::Rich(right_rich))) =
        (left.placeholders.last(), right.placeholders.first()) else { return Ok(None) };
    if right.placeholders.iter().any(|placeholder| matches!(placeholder, InputPlaceholder::Formula(_))) {
        return Ok(None);
    }
    if left.placeholders.iter().chain(&right.placeholders).any(|placeholder| match placeholder {
        InputPlaceholder::Formula(formula) => formula.token.contains(['<', '>']),
        InputPlaceholder::Rich(rich) => {
            let is_tag = |value: &str| value.starts_with('<') && value.ends_with('>');
            !is_tag(&rich.left) || !is_tag(&rich.right) || rich.left == rich.right
        },
    }) { return Ok(None); }
    let Some(left_open) = left.text.rfind(&left_rich.left) else { return Ok(None) };
    let left_start = left_open + left_rich.left.len();
    let Some(left_content) = left.text[left_start..].strip_suffix(&left_rich.right) else { return Ok(None) };
    let Some(right_content) = right.text.strip_prefix(&right_rich.left) else { return Ok(None) };
    if right_content.starts_with(char::is_whitespace) { return Ok(None); }
    let mut right_plain = right.text.clone();
    let mut right_tag_spans = Vec::new();
    let mut right_position = 0;
    for placeholder in &right.placeholders {
        if let InputPlaceholder::Rich(rich) = placeholder {
            let Some(open_offset) = right.text[right_position..].find(&rich.left) else { return Ok(None) };
            let open = right_position + open_offset;
            let open_end = open + rich.left.len();
            let Some(close_offset) = right.text[open_end..].find(&rich.right) else { return Ok(None) };
            let close = open_end + close_offset;
            right_position = close + rich.right.len();
            right_tag_spans.push(open..open_end);
            right_tag_spans.push(close..right_position);
            right_plain = right_plain.replace(&rich.left, "").replace(&rich.right, "");
        }
    }
    for pattern in patterns {
        for token in regex.find_all(*pattern, &right.text)? {
            let Some(start) = char_offset(&right.text, token.start) else { return Ok(None) };
            let Some(end) = char_offset(&right.text, token.end) else { return Ok(None) };
            if !right_tag_spans.iter().any(|span| span.start == start && span.end == end) {
                return Ok(None);
            }
        }
    }
    let Some((tail, continuation)) = super::cross_page::context_sentence_parts(left_content, &right_plain) else { return Ok(None) };
    if !continuation.ends_with(['.', '!', '?', '。', '！', '？']) { return Ok(None); }
    let Some(first_style_end) = right_content.find(&right_rich.right) else { return Ok(None) };
    let first_style = &right_content[..first_style_end];
    let Some(after_continuation) = first_style.strip_prefix(continuation) else { return Ok(None) };
    if !after_continuation.trim().is_empty() { return Ok(None); }
    let Some(sentence) = super::cross_page::extract_context_sentence(left_content, &right_plain) else { return Ok(None) };
    if tail.ends_with('-') || tail.chars().chain(continuation.chars()).any(|character| matches!(character, '<' | '>' | '{' | '}' | '\n' | '\r')) {
        return Ok(None);
    }
    for pattern in patterns {
        if !regex.find_all(*pattern, &sentence)?.is_empty() { return Ok(None); }
    }
    let Some(before_tail) = left_content.trim_end().strip_suffix(tail) else { return Ok(None) };
    if !before_tail.chars().any(char::is_alphanumeric) { return Ok(None); }
    let tail_start = left_start + before_tail.len();
    let tail_end = left_start + left_content.trim_end().len();
    let rich_span = left_open..left.text.len();
    if left_protected.iter().any(|range| range.start >= range.end || range.end > left.text.len()
        || !left.text.is_char_boundary(range.start) || !left.text.is_char_boundary(range.end)
        || (range.start < tail_end && range.end > tail_start && *range != rich_span)) { return Ok(None); }
    let mut moved_left = left.clone();
    moved_left.text = format!("{}{}{}", &left.text[..left_start], before_tail, left_rich.right);
    let mut moved_right = right.clone();
    moved_right.text = format!("{}{tail} {right_content}", right_rich.left);
    if !validate_response(regex, &moved_left, &moved_left.text, patterns)?
        || !validate_response(regex, &moved_right, &moved_right.text, patterns)? { return Ok(None); }
    Ok(Some(WholeSentenceMove {
        inputs: CrossPageInputs { left: moved_left, right: moved_right }, sentence,
        following: None, sentence_right: None, following_joins_style: false,
        left_weight: tail.chars().count(), right_weight: continuation.chars().count(), left_styled: true, right_styled: true,
    }))
}

pub fn plan_plain_to_styled_sentence<R: PlaceholderRegex>(
    left: &TranslateInput,
    right: &TranslateInput,
    left_protected: &[Range<usize>],
    sentence: &str,
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<WholeSentenceMove>, R::Error> {
    if !left.original_tokens.is_empty() || !right.original_tokens.is_empty()
        || left.placeholders.iter().any(|item| !matches!(item, InputPlaceholder::Formula(_)))
        || !validate_response(regex, left, &left.text, patterns)?
        || !validate_response(regex, right, &right.text, patterns)? { return Ok(None); }
    let Some(InputPlaceholder::Rich(right_rich)) = right.placeholders.first() else { return Ok(None) };
    if !right_rich.left.starts_with('<') || !right_rich.left.ends_with('>')
        || !right_rich.right.starts_with('<') || !right_rich.right.ends_with('>')
        || right_rich.left == right_rich.right {
        return Ok(None);
    }
    let Some(right_content) = right.text.strip_prefix(&right_rich.left) else { return Ok(None) };
    let Some(close) = right_content.find(&right_rich.right) else { return Ok(None) };
    let first_style = &right_content[..close];
    let mut matching = None;
    for (offset, character) in sentence.char_indices() {
        if !character.is_whitespace() { continue; }
        let tail = sentence[..offset].trim_end();
        let continuation = sentence[offset..].trim_start();
        if tail.is_empty() || continuation.is_empty() { continue; }
        if left.text.trim_end().ends_with(tail) && first_style.starts_with(continuation) {
            if matching.is_some() { return Ok(None); }
            matching = Some((tail, continuation));
        }
    }
    let Some((tail, continuation)) = matching else { return Ok(None) };
    if tail.ends_with('-') || !continuation.ends_with(['.', '!', '?', '。', '！', '？']) {
        return Ok(None);
    }
    if !first_style[continuation.len()..].trim().is_empty() {
        return Ok(None);
    }
    for pattern in patterns {
        if !regex.find_all(*pattern, sentence)?.is_empty() { return Ok(None); }
    }
    let Some(prefix) = left.text.trim_end().strip_suffix(tail) else { return Ok(None) };
    if !prefix.chars().any(char::is_alphanumeric) { return Ok(None); }
    let start = prefix.len();
    if left_protected.iter().any(|range| range.start >= range.end || range.end > left.text.len()
        || !left.text.is_char_boundary(range.start) || !left.text.is_char_boundary(range.end)
        || (range.start < left.text.len() && range.end > start)) { return Ok(None); }
    let mut moved_left = left.clone();
    moved_left.text = prefix.to_owned();
    let mut moved_right = right.clone();
    moved_right.text = format!("{}{tail} {right_content}", right_rich.left);
    if !validate_response(regex, &moved_left, &moved_left.text, patterns)?
        || !validate_response(regex, &moved_right, &moved_right.text, patterns)? { return Ok(None); }
    Ok(Some(WholeSentenceMove {
        inputs: CrossPageInputs { left: moved_left, right: moved_right }, sentence: sentence.to_owned(),
        following: None, sentence_right: None, following_joins_style: false,
        left_weight: tail.chars().count(), right_weight: continuation.chars().count(),
        left_styled: false, right_styled: true,
    }))
}

pub fn plan_partitioned_sentence<R: PlaceholderRegex>(
    left: &TranslateInput,
    right: &TranslateInput,
    left_protected: &[Range<usize>],
    sentence: &str,
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<WholeSentenceMove>, R::Error> {
    if !left.original_tokens.is_empty() || !right.original_tokens.is_empty()
        || !validate_response(regex, left, &left.text, patterns)?
        || !validate_response(regex, right, &right.text, patterns)? { return Ok(None); }
    let left_text = match left.placeholders.last() {
        Some(InputPlaceholder::Rich(rich)) => match left.text.strip_suffix(&rich.right) {
            Some(text) => text,
            None => return Ok(None),
        },
        _ => left.text.as_str(),
    };
    let (right_content, right_close, rich) = match right.placeholders.first() {
        Some(InputPlaceholder::Rich(rich)) => {
            let Some(content) = right.text.strip_prefix(&rich.left) else { return Ok(None) };
            let Some(close) = content.find(&rich.right) else { return Ok(None) };
            (content, close, Some(rich))
        }
        None => (right.text.as_str(), right.text.len(), None),
        _ => return Ok(None),
    };
    let first_style = &right_content[..right_close];
    let source_sentence = if rich.is_some() {
        let mut plain = right.text.clone();
        for placeholder in &right.placeholders {
            if let InputPlaceholder::Rich(rich) = placeholder {
                plain = plain.replace(&rich.left, "").replace(&rich.right, "");
            }
        }
        super::cross_page::extract_context_sentence(left_text, &plain)
    } else {
        super::cross_page::extract_context_sentence(left_text, &right.text)
    };
    if source_sentence.as_deref() != Some(sentence) { return Ok(None); }
    let mut continuation = None;
    for (offset, character) in sentence.char_indices() {
        if !character.is_whitespace() { continue; }
        let tail = sentence[..offset].trim_end();
        let rest = sentence[offset..].trim_start();
        if !tail.is_empty() && !rest.is_empty()
            && left_text.trim_end().ends_with(tail) && first_style.starts_with(rest) {
            if continuation.is_some() { return Ok(None); }
            continuation = Some(rest);
        }
    }
    let Some(continuation) = continuation else { return Ok(None) };
    let inside_suffix = &first_style[continuation.len()..];
    let (sentence_right, following, following_joins_style) = if let Some(rich) = rich {
        let after_style = &right_content[right_close + rich.right.len()..];
        if inside_suffix.trim().is_empty() && after_style.trim().is_empty() { return Ok(None); }
        let mut sentence_right = right.clone();
        sentence_right.text = format!("{}{continuation}{}", rich.left, rich.right);
        sentence_right.placeholders = vec![InputPlaceholder::Rich(rich.clone())];
        let mut following = right.clone();
        let joins_style = !inside_suffix.trim().is_empty();
        if joins_style {
            following.text = format!("{}{inside_suffix}{}{after_style}", rich.left, rich.right);
        } else {
            following.text = format!("{inside_suffix}{after_style}");
            following.placeholders.remove(0);
        }
        (sentence_right, following, joins_style)
    } else {
        if inside_suffix.trim().is_empty() { return Ok(None); }
        let mut sentence_right = right.clone();
        sentence_right.text = continuation.to_owned();
        let mut following = right.clone();
        following.text = inside_suffix.to_owned();
        (sentence_right, following, false)
    };
    if !validate_response(regex, &following, &following.text, patterns)? { return Ok(None); }
    let plan = plan_whole_sentence_reflow(left, &sentence_right, left_protected, patterns, regex)?;
    let plan = match plan {
        Some(plan) => Some(plan),
        None => {
            let right_isolated = sentence_right.placeholders.len() == 1
                && matches!(sentence_right.placeholders.first(), Some(InputPlaceholder::Rich(_)));
            if !right_isolated { return Ok(None); }
            plan_plain_to_styled_sentence(left, &sentence_right, left_protected, sentence, patterns, regex)?
        },
    };
    let Some(mut plan) = plan else { return Ok(None) };
    if plan.sentence != sentence { return Ok(None); }
    plan.following = Some(following);
    plan.sentence_right = Some(sentence_right);
    plan.following_joins_style = following_joins_style;
    Ok(Some(plan))
}

pub fn reflow_partitioned_response<R: PlaceholderRegex>(
    plan: &WholeSentenceMove,
    left_output: &str,
    sentence_output: &str,
    following_output: &str,
    original_left: &TranslateInput,
    original_right: &TranslateInput,
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<(String, String)>, R::Error> {
    let (Some(sentence_right), Some(following)) = (&plan.sentence_right, &plan.following) else { return Ok(None) };
    if !validate_response(regex, following, following_output, patterns)? { return Ok(None); }
    let Some((left, right)) = reflow_whole_sentence_response(
        plan, left_output, sentence_output, original_left, sentence_right, patterns, regex,
    )? else { return Ok(None) };
    let right = if plan.following_joins_style {
        let Some(InputPlaceholder::Rich(rich)) = sentence_right.placeholders.first() else { return Ok(None) };
        let Some(before) = right.strip_suffix(&rich.right) else { return Ok(None) };
        let Some(after) = following_output.strip_prefix(&rich.left) else { return Ok(None) };
        let separator = if before.chars().last().is_some_and(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '!' | '?' | ':' | ';' | ','))
            && after.chars().next().is_some_and(|value| value.is_ascii_alphanumeric()) { " " } else { "" };
        format!("{before}{separator}{after}")
    } else {
        let last = if plan.right_styled {
            let Some(InputPlaceholder::Rich(rich)) = sentence_right.placeholders.first() else { return Ok(None) };
            let Some(content) = right.strip_suffix(&rich.right) else { return Ok(None) };
            content.chars().last()
        } else { right.chars().last() };
        let separator = if last.is_some_and(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '!' | '?' | ':' | ';' | ','))
            && following_output.chars().next().is_some_and(|value| value.is_ascii_alphanumeric()) { " " } else { "" };
        format!("{right}{separator}{following_output}")
    };
    if !validate_response(regex, original_left, &left, patterns)?
        || !validate_response(regex, original_right, &right, patterns)? { return Ok(None); }
    Ok(Some((left, right)))
}

pub fn reflow_whole_sentence_response<R: PlaceholderRegex>(
    plan: &WholeSentenceMove,
    left_output: &str,
    right_output: &str,
    original_left: &TranslateInput,
    original_right: &TranslateInput,
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<(String, String)>, R::Error> {
    if !validate_response(regex, &plan.inputs.left, left_output, patterns)?
        || !validate_response(regex, &plan.inputs.right, right_output, patterns)? { return Ok(None); }
    let (left_prefix, left_close) = if plan.left_styled {
        let Some(InputPlaceholder::Rich(left_rich)) = original_left.placeholders.last() else { return Ok(None) };
        let Some(left_prefix) = left_output.strip_suffix(&left_rich.right) else { return Ok(None) };
        (left_prefix, left_rich.right.as_str())
    } else {
        (left_output, "")
    };
    let (right_open, right_content, close) = if plan.right_styled {
        let Some(InputPlaceholder::Rich(right_rich)) = original_right.placeholders.first() else { return Ok(None) };
        let Some(right_content) = right_output.strip_prefix(&right_rich.left) else { return Ok(None) };
        let Some(close) = right_content.find(&right_rich.right) else { return Ok(None) };
        (right_rich.left.as_str(), right_content, close)
    } else {
        ("", right_output, right_output.len())
    };
    let content = &right_content[..close];
    let Some(weight) = plan.left_weight.checked_add(plan.right_weight) else { return Ok(None) };
    if weight == 0 { return Ok(None); }
    let mut sentence_length = content.chars().count();
    let mut candidates = Vec::new();
    let output_numbers = numeric_spans(content);
    let mut number_index = 0;
    let mut depth = 0usize;
    let mut sentence_end = None;
    let Some((first_alphanumeric, _)) = content.char_indices().find(|(_, value)| value.is_alphanumeric()) else { return Ok(None) };
    let Some((last_alphanumeric, _)) = content.char_indices().rfind(|(_, value)| value.is_alphanumeric()) else { return Ok(None) };
    let mut terminators = SENTENCE_TERMINATOR.find_iter(content).peekable();
    for (position, (offset, character)) in content.char_indices().enumerate() {
        if terminators.peek().is_some_and(|found| found.start() == offset) {
            terminators.next();
            let before = &content[..offset];
            let after = &content[offset + character.len_utf8()..];
            let decimal = character == '.'
                && before.chars().next_back().is_some_and(|value| value.is_ascii_digit())
                && after.chars().next().is_some_and(|value| value.is_ascii_digit());
            if !decimal {
                let word = before.rsplit(|value: char| !value.is_alphabetic()).next().unwrap_or("");
                let figure_reference = character == '.'
                    && matches!(word.to_ascii_lowercase().as_str(), "fig" | "figs")
                    && after.starts_with(char::is_whitespace)
                    && after.trim_start().chars().next().is_some_and(|value| value.is_ascii_digit());
                let abbreviation = character == '.' && (word.chars().count() == 1
                    || matches!(word, "Dr" | "Mr" | "Mrs" | "Ms" | "Prof")
                    || matches!(word.to_ascii_lowercase().as_str(), "al" | "assoc" | "dept" | "approx" | "fig" | "figs" | "etc" | "vs"));
                if depth > 0 {
                    if !figure_reference { return Ok(None); }
                } else if abbreviation {
                    return Ok(None);
                } else {
                    sentence_length = position + 1;
                    sentence_end = Some(offset + character.len_utf8());
                    break;
                }
            }
        }
        if position > 0 && depth == 0 {
            let previous = content[..offset].chars().next_back().expect("nonempty prefix");
            let word_boundary = previous.is_whitespace() || character.is_whitespace()
                || is_ideograph(previous) || is_ideograph(character);
            let bad_start = matches!(character, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '，' | '。' | '；' | '：' | '！' | '？' | '）' | '】' | '、' | '%' | '％');
            let bad_end = matches!(previous, '(' | '[' | '{' | '（' | '【' | '“' | '‘');
            let inside_word = previous.is_ascii_alphanumeric() && character.is_ascii_alphanumeric();
            while output_numbers.get(number_index).is_some_and(|span| span.end <= offset) { number_index += 1; }
            let inside_number = output_numbers.get(number_index).is_some_and(|span| span.start < offset && offset < span.end);
            if word_boundary && !bad_start && !bad_end && !inside_word && !inside_number
                && first_alphanumeric < offset && last_alphanumeric >= offset {
                candidates.push((position, offset));
            }
        }
        match character {
            '(' | '[' | '（' | '【' => depth += 1,
            ')' | ']' | '）' | '】' => depth = depth.saturating_sub(1),
            _ => {},
        }
    }
    let Some(sentence_end) = sentence_end else { return Ok(None) };
    if !content[sentence_end..].trim().is_empty() { return Ok(None); }
    let Some(source_boundary) = plan.sentence.char_indices().nth(plan.left_weight).map(|(offset, _)| offset) else { return Ok(None) };
    let source_spans = numeric_spans(&plan.sentence);
    let target_spans = numeric_spans(&content[..sentence_end]);
    let mut numeric_boundary = None;
    if !source_spans.is_empty() {
        if source_spans.iter().map(|span| &plan.sentence[span.clone()])
            .ne(target_spans.iter().map(|span| &content[span.clone()])) { return Ok(None); }
        let left_numbers = source_spans.iter().take_while(|span| span.end <= source_boundary).count();
        if source_spans.iter().any(|span| span.start < source_boundary && span.end > source_boundary) { return Ok(None); }
        let minimum = left_numbers.checked_sub(1).map_or(0, |index| target_spans[index].end);
        let maximum = target_spans.get(left_numbers).map_or(sentence_end, |span| span.start);
        numeric_boundary = Some(minimum..=maximum);
    }
    let target = sentence_length.saturating_mul(plan.left_weight) / weight;
    candidates.sort_unstable_by_key(|(position, offset)| (position.abs_diff(target), *offset));
    for (_, offset) in candidates {
        if numeric_boundary.as_ref().is_some_and(|range| !range.contains(&offset)) { continue; }
        let first = &content[..offset];
        let rest = &right_content[offset..];
        let separator = if left_prefix.chars().last().is_some_and(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '!' | '?' | ':' | ';' | ','))
            && first.chars().next().is_some_and(|character| character.is_ascii_alphanumeric()) { " " } else { "" };
        let left = format!("{left_prefix}{separator}{first}{left_close}");
        let right = format!("{right_open}{rest}");
        if validate_response(regex, original_left, &left, patterns)?
            && validate_response(regex, original_right, &right, patterns)? {
            return Ok(Some((left, right)));
        }
    }
    Ok(None)
}

pub const STYLE_BOUNDARY: &str = "ZXQCROSSPAGESTYLEBOUNDARYQXZ";

pub fn move_styled_prefix(left: &TranslateInput, right: &TranslateInput, protected: &[Range<usize>]) -> Option<CrossPageInputs> {
    if left.text.contains(STYLE_BOUNDARY) || right.text.contains(STYLE_BOUNDARY) { return None; }
    let left_end = left.text.trim_end().chars().last()?;
    if matches!(left_end, '.' | '!' | '?' | '。' | '！' | '？' | '-' | '‐' | '‑' | '\u{ad}') { return None; }
    let rich = match right.placeholders.first()? {
        InputPlaceholder::Rich(rich) => rich,
        _ => return None,
    };
    let content = right.text.strip_prefix(&rich.left)?;
    let close = content.find(&rich.right)?;
    let styled = &content[..close];
    let (index, punctuation) = styled.char_indices().find(|(_, character)| matches!(character, '.' | '!' | '?' | '。' | '！' | '？'))?;
    let end = index + punctuation.len_utf8();
    let before = &styled[..index];
    if before.trim().is_empty() { return None; }
    if punctuation == '.' {
        let word = before.rsplit(|character: char| !character.is_alphabetic()).next()?;
        if word.chars().count() <= 3 || before.chars().last()?.is_ascii_digit() { return None; }
    }
    if !styled[end..].chars().next()?.is_whitespace() { return None; }
    let split = end + styled[end..].char_indices().find(|(_, character)| !character.is_whitespace())?.0;
    let prefix = &styled[..split];
    if prefix.chars().any(|character| matches!(character, '<' | '>' | '`' | '*' | '_' | '[' | ']' | '#' | '\\' | '~' | '|'))
        || matches!(styled[split..].chars().next()?, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '）' | '】' | '」' | '』') { return None; }
    let prefix_end = rich.left.len() + split;
    let span_end = rich.left.len() + close + rich.right.len();
    if protected.iter().any(|range| range.start < prefix_end && !((range.start == 0 && range.end == rich.left.len()) || (range.start == 0 && range.end == span_end))) { return None; }
    let mut moved_left = left.clone();
    moved_left.text.push(' ');
    moved_left.text.push_str(STYLE_BOUNDARY);
    moved_left.text.push_str(prefix);
    let mut moved_right = right.clone();
    moved_right.text = format!("{}{}", rich.left, &content[split..]);
    Some(CrossPageInputs { left: moved_left, right: moved_right })
}

/// Ranges must cover all translator-specific matches and complete styled spans.
/// This checks structural safety, not sentence continuity or translation quality.
pub fn move_sentence_prefix(left: &TranslateInput, right: &TranslateInput, protected: &[Range<usize>]) -> Option<CrossPageInputs> {
    let left_end = left.text.trim_end().chars().last()?;
    if matches!(left_end, '.' | '!' | '?' | '。' | '！' | '？' | '-' | '‐' | '‑' | '\u{ad}')
        || right.text.chars().next()?.is_whitespace() { return None; }
    if protected.iter().any(|range| range.start >= range.end || range.end > right.text.len()
        || !right.text.is_char_boundary(range.start) || !right.text.is_char_boundary(range.end)) { return None; }
    let mut ranges = protected.to_vec();
    let mut tokens: Vec<&str> = right.original_tokens.iter().map(|(token, _)| token.as_str()).collect();
    for placeholder in &right.placeholders {
        match placeholder {
            InputPlaceholder::Formula(formula) => tokens.push(&formula.token),
            InputPlaceholder::Rich(rich) => { tokens.push(&rich.left); tokens.push(&rich.right); }
        }
    }
    for token in tokens {
        if token.is_empty() || !right.text.contains(token) { return None; }
        ranges.extend(right.text.match_indices(token).map(|(start, value)| start..start + value.len()));
    }
    let (index, punctuation) = right.text.char_indices().find(|(_, character)| matches!(character, '.' | '!' | '?' | '。' | '！' | '？'))?;
    let end = index + punctuation.len_utf8();
    let before = &right.text[..index];
    if before.trim().is_empty() { return None; }
    if punctuation == '.' {
        let word = before.rsplit(|character: char| !character.is_alphabetic()).next()?;
        if word.chars().count() <= 3 || before.chars().last()?.is_ascii_digit() { return None; }
    }
    if !right.text[end..].chars().next()?.is_whitespace() { return None; }
    let split = right.text[end..].char_indices().find(|(_, character)| !character.is_whitespace()).map(|(offset, _)| end + offset)?;
    if matches!(right.text[split..].chars().next()?, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '）' | '】' | '」' | '』') { return None; }
    let prefix = &right.text[..split];
    if prefix.chars().any(|character| matches!(character, '<' | '>' | '`' | '*' | '_' | '[' | ']' | '#' | '\\' | '~' | '|'))
        || ranges.iter().any(|range| range.start < split) { return None; }
    let first = prefix.chars().next()?;
    let separator = if !left.text.ends_with(char::is_whitespace) && left_end.is_ascii_alphanumeric() && first.is_ascii_alphanumeric() { " " } else { "" };
    let mut left_result = left.clone();
    left_result.text.push_str(separator);
    left_result.text.push_str(prefix.trim_end());
    let mut right_result = right.clone();
    right_result.text = right.text[split..].to_owned();
    Some(CrossPageInputs { left: left_result, right: right_result })
}

pub fn redistribute_response<R: PlaceholderRegex>(
    moved_left: &TranslateInput,
    left_output: &str,
    moved_right: &TranslateInput,
    right_output: &str,
    original_left: &TranslateInput,
    original_right: &TranslateInput,
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<(String, String)>, R::Error> {
    if !validate_response(regex, moved_left, left_output, patterns)?
        || !validate_response(regex, moved_right, right_output, patterns)?
    {
        return Ok(None);
    }
    if let Some(moved) = moved_left.text.strip_prefix(&original_left.text).and_then(|value| value.strip_prefix(&format!(" {STYLE_BOUNDARY}"))) {
        let Some(InputPlaceholder::Rich(rich)) = original_right.placeholders.first() else { return Ok(None) };
        let Some(original_content) = original_right.text.strip_prefix(&rich.left) else { return Ok(None) };
        let Some(remaining_content) = moved_right.text.strip_prefix(&rich.left) else { return Ok(None) };
        if original_content.strip_suffix(remaining_content) != Some(moved) { return Ok(None); }
        let Some((left, continued)) = left_output.split_once(STYLE_BOUNDARY) else { return Ok(None) };
        if continued.contains(STYLE_BOUNDARY) || right_output.contains(STYLE_BOUNDARY) { return Ok(None); }
        let Some(right_content) = right_output.strip_prefix(&rich.left) else { return Ok(None) };
        if left.trim().is_empty() || continued.trim().is_empty() { return Ok(None); }
        let separator = if continued.chars().last().is_some_and(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '!' | '?' | ':' | ';' | ','))
            && right_content.chars().next().is_some_and(|character| character.is_ascii_alphanumeric()) { " " } else { "" };
        let right = format!("{}{continued}{separator}{right_content}", rich.left);
        if validate_response(regex, original_left, left, patterns)? && validate_response(regex, original_right, &right, patterns)? {
            return Ok(Some((left.to_owned(), right)));
        }
        return Ok(None);
    }
    let Some(moved_prefix) = moved_left.text.strip_prefix(&original_left.text) else {
        return Ok(None);
    };
    let Some(source_prefix) = original_right.text.strip_suffix(&moved_right.text) else {
        return Ok(None);
    };
    if moved_prefix.trim() != source_prefix.trim()
        || moved_prefix.trim().is_empty()
        || original_left.text.is_empty()
        || original_right.text.is_empty()
    {
        return Ok(None);
    }
    let mut protected = Vec::new();
    let mut earliest_split = 0;
    for pattern in patterns {
        for found in regex.find_all(*pattern, left_output)? {
            let Some(start) = char_offset(left_output, found.start) else { return Ok(None) };
            let Some(end) = char_offset(left_output, found.end) else { return Ok(None) };
            if start >= end || left_output.get(start..end) != Some(found.full.as_str()) {
                return Ok(None);
            }
            earliest_split = earliest_split.max(end);
            protected.push(start..end);
        }
    }
    for placeholder in &moved_left.placeholders {
        if let InputPlaceholder::Rich(rich) = placeholder {
            let left_pattern = regex.compile(&rich.left_pattern, true)?;
            let right_pattern = regex.compile(&rich.right_pattern, true)?;
            let left = regex.find_all(left_pattern, left_output)?;
            let right = regex.find_all(right_pattern, left_output)?;
            if left.len() != 1 {
                return Ok(None);
            }
            let Some(right) = right.iter().find(|item| item.start >= left[0].end) else { return Ok(None) };
            let Some(start) = char_offset(left_output, left[0].start) else { return Ok(None) };
            let Some(end) = char_offset(left_output, right.end) else { return Ok(None) };
            protected.push(start..end);
        }
    }
    let total = left_output.chars().count();
    let left_weight = original_left.text.chars().count();
    let right_weight = source_prefix.chars().count();
    let sentence_count = original_left.text.char_indices().filter(|(index, character)| {
        matches!(character, '.' | '!' | '?' | '。' | '！' | '？')
            && original_left.text[index + character.len_utf8()..].chars().next().is_some_and(char::is_whitespace)
    }).count();
    let target = total.saturating_mul(left_weight) / (left_weight + right_weight);
    if sentence_count > 0 {
        let end = left_output.char_indices().filter_map(|(index, character)| {
            let end = index + character.len_utf8();
            (matches!(character, '.' | '!' | '?' | '。' | '！' | '？')
                && !protected.iter().any(|range| range.start < end && end <= range.end))
                .then_some(end)
        }).nth(sentence_count - 1);
        let Some(end) = end else { return Ok(None) };
        earliest_split = earliest_split.max(end);
    }
    let mut candidates = Vec::new();
    for (position, (offset, character)) in left_output.char_indices().enumerate().skip(1) {
        if offset < earliest_split || (position > target && earliest_split == 0) || position * 2 < target || position * 10 > target * 13 || protected.iter().any(|range| range.start < offset && offset < range.end) {
            continue;
        }
        let previous = left_output[..offset].chars().next_back().unwrap();
        if !(previous.is_whitespace() || character.is_whitespace()
            || is_ideograph(previous) || is_ideograph(character)
            || matches!(previous, ',' | ';' | ':' | '，' | '；' | '：' | '、' | '。' | '！' | '？'))
            || matches!(character, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '，' | '。' | '；' | '：' | '！' | '？' | '）' | '】' | '、')
        {
            continue;
        }
        candidates.push((position, offset));
    }
    candidates.sort_by_key(|(position, _)| position.abs_diff(target));
    for (_, offset) in candidates {
        let left = left_output[..offset].to_owned();
        let continued = &left_output[offset..];
        if left.trim().is_empty() || continued.trim().is_empty() {
            continue;
        }
        let separator = if continued.chars().last().is_some_and(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '!' | '?' | ':' | ';' | ','))
            && right_output.chars().next().is_some_and(|character| character.is_ascii_alphanumeric()) { " " } else { "" };
        let right = format!("{continued}{separator}{right_output}");
        if validate_response(regex, original_left, &left, patterns)?
            && validate_response(regex, original_right, &right, patterns)?
        {
            return Ok(Some((left, right)));
        }
    }
    Ok(None)
}

fn char_offset(text: &str, offset: usize) -> Option<usize> {
    if offset == text.chars().count() { Some(text.len()) } else { text.char_indices().nth(offset).map(|(index, _)| index) }
}

fn is_ideograph(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2fa1f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ordinary_translation::{FormulaInput, RegexMatch, RichInput};
    #[derive(Default)]
    struct TestRegex(Vec<regex::Regex>);
    impl PlaceholderRegex for TestRegex {
        type Error = regex::Error;
        fn compile(&mut self, pattern: &str, ignore_case: bool) -> Result<u64, Self::Error> {
            let compiled = regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build()?;
            self.0.push(compiled);
            Ok((self.0.len() - 1) as u64)
        }
        fn matches(&mut self, pattern: u64, text: &str) -> Result<bool, Self::Error> {
            Ok(self.0[pattern as usize].is_match(text))
        }
        fn find_all(&mut self, pattern: u64, text: &str) -> Result<Vec<RegexMatch>, Self::Error> {
            Ok(self.0[pattern as usize].find_iter(text).map(|item| RegexMatch {
                full: item.as_str().to_owned(),
                start: text[..item.start()].chars().count(),
                end: text[..item.end()].chars().count(),
                groups: vec![],
            }).collect())
        }
    }
    fn input(text: &str) -> TranslateInput {
        TranslateInput { text: text.into(), placeholders: vec![], original_tokens: vec![] }
    }
    #[test]
    fn moves_complete_sentence_and_keeps_metadata() {
        let mut right = input("continued here. Next {x} follows.");
        right.original_tokens.push(("{x}".into(), 1));
        let result = move_sentence_prefix(&input("The sentence"), &right, &[]).unwrap();
        assert_eq!(result.left.text, "The sentence continued here.");
        assert_eq!(result.right.text, "Next {x} follows.");
        assert_eq!(result.right.original_tokens, right.original_tokens);
    }
    #[test]
    fn rejects_ambiguous_or_unsafe_boundaries() {
        for right in ["Dr. Smith continues. Next.", "3.14 is pi. Next.", "word.\" Next.", "word. ) Next.", "<b>word</b>. Next.", "word.", "word.   ", " word. Next."] {
            assert!(move_sentence_prefix(&input("unfinished"), &input(right), &[]).is_none(), "{right}");
        }
        assert!(move_sentence_prefix(&input("unfinished"), &input("one token. Next."), &[4..9]).is_none());
        assert!(move_sentence_prefix(&input("finished."), &input("word here. Next."), &[]).is_none());
    }
    #[test]
    fn supports_multibyte_and_rejects_invalid_ranges() {
        let right = input("继续完成。 下一句。");
        let result = move_sentence_prefix(&input("前面的句子"), &right, &[]).unwrap();
        assert_eq!(result.left.text, "前面的句子继续完成。");
        assert_eq!(result.right.text, "下一句。");
        assert!(move_sentence_prefix(&input("前文"), &right, &[1..3]).is_none());
    }
    #[test]
    fn rejects_original_token_in_prefix() {
        let mut right = input("one {token} here. Next.");
        right.original_tokens.push(("{token}".into(), 1));
        assert!(move_sentence_prefix(&input("unfinished"), &right, &[]).is_none());
    }
    #[test]
    fn validates_exact_formula_and_rejects_hallucinations() {
        let mut source = input("Start {v1} end");
        source.placeholders.push(InputPlaceholder::Formula(FormulaInput {
            id: 1, composition_index: 0, token: "{v1}".into(), pattern: r"\{v1\}".into(),
            source_chars: "x".into(), hint_source: false,
        }));
        let mut regex = TestRegex::default();
        let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()];
        for (output, expected) in [("译 {v1} 文", true), ("译 文", false), ("{v1} {v1}", false), ("{v1} {v2}", false), ("", false)] {
            assert_eq!(validate_response(&mut regex, &source, output, &patterns).unwrap(), expected, "{output}");
        }
    }
    #[test]
    fn deduplicates_identical_overall_matches_and_rejects_extra_original_tokens() {
        let mut source = input("{v1} text");
        source.original_tokens.push(("{v1}".into(), 1));
        let mut regex = TestRegex::default();
        let formula = regex.compile(r"\{v\d+\}", false).unwrap();
        let unused = regex.compile("NEVERMATCH", false).unwrap();
        assert!(validate_response(&mut regex, &source, "{v1} text", &[formula, unused, unused]).unwrap());
        assert!(validate_response(&mut regex, &source, "{v1} text", &[formula, formula, unused]).unwrap());
        assert!(!validate_response(&mut regex, &source, "{v1} {v1}", &[formula, unused, unused]).unwrap());
    }

    #[test]
    fn validates_shared_style_closures_in_source_order() {
        let mut source = input("<style id='1'>one</style> {v1} <style id='2'>two</style>");
        for id in 1..=2 {
            source.placeholders.push(InputPlaceholder::Rich(RichInput {
                id, composition_index: id as usize, left: format!("<style id='{id}'>"), right: "</style>".into(),
                left_pattern: format!("<style id='{id}'>"), right_pattern: "</style>".into(), source_text: String::new(),
            }));
        }
        source.placeholders.push(InputPlaceholder::Formula(FormulaInput {
            id: 1, composition_index: 0, token: "{v1}".into(), pattern: r"\{v1\}".into(), source_chars: "x".into(), hint_source: false,
        }));
        let mut regex = TestRegex::default();
        let patterns = [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<style id='\d+'>", false).unwrap(), regex.compile(r"</style>", false).unwrap()];
        for (output, valid) in [
            ("<style id='1'>一</style> {v1} <style id='2'>二</style>", true),
            ("<style id='2'>二</style> {v1} <style id='1'>一</style>", false),
            ("<style id='1'>一 {v1}</style> <style id='2'>二</style>", false),
            ("<style id='1'>一<style id='2'>二</style></style> {v1}", false),
            ("<style id='1'>一</style> {v1} <style id='2'>二", false),
        ] {
            assert_eq!(validate_response(&mut regex, &source, output, &patterns).unwrap(), valid, "{output}");
        }
    }

    #[test]
    fn styled_continuation_restores_original_style_and_formulas() {
        let left = input("Earlier text. A continuous sentence");
        let mut right = input("<s1>continues here. Another sentence</s1>{v3}");
        right.placeholders.push(InputPlaceholder::Rich(RichInput {
            id: 1, composition_index: 0, left: "<s1>".into(), right: "</s1>".into(),
            left_pattern: "<s1>".into(), right_pattern: "</s1>".into(), source_text: "continues here. Another sentence".into(),
        }));
        right.placeholders.push(InputPlaceholder::Formula(FormulaInput {
            id: 3, composition_index: 1, token: "{v3}".into(), pattern: r"\{v3\}".into(), source_chars: "x".into(), hint_source: false,
        }));
        let moved = move_styled_prefix(&left, &right, &[0..4, 0..41, 36..41, 41..45]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        let result = redistribute_response(&moved.left, &format!("先前内容。一个连续句子 {STYLE_BOUNDARY}在此结束。 "), &moved.right,
            "<s1>另一个句子</s1>{v3}", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert_eq!(result.0, "先前内容。一个连续句子 ");
        assert_eq!(result.1, "<s1>在此结束。 另一个句子</s1>{v3}");
        let result = redistribute_response(&moved.left, &format!("Earlier text {STYLE_BOUNDARY}ends here."), &moved.right,
            "<s1>Another sentence</s1>{v3}", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert_eq!(result.1, "<s1>ends here. Another sentence</s1>{v3}");
        let result = redistribute_response(&moved.left, &format!("Earlier text {STYLE_BOUNDARY}ends here. "), &moved.right,
            "<s1>Another sentence</s1>{v3}", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert_eq!(result.1, "<s1>ends here. Another sentence</s1>{v3}");
        assert!(redistribute_response(&moved.left, "边界丢失", &moved.right, "<s1>另一个句子</s1>{v3}", &left, &right, &patterns, &mut regex).unwrap().is_none());
        assert!(redistribute_response(&moved.left, &format!("前文{STYLE_BOUNDARY}续文{STYLE_BOUNDARY}"), &moved.right, "<s1>另一个句子</s1>{v3}", &left, &right, &patterns, &mut regex).unwrap().is_none());
        assert!(redistribute_response(&moved.left, &format!("前文{STYLE_BOUNDARY}续文"), &moved.right, "<s1>另一个句子</s1>", &left, &right, &patterns, &mut regex).unwrap().is_none());
    }

    #[test]
    fn rejects_a_newline_inside_rich_text() {
        let mut source = input("<s1>styled text</s1> end");
        source.placeholders.push(InputPlaceholder::Rich(RichInput {
            id: 1, composition_index: 0, left: "<s1>".into(), right: "</s1>".into(),
            left_pattern: "<s1>".into(), right_pattern: "</s1>".into(), source_text: "styled text".into(),
        }));
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        assert!(validate_response(&mut regex, &source, "<s1>styled text</s1> end", &patterns).unwrap());
        assert!(!validate_response(&mut regex, &source, "<s1>styled\ntext</s1> end", &patterns).unwrap());
    }

    fn patterns(regex: &mut TestRegex) -> [u64; 3] {
        [regex.compile(r"\{v\d+\}", false).unwrap(), regex.compile(r"<s\d+>", false).unwrap(), regex.compile(r"</s\d+>", false).unwrap()]
    }

    #[test]
    fn redistributes_a_complete_translation_without_losing_text() {
        let left = input("An unfinished sentence across");
        let right = input("two different pages. A separate sentence.");
        let moved = move_sentence_prefix(&left, &right, &[]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        let translated = "一个连续句子跨越两张不同的页面。";
        let (first, second) = redistribute_response(&moved.left, translated, &moved.right, "另一句。", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert!(!first.is_empty() && !second.is_empty());
        assert_eq!(format!("{first}{second}"), format!("{translated}另一句。"));
    }

    #[test]
    fn redistributes_at_word_boundaries_and_keeps_formula_on_its_page() {
        let mut left = input("The equation {v1} continues across");
        left.placeholders.push(InputPlaceholder::Formula(FormulaInput {
            id: 1, composition_index: 0, token: "{v1}".into(), pattern: r"\{v1\}".into(),
            source_chars: "x".into(), hint_source: false,
        }));
        let right = input("two different pages. A separate sentence.");
        let moved = move_sentence_prefix(&left, &right, &[]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        let (first, second) = redistribute_response(&moved.left, "The equation {v1} continues across two different pages.", &moved.right, "A separate sentence.", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert!(first.contains("{v1}"));
        assert!(!second.contains("{v1}"));
        assert_eq!(format!("{first}{second}"), "The equation {v1} continues across two different pages. A separate sentence.");
    }

    #[test]
    fn preserves_whitespace_at_the_page_boundary() {
        let left = input("A sentence extends across");
        let right = input("two pages. Another sentence.");
        let moved = move_sentence_prefix(&left, &right, &[]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        let translated = "Several   words cross the page boundary.";
        let (first, second) = redistribute_response(&moved.left, translated, &moved.right, "Next.", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert_eq!(format!("{first}{second}"), format!("{translated} Next."));
    }

    #[test]
    fn does_not_move_a_previous_sentence_without_placeholders() {
        let left = input("The first sentence was complete. The second was complete. The final sentence starts");
        let right = input("on the following page. Another sentence follows.");
        let moved = move_sentence_prefix(&left, &right, &[]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        let translated = "第一句话完整。第二句话也完整。跨页的句子从这里开始并在下一页结束。";
        let (first, second) = redistribute_response(&moved.left, translated, &moved.right, "另一句话。", &left, &right, &patterns, &mut regex).unwrap().unwrap();
        assert!(first.contains("第一句话完整。第二句话也完整。"));
        assert!(!second.contains("第二句话也完整。"));
    }

    #[test]
    fn refuses_damaged_placeholders_and_unbreakable_words() {
        let left = input("A sentence starts");
        let right = input("in the following page. A separate sentence.");
        let moved = move_sentence_prefix(&left, &right, &[]).unwrap();
        let mut regex = TestRegex::default();
        let patterns = patterns(&mut regex);
        assert!(redistribute_response(&moved.left, "UnbreakableEnglishWord", &moved.right, "Next.", &left, &right, &patterns, &mut regex).unwrap().is_none());
        assert!(redistribute_response(&moved.left, "Text with {v2} extra.", &moved.right, "Next.", &left, &right, &patterns, &mut regex).unwrap().is_none());
    }
}
