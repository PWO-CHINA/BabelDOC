use babeldoc_p2_stages::translation::cross_boundary::sentences::sentence_ends;

#[test]
fn scientific_punctuation_keeps_main_clause_and_citations() {
    for text in [
        "Majeti et al.[[BABEL_F_0_0_0]] reported 3.5% (Fig. 2).[[BABEL_F_0_0_1]] Next.",
        "The result was stable.[12, 13] Next.",
    ] {
        assert_eq!(
            sentence_ends(text),
            vec![text.find(" Next.").unwrap(), text.len()]
        );
    }
    let text = "See e.g. A. Smith in Fig. 2 for details.";
    assert_eq!(sentence_ends(text), vec![text.len()]);
    let text = "Earlier result. [[BABEL_F_0_0_0]] expression increased.";
    assert_eq!(sentence_ends(text), vec![15, text.len()]);
}

#[test]
fn dependent_clauses_and_unfinished_predicates_have_no_new_sentence() {
    assert!(
        sentence_ends("For example, in situ hybridization analyses of samples from 302 patients")
            .is_empty()
    );
    let text =
        "Earlier finding. This article describes mechanisms, which owing to improved models has";
    assert_eq!(sentence_ends(text), vec![16]);
}
