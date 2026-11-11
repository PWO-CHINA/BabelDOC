# Offline cross-page sample validation

`crosspage_offline_validation.py` reruns selected, locally held two-page PDF
inputs with a synthetic translator. It reads input names from historical
`summary.json` records and checks a caller-supplied SHA-256 before opening each
input. Optional historical tracking and prompt snapshots derive target anchors
in memory. A selected block is not, by itself, a verified target.

Keep the sample manifest, historical records, PDFs, and resulting metadata
outside the repository. The manifest uses this schema; all values below are
illustrative and do not identify a real input:

```json
{
  "version": 1,
  "samples": {
    "sample-a": {
      "run": "synthetic-run",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
      "pages": [1, 2],
      "blocks": null,
      "target": false
    }
  }
}
```

`run` names a directory directly under the suite root containing `summary.json`.
`pages` records the input's original one-based page numbers; `blocks` is either
null or two zero-based text-block indexes. Set `target` only when the historical
tracking and private prompt snapshot establish target anchors. Replace the
illustrative hash with the input's actual SHA-256 locally. Do not commit the
filled manifest or disclose sample identifiers and hashes in public logs.

Use a local Python environment containing the dependencies of the fixed BabelDOC
package. Set `PYTHONPATH` so that the fixed package and its dependencies are
imported before any other BabelDOC installation. From a directory outside the
source checkout, run for example:

```powershell
& $python scripts/crosspage_offline_validation.py `
  --manifest $localManifest --suite-root $historicalSuite --downloads $selectedInputs `
  --package-root $installedPackage --native-sha256 $nativeHash `
  --package-code-sha256 $packageCodeHash `
  --sample sample-a
```

Use an absolute path to the script if the current directory is outside the
checkout. Optionally provide `--source-checkout` and `--source-commit` together;
the checkout must be clean and at that full commit. The runner hashes the
installed Python/native files separately before and after each sample. A clean
checkout and matching native hash do not by themselves prove how the native was
built; retain the build provenance separately. `--package-code-sha256` enforces
the expected digest of installed Python and native code before processing; if
omitted, the runner records the digest but does not compare it to a supplied value.

The default translator returns each input unchanged. `--translator-mode rot13`
applies a reversible ASCII-letter substitution to ordinary text while retaining
tags, brace-delimited placeholders, digits, punctuation, scientific-notation
exponent markers, and JSON IDs. This is an offline control, not translation.
In this mode output anchors are searched
after reversing the substitution in extracted PDF text. A missing anchor still
does not establish lost text or incorrect page assignment. Dedicated first and
retry request counts use the fixed retry instruction, and target response-item
counts compare inputs and outputs only in memory.

The report records whether the native supports stage capture. With a native
built with the `stage-capture` feature, `--capture-stages` additionally records
per-page anchor counts from translation and typesetting paragraph text and
composition characters. These are short-anchor diagnostics, not proof that
complete translated text is visible or on the correct page. They do not search
across paragraph boundaries. A requested capture fails if the native lacks the
feature or either stage is missing. Captures remain in the same temporary
directory as the PDFs and are deleted on normal exit. If a separate diagnostic
native is used, report its source, build provenance and hash independently.
The styles capture also reports matched boundary composition, font, graphic
state, character-transform, and sentence-boundary classifications without
recording the contents. The sentence-boundary classifier mirrors selected
source rules and is diagnostic, not the native decision or proof of semantics.
The context-sentence source-rule replay likewise classifies captured paragraph
text in memory; it does not observe whether native pairing actually reached
that guard or expose private text.
Sentence-to-composition locations are reported only when captured character
streams and paragraph text match after removing whitespace; otherwise they are
unknown. Composition boundaries are not necessarily request placeholders.
Target tracking records also classify placeholder types and token order without
retaining the tokens. Tracking input can reflect ordinary or fallback processing
and does not prove which input a dedicated cross-page request would have used.

Output contains counts, hashes, selected page numbers, and extraction-based
geometry only. Full PDF text, prompts, responses and rendered outputs remain
in temporary local files and are removed when the process exits normally. Save
metadata output to a local file outside the repository. Python socket calls are
guarded against non-loopback connections; this Python-level guard does not
contain native code or subprocesses. The translator itself makes no network
requests. Run only in a trusted local environment with cached assets.

Interpret tracking events, dedicated requests, whole-input requests, fallback
calls, and PDF anchor locations independently. Event counts do not establish a
correct match. Fallback counts may include ordinary calls and calls caused by
other behavior; target fallback counts are based on short local anchors.
`synthetic_calls` counts the synthetic LLM interface, while
`synthetic_plain_calls` counts synthetic `do_translate` invocations separately;
neither count represents a provider request.
Missing text anchors may reflect extraction or style segmentation, so they are
an investigation signal rather than proof of lost text. Page-box checks do not
verify overlap, clipping, readability, image placement, or semantic quality.
All calls recorded by this runner are synthetic, not model or provider calls.
Failures record their exception type and frame locations without raw exception
messages. An exact allowlist of known static numeric-continuation errors is
classified separately; all other messages are reported as unclassified.
For a matched target tracking pair, the report also counts fallback tracker
categories without retaining their messages. These categories may combine
several validation stages and do not establish a more specific cause.

The native-facing synthetic tests are in
`rust/p2_bridge/tests/test_cross_page_sample_validation.py`,
`rust/p2_bridge/tests/test_crosspage_formula_boundary_validation.py`, and
`rust/p2_bridge/tests/test_crosspage_signed_number_validation.py`. The separate
`rust/p2_bridge/tests/test_crosspage_numbered_steps_validation.py` checks that
numbered steps do not join the preceding page and that a preceding decimal is
not merged into the next step. The formula
tests require an isolated continuation after a preceding formula and reject
formula overlap and a continuation split across styles. The numeric tests use
synthetic continuations on both pages to check intact expressions, rejection
of changed signs or a removed percent sign, recovery after a corrected retry,
and transactional rollback. They include explicit positive and Unicode minus
signs, a negative decimal, percentages, and scientific notation. Run them
against the same fixed installed native and record its hash first. The tests
are strict: known older binaries may fail, and such failures must not be
converted into passing results by skipping or marking them expected.
