use std::ops::Range;

use super::cross_page_protocol::validate_response;
use super::ordinary_translation::{InputPlaceholder, PlaceholderRegex, TranslateInput};

pub fn natural_split_candidates(sentence: &str) -> Vec<(&str, &str)> {
    if sentence.chars().any(|character| character.is_control()
        || matches!(character, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{2200}'..='\u{22ff}' | '\u{27c0}'..='\u{27ef}' | '\u{2980}'..='\u{2aff}')
        || matches!(character, '"' | '\'' | '“' | '”' | '‘' | '’' | '「' | '」' | '『' | '』' | '(' | ')' | '（' | '）' | '[' | ']' | '【' | '】' | '<' | '>' | '{' | '}' | '$' | '\\' | '`' | '*' | '_' | '|' | '%' | '^' | '=' | '+' | '/' | '÷' | '×' | '±' | '−' | '~' | '#' | '&' | '@')) {
        return Vec::new();
    }
    let punctuation = |character: char| matches!(character, ',' | ';' | ':' | '，' | '；' | '：' | '、' | '.' | '。' | '!' | '?' | '！' | '？' | '…');
    let mut candidates = Vec::new();
    for (index, character) in sentence.char_indices() {
        if !matches!(character, ',' | ';' | '，' | '；' | '、') { continue; }
        let end = index + character.len_utf8();
        let left = &sentence[..end];
        let right = &sentence[end..];
        let previous = sentence[..index].chars().next_back();
        let next = right.chars().next();
        let next_content = right.trim_start().chars().next();
        if previous.is_none_or(|value| value.is_whitespace() || punctuation(value))
            || next.is_none_or(|value| punctuation(value))
            || (character.is_ascii() && next.is_none_or(|value| !value.is_whitespace()))
            || (previous.is_some_and(|value| value.is_numeric()) && next_content.is_some_and(|value| value.is_numeric()))
            || !left.chars().any(char::is_alphanumeric)
            || !right.chars().any(char::is_alphanumeric)
            || next_content.is_none_or(punctuation) {
            continue;
        }
        candidates.push((left, right));
    }
    candidates
}

#[derive(Clone, Debug)]
pub struct SentencePlan {
    pub left_prefix: TranslateInput,
    pub sentence: String,
    pub right_suffix: TranslateInput,
    original_left: TranslateInput,
    original_right: TranslateInput,
    right_style_open: String,
}

fn safe_text(value: &str) -> bool {
    value.chars().any(char::is_alphanumeric)
        && !value.chars().any(|character| matches!(character, '<' | '>' | '{' | '}' | '[' | ']' | '\n' | '\r' | '`' | '*' | '_' | '\\' | '|'))
}

fn sentence_end(value: &str) -> Option<usize> {
    let (index, punctuation) = value.char_indices().find(|(_, character)| matches!(character, '.' | '!' | '?' | '。' | '！' | '？'))?;
    let before = &value[..index];
    if !safe_text(before) { return None; }
    if punctuation == '.' {
        let word = before.rsplit(|character: char| !character.is_alphabetic()).next()?;
        if word.chars().count() <= 3 || before.chars().last()?.is_ascii_digit()
            || matches!(word.to_ascii_lowercase().as_str(), "prof" | "drs" | "assoc" | "dept" | "approx" | "figs" | "et" | "al" | "etc" | "inc" | "ltd" | "corp" | "vs") { return None; }
    }
    let end = index + punctuation.len_utf8();
    if !value[end..].chars().next()?.is_whitespace() { return None; }
    let next = value[end..].char_indices().find(|(_, character)| !character.is_whitespace())?.0 + end;
    if matches!(value[next..].chars().next()?, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '）' | '】') { return None; }
    Some(next)
}

fn ranges_valid(text: &str, ranges: &[Range<usize>]) -> bool {
    ranges.iter().all(|range| range.start < range.end && range.end <= text.len()
        && text.is_char_boundary(range.start) && text.is_char_boundary(range.end))
}

pub fn prepare<R: PlaceholderRegex>(
    left: &TranslateInput, right: &TranslateInput,
    left_protected: &[Range<usize>], right_protected: &[Range<usize>],
    style_equivalent: bool, layout_safe: bool, patterns: &[u64], regex: &mut R,
) -> Result<Option<SentencePlan>, R::Error> {
    if !style_equivalent || !layout_safe || !ranges_valid(&left.text, left_protected)
        || !ranges_valid(&right.text, right_protected)
        || !validate_response(regex, left, &left.text, patterns)?
        || !validate_response(regex, right, &right.text, patterns)? { return Ok(None); }
    let Some(InputPlaceholder::Rich(right_rich)) = right.placeholders.first() else { return Ok(None) };
    let (left_content_start, left_content, left_close, left_span, allow_initial_period) = match left.placeholders.last() {
        Some(InputPlaceholder::Rich(rich)) if left.text.ends_with(&rich.right) => {
            let Some(open) = left.text.rfind(&rich.left) else { return Ok(None) };
            let start = open + rich.left.len();
            let Some(content) = left.text[start..].strip_suffix(&rich.right) else { return Ok(None) };
            (start, content, rich.right.as_str(), Some(open..left.text.len()), false)
        }
        _ => {
            let mut end = 0;
            for pattern in patterns {
                for found in regex.find_all(*pattern, &left.text)? {
                    let Some(offset) = char_offset(&left.text, found.end) else { return Ok(None) };
                    end = end.max(offset);
                }
            }
            let formula_ends = left.placeholders.last().is_some_and(|placeholder| matches!(placeholder, InputPlaceholder::Formula(formula) if left.text[..end].ends_with(&formula.token)));
            (end, &left.text[end..], "", None, formula_ends)
        }
    };
    let mut left_split = 0;
    if allow_initial_period && left_content.starts_with(". ") {
        let Some(offset) = left_content[1..].char_indices().find(|(_, character)| !character.is_whitespace()).map(|(index, _)| index + 1) else { return Ok(None) };
        left_split = offset;
    }
    while left_content[left_split..].chars().any(|character| matches!(character, '.' | '!' | '?' | '。' | '！' | '？')) {
        let Some(end) = sentence_end(&left_content[left_split..]) else { return Ok(None) };
        left_split += end;
    }
    if left_split == 0 { return Ok(None); }
    let left_sentence = &left_content[left_split..];
    if !safe_text(left_sentence) || left_sentence.chars().any(|character| matches!(character, '.' | '!' | '?' | '。' | '！' | '？'))
        || left_sentence.chars().last().is_none_or(|character| !character.is_alphanumeric()) { return Ok(None); }
    let Some(right_content) = right.text.strip_prefix(&right_rich.left) else { return Ok(None) };
    let Some(close) = right_content.find(&right_rich.right) else { return Ok(None) };
    let Some(right_split) = sentence_end(&right_content[..close]) else { return Ok(None) };
    let right_sentence = &right_content[..right_split];
    if !safe_text(right_sentence) || !safe_text(&right_content[right_split..close])
        || right_sentence.chars().next().is_none_or(|character| !character.is_lowercase()) { return Ok(None); }
    let left_range = (left_content_start + left_split)..(left_content_start + left_content.len());
    let right_range = right_rich.left.len()..(right_rich.left.len() + right_split);
    let right_span = 0..(right_rich.left.len() + close + right_rich.right.len());
    if left_protected.iter().any(|range| range.start < left_range.end && range.end > left_range.start && left_span.as_ref() != Some(range))
        || right_protected.iter().any(|range| range.start < right_range.end && range.end > right_range.start && *range != right_span) { return Ok(None); }
    let mut left_prefix = left.clone();
    left_prefix.text = format!("{}{}{}", &left.text[..left_content_start], &left_content[..left_split], left_close);
    let mut right_suffix = right.clone();
    right_suffix.text = format!("{}{}", right_rich.left, &right_content[right_split..]);
    if !validate_response(regex, &left_prefix, &left_prefix.text, patterns)?
        || !validate_response(regex, &right_suffix, &right_suffix.text, patterns)? { return Ok(None); }
    Ok(Some(SentencePlan {
        left_prefix, sentence: format!("{} {}", left_sentence.trim(), right_sentence.trim()), right_suffix,
        original_left: left.clone(), original_right: right.clone(), right_style_open: right_rich.left.clone(),
    }))
}

fn char_offset(text: &str, offset: usize) -> Option<usize> {
    if offset == text.chars().count() { Some(text.len()) } else { text.char_indices().nth(offset).map(|(index, _)| index) }
}

pub fn assemble<R: PlaceholderRegex>(
    plan: &SentencePlan, left_output: &str, sentence_output: &str, right_output: &str,
    patterns: &[u64], regex: &mut R,
) -> Result<Option<(String, String)>, R::Error> {
    if !safe_text(sentence_output) || !validate_response(regex, &plan.left_prefix, left_output, patterns)?
        || !validate_response(regex, &plan.right_suffix, right_output, patterns)? { return Ok(None); }
    for pattern in patterns {
        if !regex.find_all(*pattern, sentence_output)?.is_empty() { return Ok(None); }
    }
    let Some(rest) = right_output.strip_prefix(&plan.right_style_open) else { return Ok(None) };
    let separator = if sentence_output.chars().last().is_some_and(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '!' | '?' | ':' | ';' | ','))
        && rest.chars().next().is_some_and(|character| character.is_ascii_alphanumeric()) { " " } else { "" };
    let right = format!("{}{sentence_output}{separator}{rest}", plan.right_style_open);
    if !validate_response(regex, &plan.original_left, left_output, patterns)?
        || !validate_response(regex, &plan.original_right, &right, patterns)? { return Ok(None); }
    Ok(Some((left_output.to_owned(), right)))
}
