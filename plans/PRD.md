# NGE — Product Requirements Document

**Status:** Living reference. Created 2026-08-22 from the clean-room
competitive study (`plans/cleanroom/PRD_ONLYOFFICE.md`,
`plans/cleanroom/PRD_LIBREOFFICE.md`) crossed with our shipped state
(`v0.6.0-beta.2`). This document defines positioning, the tiered coverage
policy, and the target feature matrix. **It is not a backlog** — every
actionable gap lives in GitHub Issues (the single source of truth); this doc
cites issue numbers where they exist and gaps discovered here are filed, not
listed here as TODOs.

---

## 1. Positioning

**The Arabic-first, fidelity-first, embeddable word processor SDK.**

Four claims, each verified against the two reference products:

1. **Native RTL/Arabic typography.** ONLYOFFICE ships *no kashida
   justification* (justified Arabic stretches spaces only) and *no RTL
   tables*; its BiDi is a pragmatic subset of UAX #9. LibreOffice is strong
   here but is a desktop codebase, not an embeddable web SDK. We do full
   per-line UAX #9, priority-band kashida with real tatweel glyphs, RTL tab
   anchoring, and RTL-aware lists. **This is the moat; every sprint must
   leave it stronger.**
2. **Byte-preserving `.docx` fidelity.** Both competitors regenerate:
   ONLYOFFICE fully re-serializes through an internal binary (silent loss
   for unmodeled content); LibreOffice maps semantically + "grab bags" but
   rebuilds the XML. Our sibling-byte-identity + unmutated-paragraph
   passthrough is strictly stronger for untouched content. No feature may
   regress it.
3. **Permissive-license embeddable SDK.** Both competitors are copyleft
   (AGPL / MPL); neither can be embedded the way `@nge/core` + `@nge/ui`
   (MIT/Apache) can. The Monaco-style Locked Surface is a product feature,
   not just architecture.
4. **Deterministic, safe core.** Rust→WASM in an isolated worker: no
   memory-corruption bug class, crash = worker respawn + replay (not tab
   death), identical metrics everywhere. ONLYOFFICE validates the
   determinism thesis (their WASM font engine exists for exactly this
   reason); we get it with a safe language.

**Anti-positioning.** We are not "OnlyOffice but newer": no suite (sheets/
slides), no server product, no plugin marketplace. We are the component a
product team embeds when they need Word-fidelity paged editing — especially
for Arabic-script markets — inside their own application.

## 2. Tiered coverage policy (normative)

"Supporting OOXML" is not binary. Every WML feature has an explicit tier;
moving a feature up a tier is a roadmap decision tracked in Issues.

- **Tier 1 — Edit.** Read, model, render, edit, write. Full round-trip
  invariants + goldens + Honest-UX-complete UI.
- **Tier 2 — Render.** Read, model, render faithfully; editing may be
  gated ("Engine pending" badge). Writer emits what the reader ingested.
- **Tier 3 — Preserve.** Not modeled. Content survives byte-verbatim via
  passthrough (`other_entries` / `source_xml`); if it occupies layout
  space, an honest placeholder renders. **Nothing is ever below Tier 3** —
  data loss is the only forbidden state.

Both reference products confirm this is how mature editors actually work;
neither is anywhere near schema-complete (see `OOXML_ROADMAP.md` §0).

## 3. Feature matrix

Us = shipped state at `v0.6.0-beta.2` (✅ solid · 🟡 partial · ❌ absent).
OO/LO = competitor maturity from the clean-room PRDs. Target = our tier
goal for the MVP-and-beyond arc (T1/T2/T3 per §2).

