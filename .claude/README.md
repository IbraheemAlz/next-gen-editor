# `.claude/` — project-local agent config

This directory configures Claude Code for this repo. Booting into a new
session? Skim:

1. [`../CLAUDE.md`](../CLAUDE.md) — engineering DNA + architectural invariants. **Read first.**
2. [`settings.json`](./settings.json) — pre-approved Bash patterns + denied destructive ops + env vars.
3. [`rules/`](./rules) — path-scoped instructions (load only when touching matching files).
4. [`skills/`](./skills) — slash commands for repetitive CI workflows.
5. [`agents/`](./agents) — specialized sub-agents (currently: `phase-executor`).

## What lives where

| File | When loaded | Purpose |
| --- | --- | --- |
| `../CLAUDE.md` | every session | Top-level invariants the agent must always know |
| `rules/rust.md` | when touching `**/*.rs` / `Cargo.toml` / `.cargo/**` | Rust 1.95, LTO, wasm-ld flags |
| `rules/typescript.md` | when touching `ts/**` / `packages/**` / `tools/**/*.mjs` | tsify-next, dispatch channel, Solid.js, pnpm workspace |
| `rules/sdk-architecture.md` | when touching `packages/**` / `ts/src/sdk-bridge.tsx` | **Monaco Standard** — pnpm workspace, `@nge/core` + `@nge/ui`, Solid.js, `.nge-*` CSS, Honest UX |
| `rules/docx.md` | when touching `crates/format-docx/**` / `tools/roundtrip/**` | `.docx` round-trip invariants |
| `rules/visual-diff.md` | when touching `tools/visual-diff/**` / `ts/**` | Playwright + golden management |
| `skills/ci-gate` | `/ci-gate` | Run all gates in order, fail fast |
| `skills/update-goldens` | `/update-goldens` | Regenerate visual goldens (with eyeball reminder) |
| `skills/wasm-size` | `/wasm-size` | Current vs 15 MiB budget |
| `skills/start-dev` | `/start-dev` | Vite dev server in background |
| `skills/run-roundtrip` | `/run-roundtrip` | `.docx` round-trip harness one-shot |
| `skills/gh-issue-logger` | `/gh-issue-logger` | **MANDATORY** when a turn discovers a missing core feature, ships a pragmatic UI workaround, or surfaces tech debt outside sprint scope. File issues before concluding the turn. |
| `agents/phase-executor` | when delegating multi-week phase work | Isolated context for Phase N execution |

## Settings rationale

`settings.json` is **committed**. It pre-approves the Bash patterns the
project legitimately needs (cargo, wasm-pack, pnpm, git read commands,
visual-diff helpers) so the agent isn't blocked on permission prompts in
the middle of CI gates. Destructive operations and credential reads are
denied.

Personal overrides (e.g. local model choice, sensitive env vars) go in
`.claude/settings.local.json` which is gitignored.

## Don't put here

- Phase plans (those live at repo root: `MASTER_PLAN.md`, `PHASE_*.md`).
- Generated artifacts (target/, pkg/, dist/ — all gitignored).
- Secrets — they should never enter the repo at all.
- Phase-specific runbooks — those belong in the phase plan docs.
