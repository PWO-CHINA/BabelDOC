#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl Rect {
    fn valid(self) -> bool {
        [self.left, self.bottom, self.right, self.top]
            .iter()
            .all(|value| value.is_finite())
            && self.left < self.right
            && self.bottom < self.top
    }

    fn contains(self, inner: Self) -> bool {
        self.valid()
            && inner.valid()
            && inner.left >= self.left
            && inner.right <= self.right
            && inner.bottom >= self.bottom
            && inner.top <= self.top
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PageFacts {
    pub number: usize,
    pub selected: bool,
    pub bounds: Rect,
    pub reading_order_unambiguous: bool,
    pub body_columns: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct ParagraphFacts<'a> {
    pub text: &'a str,
    pub bounds: Rect,
    pub is_body: bool,
    pub is_boundary_body: bool,
    pub plain_single_style: bool,
    pub column: usize,
}

pub fn has_context_sentence_evidence(left: &str, right: &str) -> bool {
    context_sentence_parts(left, right).is_some()
}

pub fn numeric_continuation_needs_review(left: &str, right: &str) -> bool {
    let Some((tail, continuation)) = context_sentence_parts(left, right) else { return false };
    continuation.chars().next().is_some_and(|value| value.is_ascii_digit())
        && tail.chars().any(|value| value.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContinuationJoin {
    Space,
    PageBreakHyphen,
}

fn continuation_join(left: &str, right: &str) -> Option<ContinuationJoin> {
    let last = left.chars().last()?;
    let first = right.chars().next()?;
    if first.is_lowercase() && (last.is_alphanumeric() || matches!(last, ',' | ';')) {
        return Some(ContinuationJoin::Space);
    }
    if last.is_alphanumeric() && first.is_ascii_digit() {
        let digits = right.chars().take_while(char::is_ascii_digit).count();
        let rest = right.get(digits..)?;
        if (1..=4).contains(&digits) && rest.starts_with(' ')
            && rest.trim_start().chars().next().is_some_and(char::is_lowercase) {
            return Some(ContinuationJoin::Space);
        }
    }
    if last == '-' {
        let before = left.strip_suffix('-')?;
        let prefix = before.chars().rev().take_while(char::is_ascii_lowercase).count();
        let suffix = right.chars().take_while(char::is_ascii_lowercase).count();
        if prefix >= 4 && suffix >= 3 {
            return Some(ContinuationJoin::PageBreakHyphen);
        }
    }
    None
}

pub(super) fn context_sentence_parts<'a>(left: &'a str, right: &'a str) -> Option<(&'a str, &'a str)> {
    fn boundary(text: &str) -> Option<usize> {
        for (index, character) in text.char_indices() {
            if !matches!(character, '.' | '!' | '?' | '。' | '！' | '？') { continue; }
            let end = index + character.len_utf8();
            let previous = text[..index].chars().next_back()?;
            let next = text[end..].chars().next();
            if character == '.' && previous.is_alphanumeric() {
                if previous.is_ascii_digit() && next.is_some_and(|value| value.is_ascii_digit()) { continue; }
                if previous.is_ascii_digit() {
                    let digits = text[..index].chars().rev().take_while(|value| value.is_ascii_digit()).count();
                    let before = &text[..index - digits];
                    if digits < 2 || before.ends_with('.') || before.split_whitespace().count() < 3
                        || next.is_none_or(|value| !value.is_whitespace())
                        || text[end..].trim_start().chars().next().is_none_or(|value| !value.is_uppercase()) { continue; }
                } else {
                    let word = text[..index].rsplit(|value: char| !value.is_alphabetic()).next()?;
                    if word.chars().count() <= 3 || matches!(word.to_ascii_lowercase().as_str(), "prof" | "assoc" | "dept" | "approx" | "figs" | "etc") { continue; }
                }
            }
            if !(previous.is_alphanumeric() || matches!(previous, ')' | ']'))
                || next.is_some_and(|value| !value.is_whitespace()) { continue; }
            return Some(end);
        }
        None
    }
    fn plausible_fragment(text: &str) -> bool {
        let text = text.trim();
        text.chars().filter(|value| value.is_alphabetic()).count() >= 4
            && text.split_whitespace().count() >= 2
            && !text.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with(['•', '▪', '◦', '-', '–', '—'])
                    || line.split_once('.').is_some_and(|(prefix, rest)| !prefix.is_empty() && prefix.chars().all(|value| value.is_ascii_digit()) && rest.starts_with(char::is_whitespace))
            })
    }
    let left = left.trim();
    let right = right.trim();
    let mut left_end = 0;
    while let Some(end) = boundary(&left[left_end..]) {
        left_end += end;
    }
    let tail = if left_end == 0 {
        if left.chars().next().is_none_or(|value| !value.is_uppercase())
            || left.split_whitespace().count() < 5
            || left.chars().any(|value| matches!(value, '.' | '!' | '?' | '。' | '！' | '？' | ':' | '：')) {
            return None;
        }
        left
    } else {
        if !plausible_fragment(&left[..left_end]) { return None; }
        left[left_end..].trim()
    };
    if !plausible_fragment(tail) || continuation_join(tail, right).is_none() {
        return None;
    }
    let right_end = boundary(right)?;
    plausible_fragment(&right[..right_end]).then_some((tail, &right[..right_end]))
}

