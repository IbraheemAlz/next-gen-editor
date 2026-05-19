---
name: ci-gate
description: Run every Phase 1 CI gate in order; fail fast. Use before commit, after any cross-crate change, or to verify the working tree.
allowed-tools: Bash
user-invocable: true
---

Run the Phase 1 CI gate sequence. Stop on first failure. Assume cwd is the
project root.

```bash
set -e

export PATH="/home/linuxbrew/.linuxbrew/bin:$HOME/.cargo/bin:$PATH"

echo "=== 1. fmt ==="
cargo fmt --all -- --check

echo "=== 2. clippy ==="
cargo clippy --workspace --all-targets -- -D warnings

echo "=== 3. native tests ==="
cargo test --workspace --lib

echo "=== 4. wasm build ==="
wasm-pack build --target web --release crates/engine-wasm 2>&1 | tail -5

echo "=== 5. wasm size budget ==="
SIZE=$(wc -c < crates/engine-wasm/pkg/engine_wasm_bg.wasm)
echo "WASM: $SIZE bytes ($((SIZE / 1024)) KiB) / 15728640 budget"
test "$SIZE" -lt 15728640

echo "=== 6. wasm-pack test ==="
wasm-pack test --headless --chrome crates/engine-wasm 2>&1 | tail -5

echo "=== 7. shape-regression ==="
cargo run -p shape-regression --release 2>&1 | tail -10

echo "=== 8. roundtrip ==="
cargo run -p roundtrip --release 2>&1 | tail -5

echo "=== 9. visual diffs ==="
# Vite must already be running on :5173 for this step.
# Use /start-dev first if not.
if curl -sI http://localhost:5173/ 2>/dev/null | head -1 | grep -q "200"; then
    cd tools/visual-diff
    for case in glyph-a hello-latin hello-arabic a4-justified-mixed editing-arabic docx-round-trip; do
        URL="http://localhost:5173/?test=$case" node run.mjs "$case" 0.02 2>&1 | tail -2
    done
    cd ../..
else
    echo "SKIP visual diffs — vite not running on :5173 (use /start-dev)"
fi

echo "=== ALL GATES PASSED ==="
```

If a step fails, **do not** proceed to commit. Investigate, fix, re-run.

Common fixes:
- `cargo fmt --all` to auto-format.
- Read clippy output for the lint name; fix the code, don't `#[allow]`.
- Visual-diff drift: review the diff PNG at `/tmp/visual-diff/<case>.diff.png`; if intentional, `/update-goldens <case>`.
