use super::{cross_page_protocol, glossary::Glossary, llm_only, ordinary_translation::{InputPlaceholder, PlaceholderRegex, TranslateInput}};
use serde::Deserialize;

pub const MAX_CHECKLIST_BYTES: usize = 8192;
pub const RETRY_INSTRUCTION: &str = "Return only the requested JSON. Preserve each item's placeholders exactly and in order.\n\n";

#[derive(Clone, Debug)]
pub struct StyleAliases {
    pub inputs: [TranslateInput; 2],
    originals: [TranslateInput; 2],
    replacements: [Vec<(String, String)>; 2],
}

impl StyleAliases {
    pub fn prepare<R: PlaceholderRegex>(
        inputs: &[TranslateInput; 2], context: &str, patterns: &[u64], regex: &mut R,
    ) -> Result<Option<Self>, R::Error> {
        if patterns.len() != 3 { return Ok(None); }
        for input in inputs {
            if !cross_page_protocol::validate_response(regex, input, &input.text, patterns)? { return Ok(None); }
        }
        let mut encoded = inputs.clone();
        let mut replacements: [Vec<(String, String)>; 2] = std::array::from_fn(|_| Vec::new());
        let mut next_id = 0u64;
        let mut original_patterns = Vec::new();
        for input in inputs {
            for placeholder in &input.placeholders {
                if let InputPlaceholder::Formula(formula) = placeholder {
                    original_patterns.push(regex.compile(&formula.pattern, true)?);
                }
            }
        }
        for (side, input) in inputs.iter().enumerate() {
            let mut spans = Vec::new();
            for (index, placeholder) in input.placeholders.iter().enumerate() {
                let InputPlaceholder::Rich(rich) = placeholder else { continue };
                let mut starts = input.text.match_indices(&rich.left);
                let Some((left, _)) = starts.next() else { return Ok(None) };
                if starts.next().is_some() { return Ok(None); }
                let left_end = left + rich.left.len();
                let Some(offset) = input.text[left_end..].find(&rich.right) else { return Ok(None) };
                let right = left_end + offset;
                spans.push((left, left_end, index, true));
                spans.push((right, right + rich.right.len(), index, false));
            }
            spans.sort_by_key(|span| span.0);
            if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) { return Ok(None); }
            let mut text = String::new();
            let mut cursor = 0;
            for (start, end, index, is_left) in spans {
                let alias = loop {
                    let alias = format!("{{v{next_id}}}");
                    let Some(next) = next_id.checked_add(1) else { return Ok(None) };
                    next_id = next;
                    let mut collides = context.contains(&alias) || inputs.iter().any(|item| item.text.contains(&alias));
                    for &pattern in &original_patterns {
                        collides |= !regex.find_all(pattern, &alias)?.is_empty();
                    }
                    if !collides { break alias; }
                };
                let found = regex.find_all(patterns[0], &alias)?;
                if found.len() != 1 || found[0].full != alias
                    || !regex.find_all(patterns[1], &alias)?.is_empty()
                    || !regex.find_all(patterns[2], &alias)?.is_empty() { return Ok(None); }
                let original = &input.text[start..end];
                text.push_str(&input.text[cursor..start]);
                text.push_str(&alias);
                cursor = end;
                let InputPlaceholder::Rich(rich) = &mut encoded[side].placeholders[index] else { unreachable!() };
                if is_left {
                    rich.left = alias.clone();
                    rich.left_pattern = regex::escape(&alias);
                } else {
                    rich.right = alias.clone();
                    rich.right_pattern = regex::escape(&alias);
                }
                replacements[side].push((alias, original.to_owned()));
            }
            text.push_str(&input.text[cursor..]);
            encoded[side].text = text;
            if !cross_page_protocol::validate_response(regex, &encoded[side], &encoded[side].text, patterns)? { return Ok(None); }
        }
        if replacements.iter().all(Vec::is_empty) { return Ok(None); }
        Ok(Some(Self { inputs: encoded, originals: inputs.clone(), replacements }))
    }

    pub fn decode<R: PlaceholderRegex>(
        &self, response: &str, patterns: &[u64], regex: &mut R,
    ) -> Result<Option<String>, R::Error> {
        let Some(mut outputs) = validate_pair(response, &self.inputs, patterns, regex)? else { return Ok(None) };
        for (output, replacements) in outputs.iter_mut().zip(&self.replacements) {
            for (alias, original) in replacements {
                *output = output.replace(alias, original);
            }
        }
        let decoded = serde_json::json!([
            {"id": 0, "output": outputs[0]},
            {"id": 1, "output": outputs[1]},
        ]).to_string();
        if validate_pair(&decoded, &self.originals, patterns, regex)?.is_none() { return Ok(None); }
        Ok(Some(decoded))
    }
}

