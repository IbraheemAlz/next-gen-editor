---
name: gh-issue-logger
description: File a well-formed GitHub issue for tech debt, missing core engine work, or pragmatic UI workarounds before concluding the current turn. Enforces title format, label discipline, and cross-references to related GitHub issues. GitHub Issues is this repo's single source of truth for backlog/roadmap tracking — do not reference or recreate local markdown backlog files.
allowed-tools: Bash
user-invocable: true
---

# gh-issue-logger

File a GitHub issue capturing a gap, workaround, or tech-debt item the
current turn discovered. Enforces the project's standing discipline:
**no Phantom UI, no silent TODOs**. Every disabled "Engine pending"
badge, every `Event::Error` stub, every documented limitation
in the codebase must have a matching tracked issue.

## When to invoke (mandatory triggers)

The agent MUST run this skill (autonomously, without being prompted)
when any of the following happens during a turn:

1. **Missing core engine feature.** Sprint X (UI Edition) needs
   `Command::X` but the bridge doesn't have it AND adding the engine
   half is out of scope for the current sprint. File the gap, ship
   the UI gated.
2. **Pragmatic workaround in UI.** Component dispatches into a
   stubbed handler that returns `Event::Error`, or fakes the result
   client-side (e.g. faux styles via preset patches). The user
   experience is "good enough for QA" but the real path is missing.
3. **Tech debt found.** Code already in the tree has a known
   limitation surfaced during this turn (e.g. a TS-side `Math.max(0,
   row - 1)` that masks a bridge underflow; a paginate.rs
   construction site that lacks a new field).
4. **Round-trip not preserved.** A new in-memory engine field works
   for the active session but the `.docx` writer drops it (e.g.
   `CommentDef.resolved` not in `commentsExtended.xml`).
5. **Read-back gap.** A dialog opens with default values because
   `SELECTION_CHANGED` does not emit the matching state.

If you are unsure whether a discovery qualifies, file the issue. A
duplicate issue is cheaper than a lost one.

## Issue format (enforced)

Title (max ~80 chars, prefixed by area):

- `Core: <what's missing>` — engine / Rust crate gap.
- `UI: <what's missing>` — frontend gap only.
- `UI/Core: <what>` — cross-cutting (both halves needed).

Body sections, in order:

```
## Context

<2-4 sentences>. What sprint / component surfaced this. What the
current behaviour is (visibly: disabled badge / silent error /
in-memory-only). What the architectural gap is.

## Scope

<Numbered list of concrete changes>. For Rust: name the crates,
the structs/methods, the dispatch sites. For UI: name the
components, the props that flip, the lines that drop.

If new types are required, include the Rust shape inline:

```rust
pub struct X { ... }
```

## Acceptance

<Bulleted list of testable outcomes>. Always include:
- `cargo test --workspace --lib` clean
- `pnpm -r tsc` clean
- Specific manual-QA path that proves the gap closed
- The 4-line UI follow-up that removes the "Engine pending" badge

## Out of scope

<Bulleted list>. Explicitly enumerate what is NOT this issue.
Prevents scope creep when someone picks it up.

cc #<related-issue-number> if this overlaps or supersedes existing tracked
   work — check `gh issue list --search "<keyword>"` first.
```

## Labels (required)

At least one of `core-engine` / `ui` (or both), plus a kind label
(`enhancement` / `tech-debt`).

Existing labels in this repo:

- `core-engine` — touches `crates/engine`, `crates/engine-wasm`, or
  other core Rust crates
- `ui` — touches `packages/ui` or downstream UI integration
- `enhancement` — new feature or capability
- `tech-debt` — accepted trade-off with documented scope; revisit when
  prioritized
- `bug` — known regression or incorrect behaviour
- `edge-case` — known limitation hit by rare/pathological input
- `documentation` — docs-only

If a needed label does not exist, create it with `gh label create`
first (`core-engine` and `ui` were created this way).

## Workflow

1. Verify the label exists (`gh label list`). Create if missing.
2. Compose the title + body in a heredoc.
3. Run `gh issue create --title "..." --body "..." --label "..."`.
4. Capture the returned URL in your end-of-turn summary so the user
   sees the link.

## Reference invocation

```bash
gh issue create \
  --title "Core: Implement <feature>" \
  --body "$(cat <<'EOF'
## Context

Sprint X (UI Edition) shipped <component> in @nge/ui. The control
dispatches Command::Y but the engine returns Event::Error today
because <root cause>. The button renders disabled + amber "Engine
pending" badge.

## Scope

1. **<Layer> — <change>.**
   - <bullet>
   - <bullet>

2. **<Layer> — <change>.**
   - <bullet>

## Acceptance

- Command::Y returns Event::Z with <payload>
- <Component> drops the disabled + badge (a ~N-line follow-up)
- cargo test --workspace --lib clean
- pnpm -r tsc clean
- Manual QA: <specific user flow>

## Out of scope

- <related feature> — separate enhancement
- <related feature> — Phase N work

cc #<related-issue-number> if applicable
EOF
)" \
  --label "core-engine,enhancement"
```

## Filed issues to date (running ledger)

When this skill runs successfully, append a line to the running list
in your end-of-turn summary. Existing issues:

- `#9` — Core: HTML and PlainText export serializers
- `#10` — UI/Core: Engine state read-back for Properties Dialogs
- `#11` — Core: Live Style Table and Cascade Re-application
- `#12` — Core: numbering.xml writer and List Synthesis
- `#13` — UI: Interactive Ruler for Tabs and Indentation
- `#14` — Core: Gate edits into `<w:ins>`/`<w:del>` when Track Changes is on
- `#15` — Core: Round-trip comment 'resolved' state through commentsExtended.xml
- `#16` — Core: ARIA live announcements from engine events
- `#17` — Core: UAX-#29 word segmentation for accurate word_count

The above are templates — each sprint's actual debt should produce a
new issue, not be force-fit into an existing one.

## Never

- Open a draft issue and walk away. Either file it cleanly or
  surface the gap to the user via `AskUserQuestion`.
- Skip the labels. Untriaged issues drown the backlog.
- Reference "TODO comments" or "see this commit" — issues must
  stand alone with the architecture + scope visible to a reader
  who has never touched the code.
- File a duplicate. Run `gh issue list --search "<keyword>"`
  first if you suspect overlap — GitHub Issues is this repo's single
  source of truth for status, not a local markdown file.
