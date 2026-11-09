# Experimental PWO patch

This fork is based on upstream BabelDOC commit
`dd9adc352b957533d05b8cb94e88a7589cc56a35`, the PWO fork's main branch at the
time of development. It identifies the patched package as `0.7.1+pwo.2` and
retains the upstream AGPL-3.0 license.

The patch retains the `0.7.1+pwo.1` formula-order correction: formula groups
separated by other paragraph content are not merged across that content. It
also gives a synthesized separator at a detected line break zero width. This
prevents a negative horizontal jump from extending a paragraph far beyond its
source bounds while retaining the separator in extracted text. Ordinary word
gaps retain their previous behavior.

The following focused Rust tests use synthetic data only:

```text
cargo test --manifest-path rust/Cargo.toml -p babeldoc-p2-stages --test formula_order --test paragraph_spacing --locked
```

These changes do not ensure translation quality, preserve PDF link annotations,
or restore text omitted because of unsupported rotation. The paragraph
separator fix addresses one observed overflow cause, not every possible layout
problem. Manual review of translations, formulas, references, and layout remains
necessary. The upstream `-p babeldoc-p2-stages --lib` tests refer to test modules
absent from this upstream revision.
