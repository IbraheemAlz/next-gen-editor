# Clean-room reference protocol

**Why this exists.** This project is licensed `MIT OR Apache-2.0`. We study
copyleft competitors — OnlyOffice (AGPL-3.0) and LibreOffice (MPL-2.0) — to
learn *methods*: how mature word processors solve layout, pagination, and
OOXML interop problems. Copyright protects **expression**, not ideas; studying
is legal and explicitly invited by open source. But a rewrite that carries
over expression (even translated to another language) can be a derivative
work, which would contaminate our permissive license. This protocol is the
evidentiary wall that keeps learned *ideas* separable from copied
*expression*. It is not optional and it is not a formality.

## Roles

- **Reader** — an isolated subagent session. Readers may open and study the
  reference source trees. A Reader's *only* output is a sanitized methods
  document (see "What may cross the wall"). Readers never write product code.
- **Implementer** — the main session and any agent that writes code for this
  repo. Implementers **never open any file under `/data/code/reference/`**.
  Implementers work only from: sanitized Reader documents, the ECMA-376 spec
  corpus, Unicode UAX reports, academic literature, permissively-licensed
  references, and black-box observation of competitor behavior.

The same conversation context must never hold both roles. In practice: the
main session orchestrates and implements; dedicated subagents read.

## Reference source trees

Located **outside** the repo at `/data/code/reference/`:

| Directory | Project | License | Purpose |
|---|---|---|---|
| `libreoffice-core/` | LibreOffice | MPL-2.0 | Writer (`sw/`), edit engine, VCL text layout, `writerfilter/` OOXML import |
| `onlyoffice-sdkjs/` | ONLYOFFICE | AGPL-3.0 | JS editor engine — word-processor runtime layout/editing (`word/`, `common/`) |
| `onlyoffice-core/` | ONLYOFFICE | AGPL-3.0 | C++ core — format conversion (`x2t`), OOXML read/write libraries |

Rules for the trees themselves:

- Never copy, move, or symlink any file from `/data/code/reference/` into
  this repository or its build inputs.
- Never commit reference content anywhere. The trees stay outside the repo.
- Record provenance (remote URL, commit hash, study date) in the per-project
  YAML index whenever a Reader studies a tree.

## What may cross the wall (Reader output)

- Feature inventories: what the product does, user-visible behavior.
- Concepts and methods described as **prose or mathematics**: "layout is
  two-pass: measure, then flow", invariants, data-flow descriptions,
  complexity trade-offs.
- Architecture organization at subsystem level; repo-relative directory/file
  **paths as pointers** for future Reader dives.
- Observed behavior, bug-tracker facts, public documentation content.
- Names of user-visible features and public file-format elements
  (`<w:sectPr>`, "AutoFit", …).

## What must never cross the wall

- Source code, in any quantity, including "just one small function".
- Verbatim or paraphrased-line-by-line comments.
- Internal identifiers: class, function, variable names. (Public/UI-visible
  feature names and OOXML element names are fine — those are facts.)
- Constant tables and magic numbers (a tuned kerning table, a heuristic
  threshold list). If a Reader reports "they special-case X", it reports
  *that* the case exists, not the tuned values.
- Function-level decomposition or the **ordering of special-case handling** —
  the sequence of edge cases is their most original expression.

## Review gate

Every Reader document is reviewed before use: scan for code fences, camelCase
/ snake_case identifier clusters, and numeric tables. Anything that smells
like expression is stripped and the document is regenerated if needed.
Reader documents live in `plans/cleanroom/` and are versioned — they are the
retained evidence of exactly what crossed the wall, which is the point.

## Source-preference order

Copyleft study is the *last* resort, not the first. When designing a feature,
consult in this order:

1. ECMA-376 spec corpus + MS-OI29500 implementation notes.
2. Unicode UAX reports (#9 bidi, #14 line breaking, #29 segmentation), W3C
   CSS specs, academic literature (e.g. Knuth–Plass).
3. Permissively-licensed implementations (rustybuzz/HarfBuzz model,
   python-docx, docx-rs).
4. Black-box observation: author fixtures in Word 365 / LibreOffice, diff
   the outputs, compare renders.
5. Only if a genuine "how did anyone make this work?" question remains:
   dispatch a Reader into `/data/code/reference/`.

## Standing workflow for feature development

When developing, improving, or revising a feature:

1. Implementer states the design question ("how do mature editors handle
   footnote space negotiation during pagination?").
2. A Reader subagent studies the relevant reference areas (the YAML indexes
   in `plans/cleanroom/` point at them) and writes/updates a sanitized
   methods memo in `plans/cleanroom/memos/`.
3. The memo passes the review gate.
4. Implementer designs and writes the code **in our architecture and style**
   from the memo + spec — never from the source.
