use super::{MARKER, Source};
use std::ops::Range;

fn abbreviation(prefix: &str) -> bool {
    let word = prefix
        .rsplit(|character: char| character.is_whitespace() || "([{\"“".contains(character))
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "e.g."
            | "i.e."
            | "al."
            | "fig."
            | "figs."
            | "eq."
            | "eqs."
            | "dr."
            | "mr."
            | "mrs."
            | "ms."
            | "prof."
            | "vs."
            | "no."
            | "nos."
            | "approx."
            | "ref."
            | "refs."
            | "vol."
            | "pp."
            | "st."
    ) || (word.len() == 2 && word.as_bytes()[0].is_ascii_alphabetic())
        || (word.matches('.').count() > 1
            && word
                .trim_end_matches('.')
                .split('.')
                .all(|part| part.len() == 1))
}

fn trailing_reference(text: &str, start: usize) -> usize {
    let mut cursor = start;
    loop {
        let previous = cursor;
        while let Some(character) = text[cursor..].chars().next() {
            if !"\"”’')]}」』".contains(character) {
                break;
            }
            cursor += character.len_utf8();
        }
        let whitespace = text[cursor..].len() - text[cursor..].trim_start().len();
        let next = cursor + whitespace;
        if let Some(marker) = MARKER
            .find(&text[next..])
            .filter(|marker| marker.start() == 0 && whitespace == 0)
        {
            cursor = next + marker.end();
        } else if text[next..].starts_with('[') {
            if let Some(end) = text[next..].find(']') {
                let citation = &text[next + 1..next + end];
                if !citation.is_empty()
                    && citation
                        .chars()
                        .all(|character| character.is_numeric() || " ,;–-".contains(character))
                {
                    cursor = next + end + 1;
                }
            }
        }
        if cursor == previous {
            return cursor;
        }
    }
}

pub fn sentence_ends(text: &str) -> Vec<usize> {
    let markers = MARKER
        .find_iter(text)
        .map(|marker| marker.range())
        .collect::<Vec<_>>();
    let mut ends = Vec::new();
    let mut parentheses = 0usize;
    for (position, character) in text.char_indices() {
        if markers.iter().any(|range| range.contains(&position)) {
            continue;
        }
        if "([（".contains(character) {
            parentheses += 1;
        } else if ")]）".contains(character) {
            parentheses = parentheses.saturating_sub(1);
        }
        if !".!?。！？".contains(character) || parentheses != 0 {
            continue;
        }
        let after = position + character.len_utf8();
        if character == '.' {
            let previous = text[..position].chars().next_back();
            let following = text[after..].chars().next();
            if previous.is_some_and(|value| value.is_numeric())
                && following.is_some_and(|value| value.is_numeric())
                || abbreviation(&text[..after])
                || following.is_some_and(|value| value.is_alphanumeric())
            {
                continue;
            }
        }
        let end = trailing_reference(text, after);
        if text[end..]
            .chars()
            .next()
            .is_none_or(|value| value.is_whitespace())
            || matches!(character, '。' | '！' | '？')
        {
            if ends.last() != Some(&end) {
                ends.push(end);
            }
        }
    }
    ends
}

pub fn enclosing_start(source: &Source, start: usize) -> usize {
    let previous = sentence_ends(&source.text)
        .into_iter()
        .filter(|end| *end <= start)
        .last()
        .unwrap_or(0);
    previous + source.text[previous..].len() - source.text[previous..].trim_start().len()
}

pub fn enclosing_end(source: &Source, end: usize) -> usize {
    sentence_ends(&source.text)
        .into_iter()
        .find(|position| *position >= end)
        .unwrap_or_else(|| source.text.trim_end().len())
}

pub fn has_sentence_end(source: &Source, range: &Range<usize>) -> bool {
    sentence_ends(&source.text)
        .into_iter()
        .any(|end| range.start < end && end <= range.end)
}

pub fn ends_sentence(source: &Source) -> bool {
    sentence_ends(&source.text).last().copied() == Some(source.text.trim_end().len())
}