| Category | Us | OO | LO | Target | Notes / issues |
|---|---|---|---|---|---|
| Run formatting (faces, decorations, color, caps, sub/super) | ✅ | full | full | T1 | Underline variants, faux faces on both renderers shipped |
| Paragraph formatting (align, indent, spacing, tabs, borders) | ✅ | full | full | T1 | `page_break_before` no-op bug #75 |
| Styles & cascade | ✅ | full | full | T1 | Live style table, ModifyStyle shipped (#11 #21 #29) |
| Lists & numbering | 🟡 | full | full | T1 | RTL outline-marker bug #68; numbering writer shipped (#12) |
| Tables | 🟡 | full | full | T1 | AutoFit debt #62, AutoFit UI #46, story-Tab bug #76; **RTL tables: OO absent — differentiation chance** |
| Sections & page layout | ✅ | full | full | T1 | Even/Odd, continuous balancing shipped (#74 #8) |
| Headers & footers | ✅ | full | full | T1 | Story engine + Link-to-Previous shipped (#70–#74); HF images #78 |
| Fields & TOC | 🟡 | full | full | T1 (core set) | PAGE/NUMPAGES/DATE shipped (#43); authoring v2 #77; TOC not started |
| Footnotes & endnotes | ❌ | full | full | T2→T1 | Hardest pagination feature ahead; both refs document the negotiation protocol |
| Comments | ✅ | full | full | T1 | Threaded + resolved + OPC round-trip shipped (#27 #15 #18) |
| Track changes | 🟡 | full | full | T1 | Capture + accept/reject shipped (#14); display-for-review modes #47/#67 |
| Images & wrap | 🟡 | full | full | T1 | Inline + resize shipped (#44); floating anchors #69; wrap absent |
| Text frames / text boxes | ❌ | full | full | T2 | After floating anchors |
| Charts | ❌ | full | full | T3 | Future cargo-feature module; preserve verbatim today |
| Math (OMML) | ❌ | full | full | T3 | Future cargo-feature module; preserve verbatim today |
| Content controls / forms | ❌ | full (flagship) | full | T3→T2 | Their differentiator, not ours; preserve now |
| Mail merge | ❌ | full | full | T3 | Out of MVP scope |
| RTL / CTL | ✅ | partial (no kashida, no RTL tables) | full | **T1++** | The moat; goldens #24, marker bug #68 |
| Clipboard | ✅ | full | full | T1 | Rich + `<w:rPr>` round-trip; focus-lapse cache #57 |
| Find & replace | 🟡 | full | full | T1 | Core exists; band-story traversal fixed (#73) |
| Spellcheck | ❌ | full | full | T3 | Post-MVP; OO's WASM-worker pattern is the precedent |
| Collaboration (real-time) | ❌ | full | partial | Deferred decision | See §5 — do not promise before costing |
| Protection / permissions | ❌ | full | full | T3 | Preserve settings verbatim |
| Compare / combine | ❌ | full | full | T3 | |
| Accessibility | ✅ | **partial (weak)** | full | T1 | DOM overlay + fine deltas already beats OO; stable para ids open |
| Print / PDF | ✅ | full | full | T1 | PDF/A-1b/A-2u/X-3 shipped (#28); font subsetting open |
| Autosave / recovery | 🟡 | full | full | T1 | Event log ships; `Recover` still stub, renderer downgrade #66 |
| Master documents | ❌ | absent | full | Out of scope | |
| Plugins / macros | ❌ | full | full | Out of scope below bridge | SDK surface is our extensibility story |

## 4. Stability program (the "OnlyOffice reliability" answer)

Their stability is fifteen years of field exposure; ours must be bought with
machine time plus safety properties. Committed tracks:

1. **Real-document corpus harness.** Wild `.docx` corpus (public sets +
   Word-365-authored fixtures) run through open → layout → render → save →
   reopen nightly: no panic, sibling byte-identity, bounded drift, no text
   loss.
2. **Differential oracles.** LibreOffice headless and Word as black-box
   baselines (page counts, breaks, renders). Legally clean competitor-
   maturity mining.
3. **Continuous fuzzing.** Scale D5.5 beyond compile-check; structure-aware
   docx + command-stream fuzzing.
4. **Real crash recovery.** `Engine::snapshot()` + real `Command::Recover`
   + #66. Adopt the reference lesson both products teach: recovery = base
   snapshot + change replay, continuously persisted — our event log is
   already shaped for this.
5. **Layout self-defense.** Both references converge on the same doctrine:
   *interruptible everything, watchdogged convergence, degraded layout
   beats a hang, optimistic fast paths verified not trusted.* Adopt as
   layout-crate invariants as incremental relayout deepens.
6. **Real telemetry transport** for D5.7 so field failures come home.

## 5. Decisions this study forces

- **Footnotes/endnotes** are the biggest absent Tier-1-track feature; both
  references document compatible space-negotiation protocols (reserve
  page-bottom space per referencing line; split with continuation notices;
  never orphan the reference from its note). Design via the standing
  Reader-memo workflow when scheduled.
- **Collaboration** is a fork in the road: OO's object-id-addressed change
  stream (also their undo and their dirty tracker — one representation,
  three consumers) is the proven web-native design, but it presumes
  invertible change objects, which our snapshot-based undo does not
  provide. Do not promise collab before explicitly costing that
  model shift. Until then, the event log remains recovery-only.
- **Heavyweight subsystems** (charts, OMML, SmartArt) stay Tier 3 behind
  the 15 MiB budget; when scheduled, they land as cargo-feature modules.
- **RTL tables** (`bidiVisual`): ONLYOFFICE lacks them entirely; shipping
  them extends the moat. File and schedule as a core feature.
- **In-part grab bags** (LO's device): for attributes *inside*
  `document.xml` runs/paragraphs we re-serialize but do not model, stash
  and re-emit opaquely — complements sibling byte-preservation and hardens
  the ≤2× drift bound as coverage grows.

## 6. Standing workflow

Feature development follows `plans/cleanroom/PROTOCOL.md`: design question →
Reader memo (targeted via `plans/cleanroom/{onlyoffice,libreoffice}.yaml`) →
leakage review → implement from memo + ECMA spec in our architecture, spec
first, copyleft study last. Gaps discovered while working are filed via
`/gh-issue-logger` immediately.
