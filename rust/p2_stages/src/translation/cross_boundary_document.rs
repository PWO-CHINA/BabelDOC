use super::*;
use std::collections::{BTreeMap, HashMap};
use super::super::cross_page_protocol::NUMERIC_EXPRESSION as EXPRESSIONS;

static MATH_EXPRESSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    let atom = r"(?:\[\[BABEL_[^\]]*_F\]\]|\p{N}+(?:[.,]\p{N}+)*(?:[eE][+\-−]?\p{N}+)?|(?-u:\b)[A-Za-z](?-u:\b))";
    regex::Regex::new(&format!(r"{atom}(?:\s*(?:\+/-|[+\-−–*/⁄^=<>≤≥×÷±∓])\s*[+\-−–]?\s*{atom})+"))
        .expect("mathematical expression pattern")
});

pub(super) fn expression_range_is_complete(text: &str, range: &Range<usize>) -> bool {
    !EXPRESSIONS.find_iter(text).any(|found| {
        (found.start() < range.start && range.start < found.end())
            || (found.start() < range.end && range.end < found.end())
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    pub location: Location,
    pub composition: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputPart {
    Text {
        text: String,
        style: Option<Resource>,
    },
    Formula(Resource),
}

#[derive(Clone, Debug)]
pub struct ResolvedParagraph {
    pub location: Location,
    pub parts: Vec<OutputPart>,
}

#[derive(Clone, Debug)]
pub struct DocumentSelection {
    pub parts: Vec<SelectedPart>,
    pub joins: Vec<Join>,
}

#[derive(Clone)]
enum TokenKind {
    Formula(Resource),
    Open(Resource),
    Close(Resource),
}

#[derive(Clone)]
struct Token {
    text: String,
    kind: TokenKind,
    explanation: String,
}

struct Item {
    ranges: Vec<SelectedPart>,
    tokens: Vec<Token>,
    weights: Vec<usize>,
    prefix: String,
    suffix: String,
}

pub struct DocumentPlan {
    pub items: Vec<String>,
    metadata: Vec<Item>,
    sources: Vec<Source>,
    capacities: HashMap<Location, usize>,
}

fn remove_line_hyphen(text: &mut String) -> Result<(), String> {
    let mut end = text.len();
    while let Some(marker) = MARKER.find_iter(&text[..end]).last() {
        if marker.end() != end || !marker.as_str().ends_with("_R]]") {
            break;
        }
        end = marker.start();
    }
    let Some((position, ch)) = text[..end].char_indices().last() else {
        return Err("empty dehyphenation source".into());
    };
    if !matches!(ch, '-' | '‐' | '\u{ad}') {
        return Err("dehyphenation source has no line-break hyphen".into());
    }
    text.replace_range(position..end, "");
    Ok(())
}

fn same_join(left: Join, right: Join) -> bool {
    std::mem::discriminant(&left) == std::mem::discriminant(&right)
}

fn overlaps(left: &DocumentSelection, right: &DocumentSelection) -> bool {
    left.parts.iter().any(|part| {
        right.parts.iter().any(|other| {
            part.source.location == other.source.location
                && part.range.start < other.range.end
                && other.range.start < part.range.end
        })
    })
}

fn merge(left: DocumentSelection, right: DocumentSelection) -> Result<DocumentSelection, String> {
    let mut parts: HashMap<Location, SelectedPart> = HashMap::new();
    let mut next = HashMap::new();
    let mut previous = HashMap::new();
    for selection in [&left, &right] {
        for part in &selection.parts {
            if let Some(existing) = parts.get_mut(&part.source.location) {
                if existing.source.text != part.source.text {
                    return Err("source changed between selections".into());
                }
                existing.range.start = existing.range.start.min(part.range.start);
                existing.range.end = existing.range.end.max(part.range.end);
            } else {
                parts.insert(part.source.location, part.clone());
            }
        }
        for (pair, join) in selection.parts.windows(2).zip(&selection.joins) {
            let first = pair[0].source.location;
            let second = pair[1].source.location;
            if next
                .get(&first)
                .is_some_and(|&(target, existing)| target != second || !same_join(existing, *join))
                || previous.get(&second).is_some_and(|&target| target != first)
            {
                return Err("conflicting sentence reading order".into());
            }
            next.insert(first, (second, *join));
            previous.insert(second, first);
        }
    }
    let starts = parts
        .keys()
        .filter(|location| !previous.contains_key(location))
        .copied()
        .collect::<Vec<_>>();
    let &[mut current] = starts.as_slice() else {
        return Err("sentence selections do not form one path".into());
    };
    let mut ordered = Vec::new();
    let mut joins = Vec::new();
    loop {
        let Some(part) = parts.remove(&current) else {
            return Err("cyclic sentence selection".into());
        };
        ordered.push(part);
        if let Some(&(target, join)) = next.get(&current) {
            joins.push(join);
            current = target;
        } else {
            break;
        }
    }
    if !parts.is_empty() {
        return Err("disconnected sentence selection".into());
    }
    Ok(DocumentSelection {
        parts: ordered,
        joins,
    })
}

fn encode(part: &SelectedPart, item: usize, tokens: &mut Vec<Token>) -> String {
    let mut output = String::new();
    for segment in &part.source.segments {
        let start = segment.range.start.max(part.range.start);
        let end = segment.range.end.min(part.range.end);
        if start >= end {
            continue;
        }
        let resource = |index| Resource {
            location: part.source.location,
            composition: index,
        };
        let serial = tokens.len();
        match segment.kind {
            SourceKind::Plain => output.push_str(&part.source.text[start..end]),
            SourceKind::Formula(index) => {
                let InputPlaceholder::Formula(formula) = &part.source.input.placeholders[index]
                else {
                    unreachable!()
                };
                let text = format!("[[BABEL_{item}_{serial}_F]]");
                output.push_str(&text);
                tokens.push(Token {
                    text,
                    kind: TokenKind::Formula(resource(formula.composition_index)),
                    explanation: formula.source_chars.clone(),
                });
            }
            SourceKind::Rich(index) => {
                let InputPlaceholder::Rich(rich) = &part.source.input.placeholders[index] else {
                    unreachable!()
                };
                let open = format!("[[BABEL_{item}_{serial}_L]]");
                let close = format!("[[BABEL_{item}_{serial}_R]]");
                output.push_str(&open);
                output.push_str(&part.source.text[start..end]);
                output.push_str(&close);
                tokens.push(Token {
                    text: open,
                    kind: TokenKind::Open(resource(rich.composition_index)),
                    explanation: part.source.text[start..end].to_owned(),
                });
                tokens.push(Token {
                    text: close,
                    kind: TokenKind::Close(resource(rich.composition_index)),
                    explanation: String::new(),
                });
            }
        }
    }
    output
}

fn numeric_values(text: &str) -> Vec<String> {
    let unmarked = MARKER.replace_all(text, " ");
    let mut values = EXPRESSIONS
        .find_iter(&unmarked)
        .map(|found| {
            found
                .as_str()
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .map(|ch| if ch == '−' { '-' } else { ch })
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn mathematical_symbols(text: &str) -> Vec<char> {
    let text = MARKER.replace_all(text, " ");
    let remainder = EXPRESSIONS.replace_all(&text, " ");
    let mut symbols = SYMBOL
        .find_iter(&remainder)
        .flat_map(|found| found.as_str().chars())
        .collect::<Vec<_>>();
    symbols.sort();
    symbols
}

fn math_expressions(text: &str, tokens: &[Token]) -> Vec<String> {
    let mut text = text.to_owned();
    for token in tokens {
        if !matches!(token.kind, TokenKind::Formula(_)) {
            text = text.replace(&token.text, "");
        }
    }
    let mut expressions = MATH_EXPRESSION.find_iter(&text).map(|expression|
        expression.as_str().chars().filter(|character| !character.is_whitespace())
            .map(|character| if character == '−' { '-' } else { character }).collect::<String>()
    ).collect::<Vec<_>>();
    expressions.sort();
    expressions
}

fn numeric_style_contents(text: &str, tokens: &[Token]) -> Vec<(Resource, Vec<String>)> {
    let mut result = Vec::new();
    for token in tokens {
        let TokenKind::Open(resource) = &token.kind else { continue };
        let Some(close) = tokens.iter().find(|candidate|
            matches!(&candidate.kind, TokenKind::Close(other) if other == resource)) else { continue };
        let Some(start) = text.find(&token.text).map(|offset| offset + token.text.len()) else { continue };
        let Some(end) = text[start..].find(&close.text).map(|offset| offset + start) else { continue };
        result.push((resource.clone(), numeric_values(&text[start..end])));
    }
    result
}

fn has_formula_math_context(text: &str, tokens: &[Token]) -> bool {
    let mut text = text.to_owned();
    for token in tokens {
        if !matches!(token.kind, TokenKind::Formula(_)) {
            text = text.replace(&token.text, "");
        }
    }
    let mathematical_neighbor = |character: char| matches!(character,
        '+' | '-' | '−' | '–' | '*' | '/' | '⁄' | '^' | '=' | '<' | '>' | '≤' | '≥'
            | '×' | '÷' | '±' | '∓' | '(' | ')' | '[' | ']' | '{' | '}');
    tokens.iter().filter(|token| matches!(token.kind, TokenKind::Formula(_))).any(|token| {
        let Some(position) = text.find(&token.text) else { return true };
        text[..position].chars().rev().find(|character| !character.is_whitespace()).is_some_and(mathematical_neighbor)
            || text[position + token.text.len()..].chars().find(|character| !character.is_whitespace()).is_some_and(mathematical_neighbor)
    })
}

fn append_text(parts: &mut Vec<OutputPart>, text: &str, style: Option<Resource>) {
    if text.is_empty() {
        return;
    }
    if let Some(OutputPart::Text {
        text: previous,
        style: previous_style,
    }) = parts.last_mut()
    {
        if *previous_style == style {
            previous.push_str(text);
            return;
        }
    }
    parts.push(OutputPart::Text {
        text: text.to_owned(),
        style,
    });
}

fn decode(input: &str, output: &str, tokens: &[Token]) -> Result<Vec<OutputPart>, String> {
    if output.trim().is_empty()
        || output
            .chars()
            .any(|ch| ch.is_control() && !ch.is_whitespace())
    {
        return Err("empty or invalid translation".into());
    }
    if numeric_values(input) != numeric_values(output)
        || mathematical_symbols(input) != mathematical_symbols(output)
    {
        return Err("numeric expression or mathematical symbol changed".into());
    }
    if math_expressions(input, tokens) != math_expressions(output, tokens)
        || numeric_style_contents(input, tokens) != numeric_style_contents(output, tokens)
    {
        return Err("mathematical relationship or numeric style ownership changed".into());
    }
    let known = tokens
        .iter()
        .map(|token| (token.text.as_str(), token))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut style = None;
    let mut style_required = false;
    let mut style_visible = false;
    let mut style_content = false;
    let mut result = Vec::new();
    let mut cursor = 0;
    for marker in MARKER.find_iter(output) {
        let Some(token) = known.get(marker.as_str()) else {
            return Err("unknown protected marker".into());
        };
        if !seen.insert(marker.as_str()) {
            return Err("duplicate protected marker".into());
        }
        let text = &output[cursor..marker.start()];
        if style.is_some()
            && (if style_visible {
                !text.trim().is_empty()
            } else {
                !text.is_empty()
            })
        {
            style_content = true;
        }
        append_text(&mut result, text, style.clone());
        match &token.kind {
            TokenKind::Formula(resource) => result.push(OutputPart::Formula(resource.clone())),
            TokenKind::Open(resource) => {
                if style.is_some() {
                    return Err("nested style marker".into());
                }
                style = Some(resource.clone());
                style_required = !token.explanation.is_empty();
                style_visible = !token.explanation.trim().is_empty();
                style_content = false;
            }
            TokenKind::Close(resource) => {
                if style.as_ref() != Some(resource) {
                    return Err("unpaired style marker".into());
                }
                if style_required && !style_content {
                    return Err("nonempty source style became empty".into());
                }
                style = None;
            }
        }
        cursor = marker.end();
    }
    append_text(&mut result, &output[cursor..], style.clone());
    if style.is_some()
        || seen.len() != tokens.len()
        || output.matches("[[BABEL_").count() != seen.len()
    {
        return Err("missing or damaged protected marker".into());
    }
    if MARKER
        .replace_all(output, "")
        .to_ascii_lowercase()
        .contains("babel_")
    {
        return Err("damaged protected marker".into());
    }
    Ok(result)
}

fn plain_text(parts: &[OutputPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            OutputPart::Text { text, .. } => text.as_str(),
            OutputPart::Formula(_) => "\u{fffc}",
        })
        .collect()
}

fn split_parts(
    parts: Vec<OutputPart>,
    weights: &[usize],
    locations: &[Location],
) -> Result<Vec<Vec<OutputPart>>, String> {
    let text = plain_text(&parts);
    let expression_ranges = EXPRESSIONS
        .find_iter(&text)
        .map(|found| found.range())
        .collect::<Vec<_>>();
    let mut positions = vec![0];
    for (position, _) in text.char_indices().skip(1) {
        if !cuts_word(&text, position)
            && !expression_ranges
                .iter()
                .any(|range| range.start < position && position < range.end)
        {
            positions.push(position);
        }
    }
    positions.push(text.len());
    let total_weight = weights.iter().sum::<usize>().max(1);
    let total_chars = text.chars().count();
    let mut formulas = Vec::new();
    let mut source_cursor = 0;
    for part in &parts {
        match part {
            OutputPart::Text { text, .. } => source_cursor += text.len(),
            OutputPart::Formula(resource) => {
                formulas.push((source_cursor, resource.location.page));
                source_cursor += '\u{fffc}'.len_utf8();
            }
        }
    }
    let mut cuts = vec![0];
    let mut cumulative = 0;
    for (cut_index, weight) in weights
        .iter()
        .take(weights.len().saturating_sub(1))
        .enumerate()
    {
        cumulative += weight;
        let target = (total_chars as u128 * cumulative as u128 / total_weight as u128) as usize;
        let previous = *cuts.last().unwrap();
        let left_page = locations[cut_index].page;
        let right_page = locations[cut_index + 1].page;
        let best = positions
            .iter()
            .copied()
            .filter(|&position| {
                position >= previous
                    && (left_page == right_page
                        || formulas.iter().all(|&(offset, page)| {
                            if page <= left_page {
                                offset + '\u{fffc}'.len_utf8() <= position
                            } else {
                                offset >= position
                            }
                        }))
            })
            .min_by_key(|&position| {
                let distance = text[..position].chars().count().abs_diff(target);
                let leading_punctuation = text[position..]
                    .chars()
                    .next()
                    .is_some_and(|ch| "，。；：！？、,.!?;:".contains(ch));
                (
                    distance,
                    leading_punctuation,
                    !text[..position].ends_with(char::is_whitespace),
                )
            })
            .ok_or("formula order requires unsupported page-resource movement")?;
        cuts.push(best);
    }
    cuts.push(text.len());
    let mut result = vec![Vec::new(); weights.len()];
    let mut cursor = 0;
    for part in parts {
        match part {
            OutputPart::Text { text: value, style } => {
                let end = cursor + value.len();
                for (index, pair) in cuts.windows(2).enumerate() {
                    let start = pair[0].max(cursor);
                    let stop = pair[1].min(end);
                    if start < stop {
                        append_text(
                            &mut result[index],
                            &value[start - cursor..stop - cursor],
                            style.clone(),
                        );
                    }
                }
                cursor = end;
            }
            OutputPart::Formula(resource) => {
                let index = cuts
                    .windows(2)
                    .position(|pair| pair[0] <= cursor && cursor < pair[1])
                    .unwrap_or(result.len() - 1);
                if resource.location.page != locations[index].page {
                    return Err("formula requires resources from a different page".into());
                }
                result[index].push(OutputPart::Formula(resource));
                cursor += '\u{fffc}'.len_utf8();
            }
        }
    }
    Ok(result)
}

impl DocumentPlan {
    pub fn new(mut selections: Vec<DocumentSelection>) -> Result<Self, String> {
        let mut index = 0;
        while index < selections.len() {
            let mut other = index + 1;
            while other < selections.len() {
                if overlaps(&selections[index], &selections[other]) {
                    let right = selections.remove(other);
                    let left = selections.remove(index);
                    selections.insert(index, merge(left, right)?);
                    other = index + 1;
                } else {
                    other += 1;
                }
            }
            index += 1;
        }
        let mut sources = Vec::new();
        let mut ranges: HashMap<Location, Vec<Range<usize>>> = HashMap::new();
        for selection in &selections {
            if selection.parts.len() < 2 || selection.parts.len() != selection.joins.len() + 1 {
                return Err("invalid sentence selection".into());
            }
            for part in &selection.parts {
                if !part.source.valid_range(&part.range) {
                    return Err("invalid sentence range".into());
                }
                if !sources
                    .iter()
                    .any(|source: &Source| source.location == part.source.location)
                {
                    sources.push(part.source.clone());
                }
                ranges
                    .entry(part.source.location)
                    .or_default()
                    .push(part.range.clone());
            }
        }
        let mut plan = Self {
            items: Vec::new(),
            metadata: Vec::new(),
            sources,
            capacities: HashMap::new(),
        };
        for selection in selections {
            plan.add_item(selection.parts, selection.joins)?;
        }
        for source in plan.sources.clone() {
            let selected = ranges.get_mut(&source.location).unwrap();
            selected.sort_by_key(|range| range.start);
            let mut cursor = 0;
            let tail = source.text.len()..source.text.len();
            for range in selected.iter().chain(std::iter::once(&tail)) {
                if range.start < cursor {
                    return Err("overlapping source ranges".into());
                }
                if cursor < range.start {
                    plan.add_item(
                        vec![SelectedPart {
                            source: source.clone(),
                            range: cursor..range.start,
                        }],
                        vec![],
                    )?;
                }
                cursor = range.end;
            }
        }
        Ok(plan)
    }

    fn add_item(&mut self, ranges: Vec<SelectedPart>, joins: Vec<Join>) -> Result<(), String> {
        let mut tokens = Vec::new();
        let mut text = String::new();
        let mut weights = Vec::new();
        for (index, part) in ranges.iter().enumerate() {
            if index > 0 {
                match joins[index - 1] {
                    Join::Space => text.push(' '),
                    Join::None => {}
                    Join::Dehyphenate => {
                        if !part.source.text[part.range.clone()].starts_with(char::is_alphabetic) {
                            return Err("invalid dehyphenation join".into());
                        }
                        remove_line_hyphen(&mut text)?;
                    }
                }
            }
            let piece = encode(part, self.items.len(), &mut tokens);
            weights.push(
                MARKER
                    .replace_all(&piece, "x")
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .count()
                    .max(1),
            );
            text.push_str(&piece);
        }
        let prefix = text[..text.len() - text.trim_start().len()].to_owned();
        let suffix = text[text.trim_end().len()..].to_owned();
        if has_formula_math_context(&text, &tokens) {
            return Err("formula in mathematical context requires ordinary translation".into());
        }
        self.items.push(text);
        self.metadata.push(Item {
            ranges,
            tokens,
            weights,
            prefix,
            suffix,
        });
        Ok(())
    }

    pub fn formula_context(&self) -> Vec<(String, String)> {
        self.metadata
            .iter()
            .flat_map(|item| &item.tokens)
            .filter_map(|token| {
                matches!(token.kind, TokenKind::Formula(_))
                    .then(|| (token.text.clone(), token.explanation.clone()))
            })
            .collect()
    }

    pub fn set_capacities(&mut self, capacities: HashMap<Location, usize>) {
        self.capacities = capacities;
    }

    pub fn locations(&self) -> impl Iterator<Item = Location> + '_ {
        self.sources.iter().map(|source| source.location)
    }

    pub fn validate_items(&self, start: usize, responses: &[String]) -> Result<(), String> {
        if start
            .checked_add(responses.len())
            .is_none_or(|end| end > self.items.len())
        {
            return Err("translation item count differs".into());
        }
        for (offset, response) in responses.iter().enumerate() {
            let index = start + offset;
            if !self.items[index].trim().is_empty() {
                decode(&self.items[index], response, &self.metadata[index].tokens)?;
            }
        }
        Ok(())
    }

    pub fn resolve(&self, responses: &[String]) -> Result<Vec<ResolvedParagraph>, String> {
        if responses.len() != self.items.len() {
            return Err("translation item count differs".into());
        }
        let mut by_location: HashMap<Location, BTreeMap<usize, Vec<OutputPart>>> = HashMap::new();
        let mut reserved: HashMap<Location, usize> = HashMap::new();
        for (item, response) in self.metadata.iter().zip(responses) {
            if item.ranges.len() == 1 {
                *reserved.entry(item.ranges[0].source.location).or_default() +=
                    MARKER.replace_all(response, "x").chars().count();
            }
        }
        for ((input, item), response) in self.items.iter().zip(&self.metadata).zip(responses) {
            let response = if input.trim().is_empty() {
                input.clone()
            } else {
                format!("{}{}{}", item.prefix, response.trim(), item.suffix)
            };
            let parts = if input.trim().is_empty() {
                vec![OutputPart::Text {
                    text: input.clone(),
                    style: None,
                }]
            } else {
                decode(input, &response, &item.tokens)?
            };
            let locations = item
                .ranges
                .iter()
                .map(|part| part.source.location)
                .collect::<Vec<_>>();
            let weights = if locations.len() > 1 {
                locations
                    .iter()
                    .zip(&item.weights)
                    .map(|(location, weight)| {
                        self.capacities.get(location).map_or(*weight, |capacity| {
                            capacity
                                .saturating_sub(*reserved.get(location).unwrap_or(&0))
                                .max(1)
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                item.weights.clone()
            };
            let split = split_parts(parts, &weights, &locations)?;
            for (part, result) in item.ranges.iter().zip(split) {
                by_location
                    .entry(part.source.location)
                    .or_default()
                    .insert(part.range.start, result);
            }
        }
        Ok(self
            .sources
            .iter()
            .map(|source| ResolvedParagraph {
                location: source.location,
                parts: by_location
                    .remove(&source.location)
                    .unwrap_or_default()
                    .into_values()
                    .flatten()
                    .collect(),
            })
            .collect())
    }
}
