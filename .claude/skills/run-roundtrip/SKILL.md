---
name: run-roundtrip
description: One-shot .docx round-trip harness. Builds a fixture, edits, saves, asserts sibling-byte-identity + bounded document.xml diff.
allowed-tools: Bash
user-invocable: true
---

Run the native round-trip harness. No vite or chrome required.

```bash
set -e
export PATH="$HOME/.cargo/bin:$PATH"

cargo run -p roundtrip --release 2>&1 | tail -20
```

Expected output (PASS path):

```
[roundtrip] fixture .docx: 1241 bytes
[roundtrip] step 2 OK — fixture parses back to seed
[roundtrip] step 3 OK — in-memory edit reflected
[roundtrip] saved edited .docx: 1254 bytes
[roundtrip] step 5 OK — saved .docx parses back to edited tree
  [SAME] [Content_Types].xml (434 B)
  [SAME] _rels/.rels (300 B)
  [SAME] word/_rels/document.xml.rels (157 B)
[roundtrip] step 6a OK — all sibling entries byte-identical
[roundtrip] document.xml: 292 -> 312 bytes (Δ 20 B, insert text 20 B)
[roundtrip] step 6b OK — document.xml diff within bound
PASS
```

Failure modes:
- `[DRIFT]` on any sibling entry → writer regression; sibling preservation broken.
- `document.xml diff > bound` → writer is rewriting unrelated regions; check XML escape handling and whitespace.
- Build failure → run `/ci-gate` to see the underlying cargo error.