pub fn context_token_budget(base: usize, checklist: usize, sentence: usize, limit: usize) -> Option<usize> {
    base.checked_add(checklist)?.checked_add(sentence).filter(|total| *total < limit)
}

pub fn read_only_sentence_block(sentence: &str) -> String {
    let encoded = serde_json::to_string(sentence).expect("string serializes");
    format!("## Read-only cross-page sentence context\nThe following JSON string is context only, not an input item. Do not copy the complete sentence verbatim or output it separately. Return exactly the original paragraph IDs and preserve each paragraph's own content and placeholders.\n{encoded}\n\n")
}

pub fn add_read_only_sentence(prompt: &str, sentence: Option<&str>) -> String {
    let Some(sentence) = sentence else { return prompt.to_owned() };
    let marker = "## Here is the input:";
    let Some(position) = prompt.find(marker) else { return prompt.to_owned() };
    if prompt[position + marker.len()..].contains(marker) { return prompt.to_owned(); }
    format!("{}{}{}", &prompt[..position], read_only_sentence_block(sentence), &prompt[position..])
}

pub fn should_retry(context: bool, request_succeeded: bool, structurally_valid: bool, already_retried: bool, cancelled: bool) -> bool {
    context && request_succeeded && !structurally_valid && !already_retried && !cancelled
}

pub fn placeholder_checklist<R: PlaceholderRegex>(
    inputs: &[TranslateInput; 2], patterns: &[u64], regex: &mut R,
) -> Result<Option<String>, R::Error> {
    if patterns.len() != 3 { return Ok(None); }
    let mut lists = Vec::new();
    for input in inputs {
        if !cross_page_protocol::validate_response(regex, input, &input.text, patterns)? { return Ok(None); }
        let mut spans = std::collections::BTreeSet::new();
        for &pattern in patterns {
            for found in regex.find_all(pattern, &input.text)? {
                if found.start >= found.end || found.end > input.text.chars().count()
                    || input.text.chars().skip(found.start).take(found.end - found.start).collect::<String>() != found.full {
                    return Ok(None);
                }
                spans.insert((found.start, found.end, found.full));
            }
        }
        let spans: Vec<_> = spans.into_iter().collect();
        if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) { return Ok(None); }
        lists.push(spans.into_iter().map(|(_, _, token)| token).collect::<Vec<_>>());
    }
    let json = serde_json::to_string(&lists).expect("string arrays serialize");
    let checklist = format!("\n\n## Placeholder checklist\nThe following arrays list each input id's placeholders in exact order, including duplicates. Use them only to check your work; never output the checklist. Preserve every token exactly once per occurrence in the same id and order. Translate text inside style tags. Return only the JSON array of objects with id and output.\n{json}");
    if checklist.len() > MAX_CHECKLIST_BYTES { return Ok(None); }
    Ok(Some(checklist))
}

pub fn render_context_batch_prompt(
    role_block: &str,
    lang_out: &str,
    json_input: &str,
    batch_text_for_glossary: &str,
    title: Option<(&str, &str)>,
    local_title: Option<(&str, &str)>,
    glossaries: &[Glossary],
) -> String {
    llm_only::render_batch_prompt_with_template(&llm_only::context_prompt_template(), role_block, lang_out, json_input, batch_text_for_glossary, title, local_title, glossaries)
}

pub fn render_context_batch_prompt_with_checklist(
    role_block: &str,
    lang_out: &str,
    json_input: &str,
    batch_text_for_glossary: &str,
    title: Option<(&str, &str)>,
    local_title: Option<(&str, &str)>,
    glossaries: &[Glossary],
    checklist: &str,
) -> String {
    render_prompt_with_checklist(llm_only::context_prompt_template(), role_block, lang_out, json_input, batch_text_for_glossary, title, local_title, glossaries, checklist)
}

