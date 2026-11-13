# PWO cross-page experiment

This experimental fork is based on BabelDOC commit
`dd9adc352b957533d05b8cb94e88a7589cc56a35`. It retains upstream's AGPL-3.0
license and is identified as `0.7.1+pwo.2.crosspage.17`. This is an
experimental fork version, not an upstream release.

The PWO formula-order change prevents formula groups separated by other paragraph content from
being merged across that content. This avoids changing character order in the
synthetic identifier exercised by the regression test. Adjacent groups retain
their existing merge behavior. It does not protect bibliography metadata,
preserve PDF links or guarantee translation or layout accuracy. Reference
formats, titles, authors, dates, identifiers and links still require human
review. The PWO `0.7.1+pwo.2` paragraph-spacing fix from commit
`01ccca72e233c182892a5db38f235bd81dfe9935` is also included. The more
extensive local bibliography and link experiments are not
included in this patch because their coverage and safety are not established.

The optional cross-page and cross-column enhancement starts after ordinary
translation and output have completed. Eligible continuations are translated as
complete sentences in isolated candidates and redistributed to the original
pages or columns. Failed or rejected enhancement groups retain the completed
ordinary translation and report that the enhancement was not applied. Ordinary
translation, cancellation and real output errors still fail the job.
The numeric check compares recognized digit and operator expressions exactly;
it is not a semantic check of units, written numbers, or other translations.
Numeric formatting changes or unrecognized notation can require manual review.
Decimal sentence boundaries are conservative: explicitly signed decimals can
be recognized, while ambiguous unsigned decimals may be skipped or blocked.
The option is disabled by default and does not ensure that every continuation
is detected, translated correctly, or laid out safely.

Candidate responses are matched by explicit IDs and checked for numeric,
formula, style and layout consistency. Dependent boundaries sharing a paragraph
are committed together; outputs are verified before replacing baseline files,
with rollback on publication failure. Automatic glossary suggestions retain
source context and do not become global mandatory glossary entries. Rotated
source text and verified spacing are preserved without reordering formulas.
TOC output I/O errors propagate. If baseline restoration itself fails, the job
reports the original failure, restoration errors and recovery backup paths;
output reports mark the publication state as uncertain.
Independent numbered paragraphs and candidates with explicit non-body,
small-font or adjacent-caption evidence retain ordinary translation even if
the model attempts to select them.

Nine real two-page samples were tested through the installed Windows backend
with DeepSeek. All produced monolingual and bilingual outputs. Three of eleven
designated continuation targets were translated together and applied; eight
retained ordinary translation. This is not a claim that all translation quality
issues are fixed. Version strings, URLs, bibliography formatting, labels and PDF
link annotations have known defects deferred to a later version. Chinese words
can split across pages or columns. Strict numeric, formula and layout guards
remain in place. See `docs/release-notes/v0.7.1+pwo.2.crosspage.17.md` for scope.
Final release preparation adds conservative candidate exclusions, so this earlier
sample application rate does not establish the final version's application rate.

Continuations near figures or captions, or containing formulas and multiple
styles, may not be identified. Unidentified continuations can be translated
page by page without a continuation-specific warning. Check both pages and
their numbers and formatting manually; a missing warning does not establish
that the sentence was translated as a whole.

Run the focused regression with:

```text
cargo test --manifest-path rust/Cargo.toml -p babeldoc-p2-stages --test formula_order --test paragraph_spacing --locked
cargo test --manifest-path rust/Cargo.toml -p babeldoc-p2-stages --test cross_page --test cross_page_context --test cross_page_whole_sentence --locked
cargo test --manifest-path rust/Cargo.toml -p babeldoc-p2-bridge --test cross_page_transaction --locked
```

The test fixtures are synthetic.
The v0.7.1 source tag references test modules that are absent from that tag, so
the upstream `-p babeldoc-p2-stages --lib` test suite cannot compile unchanged.

## Provenance

The modified formula merge condition was selected from the local experimental
reference-integrity work based on the same upstream tag. The larger experiment
and its original history are not published here. The code remains under the
upstream AGPL-3.0 license.

Local Windows x64 builds use CPython 3.12.12, Rust/Cargo 1.99.1,
maturin 1.15.0, MSVC 19.44.35229 and the locked Cargo dependencies. A different
compiler or build may produce different bytes. Import and dependency checks are
not translation or layout acceptance tests.
