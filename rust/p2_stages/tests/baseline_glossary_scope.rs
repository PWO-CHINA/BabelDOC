use babeldoc_p2_stages::{
    runtime::shared_context::SharedContextRoot,
    translation::{
        automatic_term_extractor::{self, TermBatch},
        glossary::{Glossary, GlossaryEntry, term_context},
        llm_only,
        ordinary_translation::glossary_block,
    },
};

fn batch(paragraphs: Vec<&str>, labels: Vec<Option<&str>>) -> TermBatch {
    TermBatch {
        page_index: 0,
        tracking_index: 0,
        paragraphs: paragraphs.into_iter().map(str::to_owned).collect(),
        layout_labels: labels.into_iter().map(|value| value.map(str::to_owned)).collect(),
        token_count: 0,
        prompt: String::new(),
    }
}

#[test]
fn saved_asp_reference_cannot_infect_body_or_ambiguous_batches() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap();
    let path = repo.join(".crossboundary-acceptance-r2-20261112/06-Asp/work/Asp_2020_第14-15页/term_extractor_tracking.json");
    let request_path = repo.join(".crossboundary-acceptance-r2-20261112/06-Asp/requests.jsonl");
    if !path.is_file() || !request_path.is_file() {
        eprintln!("saved Asp evidence is unavailable; synthetic scope tests still run");
        return;
    }
    let tracking: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let saved = &tracking["page"][1]["paragraph"][0];
    let paragraphs = saved["pdf_unicodes"].as_array().unwrap();
    let saved_batch = batch(
        paragraphs.iter().map(|value| value.as_str().unwrap()).collect(),
        vec![Some("text"); paragraphs.len()],
    );
    let response = saved["output"].as_str().unwrap();
    let terms = automatic_term_extractor::parse_terms(response).unwrap();
    assert!(terms.iter().any(|(source, target)| source == "Development" && target == "《发育》"));
    assert!(automatic_term_extractor::source_contexts(&saved_batch, "Development").is_empty());

    let request = std::fs::read_to_string(request_path).unwrap().lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|value| value["sequence"] == 15).unwrap();
    let prompt = request["prompt"].as_str().unwrap();
    let input: Vec<String> = serde_json::from_str(prompt.split(" Input: ").nth(1).unwrap()).unwrap();
    let body = input.iter().find(|text| text.contains("technological development")).unwrap();

    let root = SharedContextRoot::new(Vec::new());
    for term in automatic_term_extractor::sourced_terms(&saved_batch, terms) {
        root.add_scoped_extracted_term(term.source, term.target, term.contexts);
    }
    root.finalize_auto_extracted_glossary().unwrap();
    assert!(!glossary_block(body, &root.get_glossaries_for_translation(true)).contains("《发育》"));
}

