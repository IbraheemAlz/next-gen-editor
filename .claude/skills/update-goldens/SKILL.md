---
name: update-goldens
description: Regenerate visual-diff goldens. Reminds the operator to eyeball the new images before merging.
allowed-tools: Bash, Read
user-invocable: true
arguments:
  - case_name
---

Regenerate the golden(s) under `tools/visual-diff/golden/`. Vite must be
running on :5173 (`/start-dev`).

**You MUST eyeball the regenerated golden before committing it.** The
harness only confirms run-to-run reproducibility; it can't tell you the new
pixels are correct.

```bash
set -e

cd tools/visual-diff

if [ -z "$1" ]; then
    # No case name → refresh ALL goldens.
    for case in glyph-a hello-latin hello-arabic a4-justified-mixed editing-arabic docx-round-trip; do
        URL="http://localhost:5173/?test=$case" UPDATE=1 node run.mjs "$case" 0.02
        echo "  → tools/visual-diff/golden/$case.png"
    done
else
    URL="http://localhost:5173/?test=$1" UPDATE=1 node run.mjs "$1" 0.02
    echo "  → tools/visual-diff/golden/$1.png"
fi

echo ""
echo "REVIEW STEP (do NOT skip):"
echo "  1. Open the new golden(s) in an image viewer."
echo "  2. Diff against the previous version via `git diff -- tools/visual-diff/golden/`."
echo "  3. Confirm every pixel change is intentional."
echo "  4. THEN stage + commit."
```
