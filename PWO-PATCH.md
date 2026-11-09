# PWO formula order patch

This experimental fork is based on BabelDOC v0.7.1, commit
`d922e2b0c5a353bde8731921eb442c07ae75849d`. It retains upstream's AGPL-3.0
license and is identified as `0.7.1+pwo.1`.

The change prevents formula groups separated by other paragraph content from
being merged across that content. This avoids changing character order in the
synthetic identifier exercised by the regression test. Adjacent groups retain
their existing merge behavior. It does not protect bibliography metadata,
preserve PDF links or guarantee translation or layout accuracy. Reference
formats, titles, authors, dates, identifiers and links still require human
review. The more extensive local bibliography and link experiments are not
included in this patch because their coverage and safety are not established.

Run the focused regression with:

```text
cargo test --manifest-path rust/Cargo.toml -p babeldoc-p2-stages --test formula_order --locked
```

The sample used in that test is synthetic and does not contain document text.
The v0.7.1 source tag references test modules that are absent from that tag, so
the upstream `-p babeldoc-p2-stages --lib` test suite cannot compile unchanged.

## Provenance

The modified formula merge condition was selected from the local experimental
reference-integrity work based on the same upstream tag. The larger experiment
and its original history are not published here. The code remains under the
upstream AGPL-3.0 license.

The Windows x64 wheel is built with CPython 3.12.12, Rust/Cargo 1.99.1,
maturin 1.15.0, MSVC 19.44.35229 and the locked Cargo dependencies. Its release
notes identify the reviewed artifact and its hash. A different compiler or
build may produce different bytes. Import and dependency checks are not
translation or layout acceptance tests.