#[test]
fn source_context_is_bounded_and_suggestions_are_optional() {
    let reference = "[21] A. Scientist, Example Journal Development 2016, 143, 2862. The reference continues.";
    let body = "A driver behind technological development is collaboration between researchers.";
    let root = SharedContextRoot::new(vec![Glossary::new("user", vec![GlossaryEntry::new("development", "发展")]).unwrap()]);
    let scoped = batch(vec![reference], vec![Some("reference")]);
    let contexts = automatic_term_extractor::source_contexts(&scoped, "Development");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].layout_label.as_deref(), Some("reference"));
    root.add_scoped_extracted_term("Development".into(), "《发育》".into(), contexts);
    root.finalize_auto_extracted_glossary().unwrap();
    let with_auto = root.get_glossaries_for_translation(true);
    assert_eq!(with_auto.len(), 2);
    let reference_block = glossary_block(reference, &with_auto);
    assert!(reference_block.contains("Always use the glossary"));
    assert!(reference_block.contains("| development | 发展 |"));
    assert!(!reference_block.contains("《发育》"));
    let auto_only = root.get_glossaries_for_translation(true).into_iter().skip(1).collect::<Vec<_>>();
    let suggested_block = glossary_block(reference, &auto_only);
    assert!(suggested_block.contains("optional suggestions"));
    assert!(suggested_block.contains("source region: reference"));
    assert!(suggested_block.contains("Development → 《发育》"));
    let body_block = glossary_block(body, &with_auto);
    assert!(body_block.contains("| development | 发展 |"));
    assert!(!body_block.contains("《发育》"));
    let mixed = format!("{reference}\n\n{body}");
    assert!(!glossary_block(&mixed, &with_auto).contains("《发育》"));
    let longer = format!("{} {} {reference} {}", "context ".repeat(40), body, "ending ".repeat(40));
    assert!(!glossary_block(&longer, &with_auto).contains("《发育》"));
    assert_eq!(root.get_glossaries_for_translation(false).len(), 1);

    let batch_prompt = llm_only::render_batch_prompt("role", "Chinese", "[]", reference, None, None, &with_auto);
    assert!(batch_prompt.contains("| development | 发展 |"));
    assert!(!batch_prompt.contains("《发育》"));
    let user_only = SharedContextRoot::new(vec![Glossary::new("user", vec![GlossaryEntry::new("cell", "细胞")]).unwrap()]);
    assert!(glossary_block("A cell appeared in the sample.", &user_only.get_glossaries_for_translation(true)).contains("| cell | 细胞 |"));
}

#[test]
fn automatic_words_need_boundaries_and_conflicts_are_not_voted() {
    for (text, source) in [
        ("Our research studies cellular structures and their behavior.", "cell"),
        ("Researchers studied superdevelopmental changes over time.", "development"),
        ("Researchers examined development_models across samples.", "development"),
    ] {
        assert!(term_context(text, source, None).is_none(), "{text}");
    }
    assert!(term_context("The cell develops within a complex biological environment.", "cell", Some("text".into())).is_some());
    assert!(term_context("Methods such as deep neural networks support this important result.", "deep neural network", None).is_none());
    assert!(term_context("Methods such as a deep neural\n network support this important result.", "deep neural network", None).is_some());
    assert!(term_context("Our research analyzes biochemical processes in a laboratory.", "chemical", None).is_none());
    assert!(term_context("Researchers asked aaaa about the observed phenomenon.", "aaa", None).is_none());
    assert!(term_context("The cell\u{0301} sample was examined during the investigation.", "cell", None).is_none());
    assert!(term_context("The CELL develops within a complex biological environment.", "cell", None).is_some());
    assert!(term_context("A cell and another cell appear in the biological study.", "cell", None).is_none());
    let root = SharedContextRoot::new(Vec::new());
    let first = "Research on cell biology explains important cellular functions.";
    let second = "The prison cell contained several occupants during the study.";
    for (text, target) in [(first, "细胞"), (first, "细胞"), (second, "牢房")] {
        root.add_scoped_extracted_term("cell".into(), target.into(), vec![term_context(text, "cell", None).unwrap()]);
    }
    root.finalize_auto_extracted_glossary().unwrap();
    assert!(root.auto_extracted_glossary().is_none());
}

