# Third-party notices

This project is licensed under the MIT License or the Apache License,
Version 2.0, at your option (`LICENSE-MIT`, `LICENSE-APACHE`). It contains
source code modified from the third-party project below, used under that
project's own license. Each modified file also says so in its header.

## WordCraft

- Project: WordCraft, a clean-room Rust Word clone —
  <https://github.com/storytold/wordcraft>
- Commit the code was taken from: `3a4770ef8ce037d2650c93e2d5764536549357c7`
- License: MIT OR Apache-2.0 (the same dual license as this project)
- Copyright (c) 2026 ArtCraft Team and the WordCraft contributors

WordCraft's `NOTICE` file reads, for the code (its remaining entries concern
fonts, hyphenation patterns and brand assets, none of which is used here):

> WordCraft
> Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
> Licensed under the MIT License or the Apache License, Version 2.0, at your
> option (LICENSE-MIT, LICENSE-APACHE).

### Files modified from WordCraft

The legacy Word 97-2003 binary document importer (`crates/format-doc`,
issue #567) borrows the structure of WordCraft's `.doc` reader
(`crates/docbin`). Every borrowed file was re-expressed against this
project's own model (`engine::DocumentTree`, `SpanStyle`, `ParaProperties`,
`PageGeometry`), error type and bounds; the public specification ([MS-DOC],
Microsoft Open Specifications) is the reference each structure cites.

| File in this repository | Modified from (WordCraft) | Main changes |
|---|---|---|
| `crates/format-doc/src/fib.rs` | `crates/docbin/src/fib.rs` | Encryption checked on the cleartext `FibBase` first; only the pairs read are named; a short `RgFcLcb` blob is read as far as it goes. |
| `crates/format-doc/src/piece.rs` | `crates/docbin/src/piece.rs` | Pieces exposed whole and walked in CP order; true byte offsets; a non-ascending CP table is a typed error. |
| `crates/format-doc/src/fkp.rs` | `crates/docbin/src/fkp.rs` | A lookup returns its FC run (the reader caches one run's formatting) and a PAPX entry's `istd` (issue #575); spec `crun` caps; typed errors. |
| `crates/format-doc/src/sprm.rs` | `crates/docbin/src/sprm.rs` | Prm0 table limited to the applied sprms, `isprm` 0x05 mapped to `sprmPJc80`. |
| `crates/format-doc/src/props.rs` | `crates/docbin/src/fmt.rs`, `crates/docbin/src/sections.rs` | Onto this engine's model; RTL sprms and complex-script twins added; physical vs logical justification against the cascaded direction; toggle operands resolved against the style chain and fonts kept as font-table indices (issue #575); every measure clamped to Word's bounds. |
| `crates/format-doc/src/sections.rs` | `crates/docbin/src/sections.rs` | Section table only (no header / footer stories); typed errors. |
| `crates/format-doc/src/reader.rs` | `crates/docbin/src/lib.rs` (the main-document walk) | Piece-order walk; paragraph `Prm` applied; fields kept as their result; special characters mapped through `engine::text_input`; unmodelled content counted for the import report; character budget and paragraph cap; paragraphs formatted through the engine's one style cascade (issue #575). |

Not borrowed: WordCraft's `crypt.rs`, drawing, list, table, note, media,
metadata and style-sheet modules (this project's style sheet and font table
readers, `stsh.rs`, `styles.rs` and `ffn.rs`, issue #575, are written from
[MS-DOC]); its fonts, icons and other assets; and its
`cfb` crate dependency — the OLE compound-file container is read with this
project's own bounded reader (`crates/format-docx/src/opc/cfb.rs`), so the
importer adds no third-party crate.

### WordCraft's MIT license

```
MIT License

Copyright (c) 2026 ArtCraft Team and the WordCraft contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Under the Apache License, Version 2.0 option, the license text is the one in
this repository's `LICENSE-APACHE`; the modified files carry the "modified
from WordCraft" statement section 4(b) asks for.
