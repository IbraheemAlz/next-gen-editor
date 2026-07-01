---
name: ci-gate
description: Run every Phase 1 CI gate in order; fail fast. Use before commit, after any cross-crate change, or to verify the working tree.
allowed-tools: Bash
user-invocable: true
---

Run the Phase 1 CI gate sequence. Stop on first failure. Assume cwd is the
project root.

```bash
set -eo pipefail   # pipefail is load-bearing: every gate pipes into `tail`,
                   # and without it a failing gate exits 0 (tail's status).

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
# wasm-pack's cached chromedriver drifts out of sync with system Chrome
# (symptom: "Error: http status: 404" right after ChromeDriver starts) and
# it IGNORES a CHROMEDRIVER env var. Pick a major-version match from the
# cache explicitly; fall back to wasm-pack's own download when none matches.
CHROME_MAJOR=$(google-chrome --version | grep -oE '[0-9]+' | head -1)
DRIVER=""
for d in "$HOME"/.cache/.wasm-pack/chromedriver-*/chromedriver; do
    [ -x "$d" ] || continue
    V=$("$d" --version | grep -oE '[0-9]+' | head -1)
    if [ "$V" = "$CHROME_MAJOR" ]; then DRIVER="$d"; break; fi
done
if [ -n "$DRIVER" ]; then
    wasm-pack test --headless --chrome --chromedriver "$DRIVER" crates/engine-wasm 2>&1 | tail -5
else
    wasm-pack test --headless --chrome crates/engine-wasm 2>&1 | tail -5
fi

echo "=== 7. shape-regression ==="
cargo run -p shape-regression --release 2>&1 | tail -10

echo "=== 8. roundtrip ==="
cargo run -p roundtrip --release 2>&1 | tail -5

echo "=== 9. visual diffs ==="
# Vite must already be running on :5173 for this step.
# Use /start-dev first if not.
if curl -sI http://localhost:5173/ 2>/dev/null | head -1 | grep -q "200"; then
    # Farm mode runs EVERY committed golden (11 cases as of beta.3) at the
    # tier-A bar (<= 0.5% per case, 100% pass) — stricter and more complete
    # than the old hand-listed 6-case loop.
    (cd tools/visual-diff && node run.mjs --tier A 2>&1 | tail -14)
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
- Visual-diff failures while a heavy build runs concurrently are suspect:
  headless Chrome can composite a stale frame from the previous page under
  CPU pressure (a passing case's pixels bleed into the next capture).
  Re-run the failing case on a quiet machine before believing the diff.
- Gate 6 "http status: 404": chromedriver/Chrome major-version mismatch —
  the matching logic above should prevent it; if no cached driver matches,
  download one for your Chrome major from
  https://googlechromelabs.github.io/chrome-for-testing/.