#[test]
fn multi_paragraph_batches_remain_unscoped_without_explicit_source_attribution() {
    let references = "[21] A. Scientist, Development 2016, 143, 2862. The reference continues.";
    let body = "A driver behind technological development is collaboration between researchers.";
    let unrelated = "Modern microscopy creates detailed images of biological tissue.";
    let reference_batch = batch(vec![unrelated, references], vec![Some("text"), Some("reference")]);
    assert!(automatic_term_extractor::source_contexts(&reference_batch, "Development").is_empty());

    let single_reference = batch(vec![references], vec![Some("reference")]);
    let context = automatic_term_extractor::source_contexts(&single_reference, "Development");
    assert_eq!(context.len(), 1);
    assert_eq!(context[0].layout_label.as_deref(), Some("reference"));

    let root = SharedContextRoot::new(Vec::new());
    root.add_scoped_extracted_term("Development".into(), "《发育》".into(), context);
    root.finalize_auto_extracted_glossary().unwrap();
    let glossaries = root.get_glossaries_for_translation(true);
    assert!(glossary_block(references, &glossaries).contains("Development → 《发育》"));
    assert!(!glossary_block(body, &glossaries).contains("《发育》"));

    let ambiguous = batch(vec![references, body], vec![Some("reference"), Some("text")]);
    assert!(automatic_term_extractor::source_contexts(&ambiguous, "Development").is_empty());
    let short_match = batch(vec![references, "Development"], vec![Some("reference"), Some("title")]);
    assert!(automatic_term_extractor::source_contexts(&short_match, "Development").is_empty());
    let missing_label = batch(vec![references], vec![]);
    assert!(automatic_term_extractor::source_contexts(&missing_label, "Development").is_empty());
}

#[test]
fn suggestions_require_full_source_context_not_case_folded_or_adjacent_text() {
    let journal = "Reference: Development 2016, 143, 2862, and related publications.";
    let ordinary = "Reference: development 2016, 143, 2862, and related publications.";
    let root = SharedContextRoot::new(Vec::new());
    root.add_scoped_extracted_term(
        "Development".into(),
        "《发育》".into(),
        vec![term_context(journal, "Development", Some("reference".into())).unwrap()],
    );
    root.finalize_auto_extracted_glossary().unwrap();
    let glossaries = root.get_glossaries_for_translation(true);
    assert!(glossary_block(journal, &glossaries).contains("《发育》"));
    assert!(!glossary_block(ordinary, &glossaries).contains("《发育》"));
    assert!(!glossary_block(&format!("{journal}\nAdditional body text."), &glossaries).contains("《发育》"));
}

#[test]
fn ambiguous_batch_terms_still_suppress_conflicting_scoped_suggestions() {
    let first = "Research on cell biology explains important cellular functions.";
    let second = "The prison cell contained several occupants during the study.";
    let root = SharedContextRoot::new(Vec::new());
    root.add_scoped_extracted_term("cell".into(), "细胞".into(), vec![term_context(first, "cell", None).unwrap()]);
    let ambiguous = batch(vec![second, "Another paragraph in the same extraction batch."], vec![Some("text"), Some("text")]);
    for term in automatic_term_extractor::sourced_terms(&ambiguous, vec![("Cell".into(), "牢房".into())]) {
        root.add_scoped_extracted_term(term.source, term.target, term.contexts);
    }
    root.finalize_auto_extracted_glossary().unwrap();
    assert!(root.auto_extracted_glossary().is_none());
}

#[test]
fn explicit_user_phrase_suppresses_overlapping_automatic_suggestion() {
    let text = "The cell biology findings improved knowledge of cellular development.";
    let user = Glossary::new("user", vec![GlossaryEntry::new("cell", "细胞")]).unwrap();
    let root = SharedContextRoot::new(vec![user]);
    root.add_scoped_extracted_term("cell biology".into(), "生物学".into(), vec![term_context(text, "cell biology", Some("text".into())).unwrap()]);
    root.finalize_auto_extracted_glossary().unwrap();
    let block = glossary_block(text, &root.get_glossaries_for_translation(true));
    assert!(block.contains("| cell | 细胞 |"));
    assert!(!block.contains("cell biology → 生物学"));
}

#[test]
fn raw_unscoped_auto_entries_cannot_become_prompt_constraints() {
    let root = SharedContextRoot::new(Vec::new());
    root.add_raw_extracted_term_pair("Nature".into(), "《自然》".into());
    root.finalize_auto_extracted_glossary().unwrap();
    assert!(root.auto_extracted_glossary().is_some());
    let text = "The nature of this material depends on its composition.";
    assert!(glossary_block(text, &root.get_glossaries_for_translation(true)).is_empty());
}
