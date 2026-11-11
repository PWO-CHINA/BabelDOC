# PWO cross-page experiment

This experimental fork is based on BabelDOC commit
`dd9adc352b957533d05b8cb94e88a7589cc56a35`. It retains upstream's AGPL-3.0
license and is identified as `0.7.1+pwo.2.crosspage.16`. This is an
experimental version and is not a released dependency.

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

The optional cross-page experiment identifies conservative candidates and
validates response structure. Eligible continuations can be translated as a
complete sentence and redistributed to the original pages. Identified numeric
continuations that cannot be handled as a whole fail with a review message.
The numeric check compares recognized digit and operator expressions exactly;
it is not a semantic check of units, written numbers, or other translations.
Numeric formatting changes or unrecognized notation can require manual review.
Decimal sentence boundaries are conservative: explicitly signed decimals can
be recognized, while ambiguous unsigned decimals may be skipped or blocked.
The option is disabled by default and does not ensure that every continuation
is detected, translated correctly, or laid out safely.

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