pub fn extract_context_sentence(left: &str, right: &str) -> Option<String> {
    let (left, right) = context_sentence_parts(left, right)?;
    let right_body = right.strip_suffix(['.', '!', '?', '。', '！', '？'])?;
    let fragments = format!("{left} {right_body}");
    let mut parentheses = 0usize;
    for (index, character) in fragments.char_indices() {
        match character {
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.checked_sub(1)?,
            '.' => {
                let word = fragments[..index].rsplit(|value: char| !value.is_alphabetic()).next()?;
                if parentheses == 0 || !matches!(word.to_ascii_lowercase().as_str(), "fig" | "figs") {
                    return None;
                }
            }
            '!' | '?' | '。' | '！' | '？' => return None,
            value if value.is_control() => return None,
            _ => {}
        }
    }
    if parentheses != 0 { return None; }
    let separator = match continuation_join(left, right)? {
        ContinuationJoin::Space => " ",
        ContinuationJoin::PageBreakHyphen => "\n",
    };
    Some(format!("{}{separator}{}", left.trim(), right.trim()))
}

/// Identifies a conservative candidate; it does not establish translation quality.
/// Coordinates use PDF space, where larger y values are nearer the page top.
pub fn is_continuation(
    left_page: PageFacts,
    left: ParagraphFacts<'_>,
    right_page: PageFacts,
    right: ParagraphFacts<'_>,
) -> bool {
    if left_page.number.checked_add(1) != Some(right_page.number)
        || !left_page.selected
        || !right_page.selected
        || !left_page.reading_order_unambiguous
        || !right_page.reading_order_unambiguous
        || !left_page.bounds.contains(left.bounds)
        || !right_page.bounds.contains(right.bounds)
        || !left.is_body
        || !right.is_body
        || !left.is_boundary_body
        || !right.is_boundary_body
        || !left.plain_single_style
        || !right.plain_single_style
        || !(1..=2).contains(&left_page.body_columns)
        || left_page.body_columns != right_page.body_columns
        || left.column.checked_add(1) != Some(left_page.body_columns)
        || right.column != 0
    {
        return false;
    }

    let left_width = left_page.bounds.right - left_page.bounds.left;
    let right_width = right_page.bounds.right - right_page.bounds.left;
    let left_height = left_page.bounds.top - left_page.bounds.bottom;
    let right_height = right_page.bounds.top - right_page.bounds.bottom;
    let left_x = (left.bounds.left - left_page.bounds.left) / left_width;
    let right_x = (right.bounds.left - right_page.bounds.left) / right_width;
    let left_extent = (left.bounds.right - left.bounds.left) / left_width;
    let right_extent = (right.bounds.right - right.bounds.left) / right_width;
    let left_bottom = (left.bounds.bottom - left_page.bounds.bottom) / left_height;
    let right_top = (right.bounds.top - right_page.bounds.bottom) / right_height;

    let columns_match = if left_page.body_columns == 1 {
        left_extent >= 0.55 && right_extent >= 0.55 && (left_x - right_x).abs() <= 0.04
    } else {
        let left_right = (left.bounds.right - left_page.bounds.left) / left_width;
        let right_right = (right.bounds.right - right_page.bounds.left) / right_width;
        (0.25..=0.48).contains(&left_extent)
            && (0.25..=0.48).contains(&right_extent)
            && left_right <= 0.95
            && right_x >= 0.05
            && left_x >= right_right
            && (left_extent - right_extent).abs() <= 0.08
    };
    if !columns_match
        || !(0.05..=0.30).contains(&left_bottom)
        || !(0.70..=0.95).contains(&right_top)
    {
        return false;
    }

    let left_text = left.text.trim();
    let right_text = right.text.trim();
    let Some(last) = left_text.chars().last() else {
        return false;
    };
    let Some(first) = right_text.chars().next() else {
        return false;
    };
    if right.text.starts_with(char::is_whitespace) { return false; }
    if last.is_alphanumeric() && first.is_lowercase() { return true; }
    continuation_join(left_text, right_text).is_some() && extract_context_sentence(left_text, right_text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(number: usize) -> PageFacts {
        PageFacts {
            number,
            selected: true,
            bounds: Rect { left: 0.0, bottom: 0.0, right: 600.0, top: 800.0 },
            reading_order_unambiguous: true,
            body_columns: 1,
        }
    }

    fn paragraph(text: &str, bottom: f64, top: f64) -> ParagraphFacts<'_> {
        ParagraphFacts {
            text,
            bounds: Rect { left: 60.0, bottom, right: 540.0, top },
            is_body: true,
            is_boundary_body: true,
            plain_single_style: true,
            column: 0,
        }
    }

    #[test]
    fn identifies_adjacent_plain_body_continuation() {
        assert!(is_continuation(page(1), paragraph("The result depends on", 80.0, 150.0), page(2), paragraph("the measured values.", 650.0, 720.0)));
    }

    #[test]
    fn rejects_completed_sentences_ambiguous_hyphens_and_new_paragraphs() {
        for text in ["The result.", "The result?", "The result-", "The result—", ""] {
            assert!(!is_continuation(page(1), paragraph(text, 80.0, 150.0), page(2), paragraph("the next words", 650.0, 720.0)));
        }
        assert!(!is_continuation(page(1), paragraph("The result depends on", 80.0, 150.0), page(2), paragraph("A new paragraph", 650.0, 720.0)));
    }

    #[test]
    fn accepts_only_evidenced_punctuation_and_number_boundaries() {
        for (left, right) in [
            ("After analysis, 7 of the recovered", "11 samples were checked successfully."),
            ("Integration of data from genomics,", "transcriptomics and proteomics improve treatment."),
            ("Cellular transitions continue during develop-", "ment in the collecting duct."),
        ] {
            assert!(is_continuation(page(1), paragraph(left, 80.0, 150.0), page(2), paragraph(right, 650.0, 720.0)), "{left:?} {right:?}");
        }
        for (left, right) in [
            ("A study of the selected", "1. New numbered section."),
            ("A study of the selected", "2024 Results start here."),
            ("An unfinished sentence ends with a comma,", "New heading begins here."),
            ("The analysis includes state-of-", "the-art techniques."),
            ("A study of the selected", "11 samples without punctuation"),
        ] {
            assert!(!is_continuation(page(1), paragraph(left, 80.0, 150.0), page(2), paragraph(right, 650.0, 720.0)), "{left:?} {right:?}");
        }
    }

    #[test]
    fn rejects_unselected_skipped_or_ambiguous_pages() {
        let left = paragraph("The result depends on", 80.0, 150.0);
        let right = paragraph("the measured values", 650.0, 720.0);
        let mut next = page(2);
        next.selected = false;
        assert!(!is_continuation(page(1), left, next, right));
        next.selected = true;
        next.reading_order_unambiguous = false;
        assert!(!is_continuation(page(1), left, next, right));
        assert!(!is_continuation(page(1), left, page(3), right));
        assert!(!is_continuation(page(usize::MAX), left, page(0), right));
    }

    #[test]
    fn rejects_non_body_non_boundary_and_complex_content() {
        let left = paragraph("The result depends on", 80.0, 150.0);
        let right = paragraph("the measured values", 650.0, 720.0);
        let mut candidate = left;
        candidate.is_body = false;
        assert!(!is_continuation(page(1), candidate, page(2), right));
        candidate = left;
        candidate.is_boundary_body = false;
        assert!(!is_continuation(page(1), candidate, page(2), right));
        candidate = left;
        candidate.plain_single_style = false;
        assert!(!is_continuation(page(1), candidate, page(2), right));
    }

    #[test]
    fn rejects_headers_footers_columns_and_invalid_boxes() {
        let left = paragraph("The result depends on", 80.0, 150.0);
        let right = paragraph("the measured values", 650.0, 720.0);
        assert!(!is_continuation(page(1), paragraph(left.text, 10.0, 30.0), page(2), right));
        assert!(!is_continuation(page(1), left, page(2), paragraph(right.text, 760.0, 790.0)));
        let mut narrow = left;
        narrow.bounds.right = 260.0;
        assert!(!is_continuation(page(1), narrow, page(2), right));
        let mut shifted = right;
        shifted.bounds.left = 100.0;
        assert!(!is_continuation(page(1), left, page(2), shifted));
        let mut invalid = left;
        invalid.bounds.left = f64::NAN;
        assert!(!is_continuation(page(1), invalid, page(2), right));
        invalid.bounds.left = -1.0;
        assert!(!is_continuation(page(1), invalid, page(2), right));
    }

    #[test]
    fn identifies_last_column_to_next_first_column() {
        let mut left_page = page(1);
        let mut right_page = page(2);
        left_page.body_columns = 2;
        right_page.body_columns = 2;
        let mut left = paragraph("The result depends on", 80.0, 150.0);
        left.bounds.left = 350.0;
        left.bounds.right = 555.0;
        left.column = 1;
        let mut right = paragraph("the measured values", 650.0, 720.0);
        right.bounds.left = 140.0;
        right.bounds.right = 345.0;
        assert!(is_continuation(left_page, left, right_page, right));
        left.column = 0;
        assert!(!is_continuation(left_page, left, right_page, right));
        left.column = 1;
        right.column = 1;
        assert!(!is_continuation(left_page, left, right_page, right));
        right.column = 0;
        right_page.reading_order_unambiguous = false;
        assert!(!is_continuation(left_page, left, right_page, right));
    }
}