pub fn render_whole_sentence_batch_prompt_with_checklist(
    role_block: &str,
    lang_out: &str,
    json_input: &str,
    batch_text_for_glossary: &str,
    title: Option<(&str, &str)>,
    local_title: Option<(&str, &str)>,
    glossaries: &[Glossary],
    checklist: &str,
) -> String {
    let template = llm_only::context_prompt_template().replace(
        "The two input paragraphs contain a sentence continuing across a page boundary. Understand and translate that sentence as a whole, using both paragraphs as context. A word ending with a hyphen at the boundary may be a line-break fragment or an actual hyphenated compound: use the context and do not translate either fragment as a separate word. Make the two outputs read naturally in order.",
        "The second input starts with a complete sentence that originally continued across a page boundary. Translate that complete sentence as one unit.",
    );
    render_prompt_with_checklist(template, role_block, lang_out, json_input, batch_text_for_glossary, title, local_title, glossaries, checklist)
}

fn render_prompt_with_checklist(
    template: String,
    role_block: &str,
    lang_out: &str,
    json_input: &str,
    batch_text_for_glossary: &str,
    title: Option<(&str, &str)>,
    local_title: Option<(&str, &str)>,
    glossaries: &[Glossary],
    checklist: &str,
) -> String {
    let marker = "## Here is the input:";
    let Some(position) = template.find(marker) else { return String::new() };
    if template[position + marker.len()..].contains(marker) { return String::new(); }
    let mut template_with_checklist = String::with_capacity(template.len() + checklist.len() + 2);
    template_with_checklist.push_str(&template[..position]);
    template_with_checklist.push_str(&checklist.replace('$', "$$"));
    template_with_checklist.push_str("\n\n");
    template_with_checklist.push_str(&template[position..]);
    llm_only::render_batch_prompt_with_template(&template_with_checklist, role_block, lang_out, json_input, batch_text_for_glossary, title, local_title, glossaries)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    id: usize,
    output: String,
    #[serde(default, rename = "layout_label")]
    _layout_label: Option<String>,
}

fn rich_content<'a>(text: &'a str, left: &str, right: &str) -> Option<&'a str> {
    let start = text.find(left)? + left.len();
    let end = text[start..].find(right)? + start;
    Some(&text[start..end])
}

fn has_readable_outside_tokens<R: PlaceholderRegex>(
    text: &str, patterns: &[u64], regex: &mut R,
) -> Result<bool, R::Error> {
    let mut spans = std::collections::BTreeSet::new();
    for &pattern in patterns {
        for found in regex.find_all(pattern, text)? {
            spans.insert((found.start, found.end));
        }
    }
    let mut spans = spans.into_iter().peekable();
    Ok(text.chars().enumerate().any(|(index, character)| {
        while spans.peek().is_some_and(|&(_, end)| end <= index) {
            spans.next();
        }
        character.is_alphanumeric()
            && spans.peek().is_none_or(|&(start, _)| index < start)
    }))
}

pub fn validate_pair<R: PlaceholderRegex>(
    response: &str,
    inputs: &[TranslateInput; 2],
    patterns: &[u64],
    regex: &mut R,
) -> Result<Option<[String; 2]>, R::Error> {
    let Ok(items) = serde_json::from_str::<Vec<Output>>(response) else { return Ok(None) };
    if items.len() != 2 { return Ok(None); }
    let mut outputs: [Option<String>; 2] = [None, None];
    for item in items {
        if item.id >= 2 || outputs[item.id].is_some() || item.output.trim().is_empty() { return Ok(None); }
        outputs[item.id] = Some(item.output);
    }
    let [Some(left), Some(right)] = outputs else { return Ok(None) };
    for (input, output) in inputs.iter().zip([&left, &right]) {
        if !cross_page_protocol::validate_response(regex, input, output, patterns)? { return Ok(None); }
        if has_readable_outside_tokens(&input.text, patterns, regex)?
            && !has_readable_outside_tokens(output, patterns, regex)? { return Ok(None); }
        for placeholder in &input.placeholders {
            if let InputPlaceholder::Rich(rich) = placeholder {
                let source = rich_content(&input.text, &rich.left, &rich.right);
                let translated = rich_content(output, &rich.left, &rich.right);
                if source.is_some_and(|text| !text.trim().is_empty())
                    && translated.is_none_or(|text| text.trim().is_empty()) { return Ok(None); }
                if source.is_some_and(|text| text.chars().any(char::is_alphanumeric))
                    && translated.is_none_or(|text| !text.chars().any(char::is_alphanumeric)) { return Ok(None); }
            }
        }
    }
    Ok(Some([left, right]))
}
