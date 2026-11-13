use std::collections::HashMap;
use std::sync::OnceLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::shared::layout_helper::unicode::is_python_strip_whitespace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlossaryEntry {
    pub source: String,
    pub target: String,
    pub target_language: Option<String>,
}

impl GlossaryEntry {
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            target_language: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Glossary {
    pub name: String,
    pub entries: Vec<GlossaryEntry>,
    matcher: Option<AhoCorasick>,
    automatic: bool,
    contexts: HashMap<String, Vec<TermContext>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TermContext {
    pub text: String,
    pub layout_label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedTerm {
    pub source: String,
    pub target: String,
    pub contexts: Vec<TermContext>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlossarySuggestion {
    pub source: String,
    pub target: String,
    pub context: TermContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlossaryError {
    EmptyPattern,
    Matcher(String),
}

impl std::fmt::Display for GlossaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPattern => f.write_str("empty glossary source is not supported"),
            Self::Matcher(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for GlossaryError {}

impl Glossary {
    pub fn new(
        name: impl Into<String>,
        entries: Vec<GlossaryEntry>,
    ) -> Result<Self, GlossaryError> {
        Self::new_with_source_spelling_votes(name, entries, false)
    }

    fn new_with_source_spelling_votes(
        name: impl Into<String>,
        entries: Vec<GlossaryEntry>,
        prefer_frequent_spelling: bool,
    ) -> Result<Self, GlossaryError> {
        let mut groups: Vec<Vec<(GlossaryEntry, usize)>> = Vec::new();
        let mut group_indices: HashMap<String, usize> = HashMap::new();
        for entry in entries {
            let normalized = normalize_source(&entry.source);
            if let Some(index) = group_indices.get(&normalized).copied() {
                if !prefer_frequent_spelling {
                    continue;
                }
                let group = &mut groups[index];
                if let Some((_, count)) = group
                    .iter_mut()
                    .find(|(candidate, _)| candidate.source == entry.source)
                {
                    *count += 1;
                } else {
                    group.push((entry, 1));
                }
            } else {
                group_indices.insert(normalized, groups.len());
                groups.push(vec![(entry, 1)]);
            }
        }
        let mut unique = Vec::with_capacity(groups.len());
        for group in groups {
            if !prefer_frequent_spelling {
                unique.push(
                    group
                        .into_iter()
                        .next()
                        .expect("each normalized source group has an entry")
                        .0,
                );
                continue;
            }
            let mut selected = group[0].0.clone();
            let mut highest_count = group[0].1;
            for (entry, count) in group.into_iter().skip(1) {
                if count > highest_count
                    || (count == highest_count
                        && entry.source.chars().cmp(selected.source.chars()).is_lt())
                {
                    selected = entry;
                    highest_count = count;
                }
            }
            unique.push(selected);
        }
        let matcher = if unique.is_empty() {
            None
        } else {
            if unique.iter().any(|entry| entry.source.is_empty()) {
                return Err(GlossaryError::EmptyPattern);
            }
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::Standard)
                    .ascii_case_insensitive(true)
                    .build(unique.iter().map(|entry| entry.source.as_bytes()))
                    .map_err(|error| GlossaryError::Matcher(error.to_string()))?,
            )
        };
        Ok(Self {
            name: name.into(),
            entries: unique,
            matcher,
            automatic: false,
            contexts: HashMap::new(),
        })
    }

    pub fn active_entries_for_text(&self, text: &str) -> Vec<(String, String)> {
        if self.automatic {
            return Vec::new();
        }
        let Some(matcher) = &self.matcher else {
            return Vec::new();
        };
        let normalized = normalize_match_text(text);
        let mut seen = std::collections::HashSet::new();
        matcher
            .find_overlapping_iter(normalized.as_bytes())
            .filter_map(|mat| {
                let index = mat.pattern().as_usize();
                seen.insert(index).then(|| {
                    (
                        self.entries[index].source.clone(),
                        self.entries[index].target.clone(),
                    )
                })
            })
            .collect()
    }

    pub fn active_suggestions_for_text(&self, text: &str) -> Vec<GlossarySuggestion> {
        if !self.automatic {
            return Vec::new();
        }
        let normalized_text = normalize_match_text(text);
        let mut suggestions = Vec::new();
        for entry in &self.entries {
            if let Some(contexts) = self.contexts.get(&normalize_source(&entry.source)) {
                for context in contexts {
                    if normalized_text == normalize_match_text(&context.text) {
                        let suggestion = GlossarySuggestion {
                            source: entry.source.clone(),
                            target: entry.target.clone(),
                            context: context.clone(),
                        };
                        if !suggestions.contains(&suggestion) {
                            suggestions.push(suggestion);
                        }
                    }
                }
            }
        }
        suggestions
    }

    pub fn to_csv(&self) -> String {
        let mut out = String::from("source,target,tgt_lng\r\n");
        for entry in &self.entries {
            out.push_str(&csv_field(&entry.source));
            out.push(',');
            out.push_str(&csv_field(&entry.target));
            out.push(',');
            out.push_str(&csv_field(entry.target_language.as_deref().unwrap_or("")));
            out.push_str("\r\n");
        }
        out
    }
}

pub fn normalize_source(source: &str) -> String {
    collapse_python_whitespace(source.to_lowercase().as_str())
}

fn normalize_match_text(text: &str) -> String {
    collapse_python_whitespace(text)
}

fn collapse_python_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if is_python_strip_whitespace(ch) {
            if !out.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        let mut out = String::with_capacity(field.len() + 2);
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        field.to_owned()
    }
}

pub fn finalize_terms(
    name: impl Into<String>,
    raw_terms: &[(String, String)],
) -> Result<Option<Glossary>, GlossaryError> {
    if raw_terms.is_empty() {
        return Ok(None);
    }
    let mut source_indices: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<(&str, &str, bool)> = Vec::new();
    for (source, target) in raw_terms {
        let normalized = normalize_source(source);
        if normalized.is_empty() || target.trim().is_empty() {
            continue;
        }
        if let Some(index) = source_indices.get(&normalized).copied() {
            if groups[index].1 != target {
                groups[index].2 = true;
            }
        } else {
            source_indices.insert(normalized, groups.len());
            groups.push((source, target, false));
        }
    }
    let entries = groups
        .into_iter()
        .filter(|(_, _, conflicted)| !conflicted)
        .map(|(source, target, _)| GlossaryEntry::new(source, target))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(None);
    }
    let mut glossary = Glossary::new(name, entries)?;
    glossary.automatic = true;
    Ok(Some(glossary))
}

pub fn finalize_scoped_terms(
    name: impl Into<String>,
    raw_terms: &[(String, String)],
    scoped_terms: &[ScopedTerm],
) -> Result<Option<Glossary>, GlossaryError> {
    let Some(mut glossary) = finalize_terms(name, raw_terms)? else {
        return Ok(None);
    };
    for term in scoped_terms {
        let key = normalize_source(&term.source);
        if glossary.entries.iter().any(|entry| {
            normalize_source(&entry.source) == key && entry.target == term.target
        }) {
            glossary.contexts.entry(key).or_default().extend(term.contexts.clone());
        }
    }
    Ok(Some(glossary))
}

pub fn term_context(text: &str, source: &str, layout_label: Option<String>) -> Option<TermContext> {
    if term_occurrences(text, source) != 1 {
        return None;
    }
    let context = text.trim();
    if context.chars().count() < source.trim().chars().count() + 16
        || context.chars().count() < 32
    {
        return None;
    }
    Some(TermContext { text: context.to_owned(), layout_label })
}

pub(super) fn term_occurrences(text: &str, source: &str) -> usize {
    let text = normalize_source(text);
    let source = normalize_source(source);
    if source.is_empty() {
        return 0;
    }
    let matcher = AhoCorasickBuilder::new()
        .build([source.as_bytes()])
        .expect("a nonempty source is a valid pattern");
    let mut matches = 0;
    for found in matcher.find_overlapping_iter(text.as_bytes()) {
        let before = text[..found.start()].chars().next_back();
        let after = text[found.end()..].chars().next();
        let starts_word = source.chars().next().is_some_and(is_word_character);
        let ends_word = source.chars().next_back().is_some_and(is_word_character);
        if (starts_word && before.is_some_and(is_word_character))
            || (ends_word && after.is_some_and(is_word_character))
        {
            continue;
        }
        matches += 1;
        if matches > 1 {
            break;
        }
    }
    matches
}

fn is_word_character(character: char) -> bool {
    static WORD_CHARACTER: OnceLock<regex::Regex> = OnceLock::new();
    let matcher = WORD_CHARACTER.get_or_init(|| regex::Regex::new(r"^\w$").unwrap());
    let mut buffer = [0; 4];
    matcher.is_match(character.encode_utf8(&mut buffer))
}

#[cfg(test)]
mod glossary_tests;
