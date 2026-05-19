---
name: wasm-size
description: Build the wasm artifact and report its size against the 15 MiB Phase 1 budget.
allowed-tools: Bash
user-invocable: true
---

Build the release wasm artifact and report size. Use this when a heavy crate
gets added to verify the budget hasn't been blown.

```bash
set -e
export PATH="/home/linuxbrew/.linuxbrew/bin:$HOME/.cargo/bin:$PATH"

wasm-pack build --target web --release crates/engine-wasm 2>&1 | grep -vE "rust-lld: warning|Compiling|Checking" | tail -4

SIZE=$(wc -c < crates/engine-wasm/pkg/engine_wasm_bg.wasm)
BUDGET=15728640
PCT=$(awk "BEGIN { printf \"%.1f\", ($SIZE / $BUDGET) * 100 }")
KIB=$((SIZE / 1024))

echo ""
echo "  WASM artifact: $SIZE bytes ($KIB KiB)"
echo "  Budget:        $BUDGET bytes (15 MiB)"
echo "  Used:          $PCT %"

if [ "$SIZE" -lt "$BUDGET" ]; then
    echo "  Status:        OK (under budget)"
else
    echo "  Status:        FAIL (over budget)"
    exit 1
fi
```

Phase 1 trajectory (informational):

| Milestone | KiB | % of budget |
| --- | --- | --- |
| Week 4 — bridge only | 93 | 0.6 |
| Week 6 — + swash + read-fonts | 687 | 4.5 |
| Week 9 — + rustybuzz + ttf-parser | 1185 | 7.7 |
| Week 14 — + icu_segmenter + unicode-bidi | 1607 | 10.5 |
| Week 24 — + engine + zip + xml | 1862 | 12.1 |
