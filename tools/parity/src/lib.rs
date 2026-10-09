//! Issue #342 — the Command parity matrix.
//!
//! One row per `bridge::Command` variant, joining:
//!
//! - **meta** — `bridge::meta` (`CommandMeta`: status, logged, read-only,
//!   story policy), the single source of truth;
//! - **facade** — `packages/core/src/facadeMap.ts` (`COMMAND_FACADE` +
//!   `FACADE_METHOD_COMMANDS`, tsc-checked against the generated meta);
//! - **ui** — `data-nge-command` / `data-nge-pending-issue` attributes on
//!   the shell's JSX elements (`packages/ui/src`, `packages/core/src`,
//!   `ts/src`), plus every literal "Engine pending" badge;
//! - **e2e** — wire names the Playwright suite (`ts/e2e`) dispatches;
//! - **fuzz** — whether `fuzz/src/command_gen.rs` has a curated generator
//!   arm (every variant is reachable blind through `Arbitrary`).
//!
//! Rows measure FUNCTION, not registration: a `Stub` never counts as live,
//! however many facade members, buttons or tests mention it.
//! [`Matrix::violations`] is the floor (blocking in CI through
//! `cargo test --workspace` and `cargo run -p parity`); the report goes to
//! stdout / the CI job summary — GitHub Issues stays the backlog, so there
//! is no committed report file.

use bridge::{CommandKind, CommandMeta, CommandStatus, StoryPolicy, UNFILED};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The `@nge/core` facade map (repo-relative).
pub const FACADE_MAP: &str = "packages/core/src/facadeMap.ts";
/// Roots scanned for `data-nge-*` attributes and "Engine pending" badges.
pub const UI_ROOTS: &[&str] = &["packages/ui/src", "packages/core/src", "ts/src"];
/// The Playwright suite.
pub const E2E_ROOT: &str = "ts/e2e";
/// The `@nge/ui` toast (`ERROR_TOAST_COPY` and friends, issue #427).
pub const ERROR_TOAST: &str = "packages/ui/src/ErrorToast.tsx";
/// The fuzz generator holding the `classify_variants!` list.
pub const FUZZ_GEN: &str = "fuzz/src/command_gen.rs";
/// Ratchet: `Implemented` commands the e2e suite dispatches by wire name.
/// Raise it as coverage grows; never lower it.
pub const E2E_IMPLEMENTED_FLOOR: usize = 45;
/// Gaps allowed to cite `bridge::UNFILED` instead of an issue number —
/// none (mirrors `bridge::meta`'s own test; #396 / #397 filed the last
/// four). File the issue before classifying a gap.
pub const MAX_UNFILED: usize = 0;

/// The repository root this crate lives in (`tools/parity/../..`).
pub fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/* ======================================================================
Sources
====================================================================== */

/// How `COMMAND_FACADE` classifies a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FacadeTarget {
    /// Exposed as this `EditorCommands` member.
    Member(String),
    /// Dispatched by the shell / worker / harness only.
    Internal,
    /// Not reachable: the engine only stubs it.
    Stub,
}

/// What a facade member dispatches (`FACADE_METHOD_COMMANDS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberTarget {
    Commands(Vec<String>),
    Host,
    Raw,
}

/// One JSX element carrying a `data-nge-command`, a
/// `data-nge-pending-issue`, or a literal "Engine pending" badge text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiElement {
    pub file: String,
    pub line: usize,
    pub commands: Vec<String>,
    /// `Some(n)` for `data-nge-pending-issue="n"`; `Some(0)` when the
    /// attribute is present but not a number.
    pub pending_issue: Option<u32>,
    /// The element's own text is the "Engine pending" badge.
    pub badge_text: bool,
}

/// Everything the matrix joins besides `bridge::meta`.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub facade: BTreeMap<String, FacadeTarget>,
    pub members: BTreeMap<String, MemberTarget>,
    pub ui: Vec<UiElement>,
    /// Wire names the e2e suite mentions as string literals.
    pub e2e: BTreeSet<String>,
    /// Variant names with a curated fuzz generator arm.
    pub fuzz_curated: BTreeSet<String>,
    /// Every variant name `classify_variants!` lists.
    pub fuzz_listed: BTreeSet<String>,
    /// Issue #427 - `ErrorKind` names with an `ERROR_TOAST_COPY` entry.
    pub toast_copy: BTreeSet<String>,
    /// Issue #427 - `ErrorKind` names in `ERROR_KINDS_WITH_OWN_PRESENTATION`.
    pub own_presentation: BTreeSet<String>,
    /// Issue #427 - the `ErrorKind` keys of `ERROR_KIND_PRESENTATION`.
    pub presentation_keys: BTreeSet<String>,
}

/// Issue #427 - parse the error-kind tables out of `ErrorToast.tsx`:
/// `(toast_copy, own_presentation, presentation_keys)`.
pub fn parse_error_toast(src: &str) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let keys = |name: &str| -> BTreeSet<String> {
        object_lines(src, name)
            .into_iter()
            .filter(|l| {
                /* A key sits at the object's own indent (4 spaces); the
                wrapped string continuation lines are deeper. */
                l.starts_with("    ") && !l.starts_with("     ")
            })
            .filter_map(|l| l.trim().split_once(':').map(|(k, _)| k.trim().to_string()))
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect()
    };
    let mut own = BTreeSet::new();
    if let Some(start) = src.find("export const ERROR_KINDS_WITH_OWN_PRESENTATION") {
        let rest = &src[start..];
        if let Some(end) = rest.find("] as const") {
            let body = &rest[rest.find("= [").map_or(0, |i| i + 3)..end];
            for part in body.split(',') {
                if let Some(n) = unquote(part) {
                    own.insert(n.to_string());
                }
            }
        }
    }
    (
        keys("ERROR_TOAST_COPY"),
        own,
        keys("ERROR_KIND_PRESENTATION"),
    )
}

fn read(root: &Path, rel: &str) -> std::io::Result<String> {
    std::fs::read_to_string(root.join(rel))
}

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "node_modules" && name != "dist" && !name.starts_with('.') {
                walk(&path, exts, out)?;
            }
        } else if exts.iter().any(|e| name.ends_with(e)) {
            out.push(path);
        }
    }
    Ok(())
}

/// Load every source from the repository at `root`.
pub fn load(root: &Path) -> std::io::Result<Sources> {
    let (facade, members) = parse_facade_map(&read(root, FACADE_MAP)?);
    let mut ui = Vec::new();
    for dir in UI_ROOTS {
        let mut files = Vec::new();
        walk(&root.join(dir), &[".ts", ".tsx"], &mut files)?;
        for f in files {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(&f)
                .to_string_lossy()
                .replace('\\', "/");
            ui.extend(scan_ui(&rel, &std::fs::read_to_string(&f)?));
        }
    }
    let mut e2e_text = String::new();
    let mut files = Vec::new();
    walk(&root.join(E2E_ROOT), &[".ts"], &mut files)?;
    for f in files {
        e2e_text.push_str(&std::fs::read_to_string(&f)?);
        e2e_text.push('\n');
    }
    let e2e = CommandKind::ALL
        .iter()
        .map(|k| k.wire_name())
        .filter(|w| e2e_text.contains(&format!("'{w}'")) || e2e_text.contains(&format!("\"{w}\"")))
        .collect();
    let (fuzz_listed, fuzz_curated) = parse_fuzz_classification(&read(root, FUZZ_GEN)?);
    let (toast_copy, own_presentation, presentation_keys) =
        parse_error_toast(&read(root, ERROR_TOAST)?);
    Ok(Sources {
        toast_copy,
        own_presentation,
        presentation_keys,
        facade,
        members,
        ui,
        e2e,
        fuzz_curated,
        fuzz_listed,
    })
}

/// The lines of the `export const <name>` object literal in `src`.
fn object_lines<'a>(src: &'a str, name: &str) -> Vec<&'a str> {
    let Some(start) = src.find(&format!("export const {name}")) else {
        return Vec::new();
    };
    let body = &src[start..];
    /* The initializer's `{` — after the `=` (which may sit on its own line
    when the type annotation is long), past any whitespace. */
    let mut from = 0;
    let open = loop {
        let Some(eq) = body[from..].find('=') else {
            return Vec::new();
        };
        let after = &body[from + eq + 1..];
        let trimmed = after.trim_start();
        if trimmed.starts_with('{') {
            break from + eq + 1 + (after.len() - trimmed.len()) + 1;
        }
        from += eq + 1;
    };
    body[open..]
        .lines()
        .take_while(|l| !l.trim_start().starts_with('}'))
        .collect()
}

/// `key: value,` with any trailing `// comment` removed.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let code = line.split("//").next()?.trim();
    if code.is_empty() || code.starts_with("/*") || code.starts_with('*') {
        return None;
    }
    let (k, v) = code.split_once(':')?;
    Some((k.trim(), v.trim().trim_end_matches(',').trim()))
}

fn unquote(v: &str) -> Option<&str> {
    let v = v.trim();
    v.strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .or_else(|| v.strip_prefix('"').and_then(|r| r.strip_suffix('"')))
}

/// Parse `COMMAND_FACADE` and `FACADE_METHOD_COMMANDS` out of facadeMap.ts.
pub fn parse_facade_map(
    src: &str,
) -> (
    BTreeMap<String, FacadeTarget>,
    BTreeMap<String, MemberTarget>,
) {
    let mut facade = BTreeMap::new();
    for line in object_lines(src, "COMMAND_FACADE") {
        let Some((k, v)) = key_value(line) else {
            continue;
        };
        let Some(v) = unquote(v) else {
            continue;
        };
        let target = match v {
            "internal" => FacadeTarget::Internal,
            "stub" => FacadeTarget::Stub,
            m => FacadeTarget::Member(m.to_string()),
        };
        facade.insert(k.to_string(), target);
    }
    let mut members = BTreeMap::new();
    for line in object_lines(src, "FACADE_METHOD_COMMANDS") {
        let Some((k, v)) = key_value(line) else {
            continue;
        };
        let target = if let Some(list) = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            MemberTarget::Commands(
                list.split(',')
                    .filter_map(|c| unquote(c).map(str::to_string))
                    .collect(),
            )
        } else {
            match unquote(v) {
                Some("host") => MemberTarget::Host,
                Some("raw") => MemberTarget::Raw,
                _ => continue,
            }
        };
        members.insert(k.to_string(), target);
    }
    (facade, members)
}

/// `(every listed variant, the curated ones)` from `classify_variants!`.
pub fn parse_fuzz_classification(src: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut listed = BTreeSet::new();
    let mut curated = BTreeSet::new();
    let Some(start) = src.find("\nclassify_variants! {") else {
        return (listed, curated);
    };
    for line in src[start + 1..].lines().skip(1) {
        if line.starts_with('}') {
            break;
        }
        let code = line.split("//").next().unwrap_or("").trim();
        let Some((lhs, rhs)) = code.split_once("=>") else {
            continue;
        };
        let name: String = lhs
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        if rhs.trim().trim_end_matches(',').trim() == "true" {
            curated.insert(name.clone());
        }
        listed.insert(name);
    }
    (listed, curated)
}

/// Start of the JSX/HTML opening tag enclosing byte `at`: the nearest `<`
/// before it that is followed by a tag-name letter.
fn tag_start(src: &str, at: usize) -> Option<usize> {
    let mut end = at;
    while let Some(p) = src[..end].rfind('<') {
        if src[p + 1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            return Some(p);
        }
        end = p;
    }
    None
}

/// End (exclusive) of the opening tag starting at `start`: the first `>`
/// outside every `{ … }` expression and string literal, so arrow functions
/// in attribute expressions (`onClick={() => …}`) do not end the tag.
pub fn tag_end(src: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for (off, ch) in src[start..].char_indices() {
        if let Some(q) = quote {
            if ch == q && prev != '\\' {
                quote = None;
            }
        } else {
            match ch {
                '"' | '\'' | '`' => quote = Some(ch),
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                '>' if depth == 0 => return Some(start + off + 1),
                _ => {}
            }
        }
        prev = ch;
    }
    None
}

/// Every value of attribute `name` in the tag text (`name="v"`,
/// `name='v'` or `name={v}` / `name={'v'}`).
fn attr_values(tag: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(rel) = tag[from..].find(&needle) {
        let at = from + rel + needle.len();
        let rest = &tag[at..];
        let value = if let Some(r) = rest.strip_prefix('"') {
            r.split('"').next()
        } else if let Some(r) = rest.strip_prefix('\'') {
            r.split('\'').next()
        } else if let Some(r) = rest.strip_prefix('{') {
            r.split('}')
                .next()
                .map(|v| v.trim().trim_matches(['\'', '"']))
        } else {
            None
        };
        if let Some(v) = value {
            out.push(v.to_string());
        }
        from = at;
    }
    out
}

fn line_of(src: &str, at: usize) -> usize {
    src[..at].matches('\n').count() + 1
}

/// Scan one source file for parity-relevant JSX elements.
pub fn scan_ui(file: &str, src: &str) -> Vec<UiElement> {
    let mut starts = BTreeSet::new();
    for needle in ["data-nge-command=", "data-nge-pending-issue="] {
        let mut from = 0;
        while let Some(rel) = src[from..].find(needle) {
            let at = from + rel;
            if let Some(s) = tag_start(src, at) {
                starts.insert(s);
            }
            from = at + needle.len();
        }
    }
    /* A literal badge text: "Engine pending" as JSX text (between `>` and
    `<`, ignoring whitespace) — never a doc comment or a title string. */
    let mut badge_starts = BTreeSet::new();
    let mut from = 0;
    while let Some(rel) = src[from..].find("Engine pending") {
        let at = from + rel;
        let before = src[..at].trim_end();
        let after = src[at + "Engine pending".len()..].trim_start();
        if before.ends_with('>')
            && after.starts_with('<')
            && let Some(s) = tag_start(src, before.len() - 1)
        {
            starts.insert(s);
            badge_starts.insert(s);
        }
        from = at + 1;
    }
    let mut out = Vec::new();
    for start in starts {
        let Some(end) = tag_end(src, start) else {
            continue;
        };
        let tag = &src[start..end];
        out.push(UiElement {
            file: file.to_string(),
            line: line_of(src, start),
            commands: attr_values(tag, "data-nge-command"),
            pending_issue: attr_values(tag, "data-nge-pending-issue")
                .first()
                .map(|v| v.parse().unwrap_or(0)),
            badge_text: badge_starts.contains(&start),
        });
    }
    out
}

/* ======================================================================
Matrix
====================================================================== */

/// One `Command` variant's row.
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: CommandKind,
    pub wire: String,
    pub meta: &'static CommandMeta,
    pub facade: Option<FacadeTarget>,
    /// UI elements naming this command without a pending badge.
    pub ui_controls: usize,
    /// Issues cited by pending badges naming this command.
    pub ui_badges: Vec<u32>,
    pub e2e: bool,
    pub fuzz_curated: bool,
}

impl Row {
    /// Live = the engine does real work for at least some inputs. A stub
    /// is never live, whatever references it.
    pub fn is_live(&self) -> bool {
        self.meta.status.is_live()
    }
}

/// Per-status totals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub implemented: usize,
    pub partial: usize,
    pub stub: usize,
    pub live: usize,
    pub unfiled: usize,
    pub facade_members: usize,
    pub internal: usize,
    pub e2e_implemented: usize,
    pub fuzz_curated: usize,
}

/// The joined matrix.
#[derive(Clone, Debug)]
pub struct Matrix {
    pub rows: Vec<Row>,
    pub sources: Sources,
}

fn status_name(s: CommandStatus) -> &'static str {
    match s {
        CommandStatus::Implemented => "implemented",
        CommandStatus::Partial { .. } => "partial",
        CommandStatus::Stub { .. } => "stub",
    }
}

fn story_name(s: StoryPolicy) -> &'static str {
    match s {
        StoryPolicy::Allowed => "allowed",
        StoryPolicy::BodyOnly => "body only",
        StoryPolicy::TextBoxOnly => "text box only",
        StoryPolicy::ExitsStory => "exits story",
    }
}

fn issue_cell(issue: u32) -> String {
    if issue == UNFILED {
        "UNFILED".to_string()
    } else {
        format!("#{issue}")
    }
}

impl Matrix {
    /// Join `bridge::meta` with `sources`.
    pub fn build(sources: Sources) -> Matrix {
        let rows = CommandKind::ALL
            .iter()
            .map(|&kind| {
                let wire = kind.wire_name();
                let mut ui_controls = 0;
                let mut ui_badges = Vec::new();
                for el in sources.ui.iter().filter(|e| e.commands.contains(&wire)) {
                    match el.pending_issue {
                        Some(n) => ui_badges.push(n),
                        None => ui_controls += 1,
                    }
                }
                Row {
                    kind,
                    meta: kind.meta(),
                    facade: sources.facade.get(&wire).cloned(),
                    ui_controls,
                    ui_badges,
                    e2e: sources.e2e.contains(&wire),
                    fuzz_curated: sources.fuzz_curated.contains(kind.name()),
                    wire,
                }
            })
            .collect();
        Matrix { rows, sources }
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            total: self.rows.len(),
            ..Counts::default()
        };
        for r in &self.rows {
            match r.meta.status {
                CommandStatus::Implemented => c.implemented += 1,
                CommandStatus::Partial { .. } => c.partial += 1,
                CommandStatus::Stub { .. } => c.stub += 1,
            }
            if r.is_live() {
                c.live += 1;
            }
            if r.meta.status.issue() == Some(UNFILED) {
                c.unfiled += 1;
            }
            match r.facade {
                Some(FacadeTarget::Member(_)) => c.facade_members += 1,
                Some(FacadeTarget::Internal) => c.internal += 1,
                _ => {}
            }
            if r.e2e && r.meta.status == CommandStatus::Implemented {
                c.e2e_implemented += 1;
            }
            if r.fuzz_curated {
                c.fuzz_curated += 1;
            }
        }
        c
    }

    /// Every issue number a gap cites (UNFILED excluded).
    pub fn cited_issues(&self) -> BTreeSet<u32> {
        self.rows
            .iter()
            .filter_map(|r| r.meta.status.issue())
            .filter(|&n| n != UNFILED)
            .collect()
    }

    /// Issue #427 - every `bridge::ErrorKind` has toast copy OR a declared
    /// own presentation (floor: 0 uncovered), never both, and the shell's
    /// tables name only real kinds.
    pub fn error_kind_violations(&self) -> Vec<String> {
        let s = &self.sources;
        let mut v = Vec::new();
        let kinds: BTreeSet<&str> = bridge::ErrorKind::ALL.iter().map(|k| k.name()).collect();
        for k in &kinds {
            let copy = s.toast_copy.contains(*k);
            let own = s.own_presentation.contains(*k);
            if !copy && !own {
                v.push(format!(
                    "ErrorKind::{k} has no ERROR_TOAST_COPY entry and is not in \
                     ERROR_KINDS_WITH_OWN_PRESENTATION ({ERROR_TOAST})"
                ));
            }
            if copy && own {
                v.push(format!(
                    "ErrorKind::{k} has toast copy AND an own presentation"
                ));
            }
            if !s.presentation_keys.contains(*k) {
                v.push(format!(
                    "ErrorKind::{k} is missing from ERROR_KIND_PRESENTATION"
                ));
            }
        }
        for (what, set) in [
            ("ERROR_TOAST_COPY", &s.toast_copy),
            ("ERROR_KINDS_WITH_OWN_PRESENTATION", &s.own_presentation),
            ("ERROR_KIND_PRESENTATION", &s.presentation_keys),
        ] {
            for k in set {
                if !kinds.contains(k.as_str()) {
                    v.push(format!("{what} names unknown ErrorKind {k}"));
                }
            }
        }
        v
    }

    /// The floor. Empty means every check passes.
    pub fn violations(&self) -> Vec<String> {
        let mut v = Vec::new();
        let wires: BTreeSet<&str> = self.rows.iter().map(|r| r.wire.as_str()).collect();
        let by_wire: BTreeMap<&str, &Row> =
            self.rows.iter().map(|r| (r.wire.as_str(), r)).collect();
        let s = &self.sources;

        /* -- meta ------------------------------------------------------ */
        let unfiled = self.counts().unfiled;
        if unfiled > MAX_UNFILED {
            v.push(format!(
                "{unfiled} gaps cite UNFILED (cap {MAX_UNFILED}) — file the issues and cite them"
            ));
        }

        /* -- fuzz: classify_variants! and bridge::meta list the same variants */
        let names: BTreeSet<String> = self.rows.iter().map(|r| r.kind.name().into()).collect();
        if s.fuzz_listed != names {
            v.push(format!(
                "fuzz classify_variants! and bridge::meta disagree: only in fuzz {:?}, only in meta {:?}",
                s.fuzz_listed.difference(&names).collect::<Vec<_>>(),
                names.difference(&s.fuzz_listed).collect::<Vec<_>>()
            ));
        }

        /* -- error kinds (issue #427) ---------------------------------- */
        v.extend(self.error_kind_violations());

        /* -- facade ---------------------------------------------------- */
        for r in &self.rows {
            let is_stub = matches!(r.meta.status, CommandStatus::Stub { .. });
            match &r.facade {
                None => v.push(format!("{}: not classified in COMMAND_FACADE", r.wire)),
                Some(FacadeTarget::Stub) if !is_stub => {
                    v.push(format!("{}: COMMAND_FACADE says 'stub' but meta says {}", r.wire, status_name(r.meta.status)));
                }
                Some(FacadeTarget::Member(_) | FacadeTarget::Internal) if is_stub => {
                    v.push(format!(
                        "{}: a stub must map to 'stub' in COMMAND_FACADE, not {}",
                        r.wire,
                        facade_cell(&r.facade)
                    ));
                }
                Some(FacadeTarget::Member(m)) => match s.members.get(m) {
                    Some(MemberTarget::Commands(cmds)) if cmds.contains(&r.wire) => {}
                    _ => v.push(format!(
                        "{}: COMMAND_FACADE names `{m}`, but FACADE_METHOD_COMMANDS.{m} does not list it",
                        r.wire
                    )),
                },
                _ => {}
            }
        }
        for wire in s.facade.keys() {
            if !wires.contains(wire.as_str()) {
                v.push(format!("COMMAND_FACADE classifies unknown command {wire}"));
            }
        }
        for (member, target) in &s.members {
            if let MemberTarget::Commands(cmds) = target {
                for c in cmds {
                    match by_wire.get(c.as_str()) {
                        None => v.push(format!("facade `{member}` dispatches unknown command {c}")),
                        Some(r) if !r.is_live() => v.push(format!(
                            "facade `{member}` dispatches {c}, which the engine only stubs ({})",
                            issue_cell(r.meta.status.issue().unwrap_or(UNFILED))
                        )),
                        _ => {}
                    }
                }
            }
        }

        /* -- UI ---------------------------------------------------------- */
        for el in &s.ui {
            let at = format!("{}:{}", el.file, el.line);
            for c in &el.commands {
                if !wires.contains(c.as_str()) {
                    v.push(format!("{at}: data-nge-command names unknown command {c}"));
                }
            }
            if el.badge_text && el.pending_issue.is_none() {
                v.push(format!(
                    "{at}: an \"Engine pending\" badge without data-nge-pending-issue"
                ));
            }
            if let Some(issue) = el.pending_issue {
                if el.commands.is_empty() {
                    v.push(format!(
                        "{at}: data-nge-pending-issue without data-nge-command"
                    ));
                }
                for c in &el.commands {
                    let Some(r) = by_wire.get(c.as_str()) else {
                        continue;
                    };
                    match r.meta.status.issue() {
                        Some(n) if n == issue && n != UNFILED => {}
                        Some(n) => v.push(format!(
                            "{at}: badge cites #{issue} for {c}, meta cites {}",
                            issue_cell(n)
                        )),
                        None => v.push(format!(
                            "{at}: stale badge — {c} is implemented (badge cites #{issue})"
                        )),
                    }
                }
            } else {
                for c in &el.commands {
                    if let Some(r) = by_wire.get(c.as_str())
                        && !r.is_live()
                    {
                        v.push(format!(
                            "{at}: {c} is a stub exposed without an \"Engine pending\" badge"
                        ));
                    }
                }
            }
        }
        for r in &self.rows {
            if let CommandStatus::Partial { issue } = r.meta.status
                && !r.ui_badges.contains(&issue)
            {
                v.push(format!(
                    "{}: partial ({}) but no UI element renders its pending badge \
                     (data-nge-command=\"{}\" data-nge-pending-issue=\"{issue}\")",
                    r.wire,
                    issue_cell(issue),
                    r.wire
                ));
            }
        }

        /* -- e2e --------------------------------------------------------- */
        let e2e = self.counts().e2e_implemented;
        if e2e < E2E_IMPLEMENTED_FLOOR {
            v.push(format!(
                "e2e covers {e2e} implemented commands, below the floor of {E2E_IMPLEMENTED_FLOOR}"
            ));
        }
        v
    }

    /// The Markdown report (stdout / `$GITHUB_STEP_SUMMARY`).
    pub fn render_markdown(&self, extra: &[String]) -> String {
        let c = self.counts();
        let mut out = String::new();
        let _ = writeln!(out, "## Command parity matrix (issue #342)\n");
        let _ = writeln!(
            out,
            "Source of truth: `crates/bridge/src/meta.rs`. Rows are `Command` variants; \
             a stub never counts as live.\n"
        );
        let _ = writeln!(out, "| Status | Commands |\n|---|---:|");
        let _ = writeln!(out, "| Implemented | {} |", c.implemented);
        let _ = writeln!(out, "| Partial | {} |", c.partial);
        let _ = writeln!(out, "| Stub | {} |", c.stub);
        let _ = writeln!(out, "| **Total** | **{}** |\n", c.total);
        let _ = writeln!(
            out,
            "- Live (implemented + partial): **{} / {}**\n\
             - Exposed on the `@nge/core` facade: {} · internal-only: {} · stub: {}\n\
             - Implemented commands the e2e suite dispatches: {} (floor {})\n\
             - Curated fuzz generator arms: {} (every variant is reachable blind)\n\
             - Gaps citing UNFILED: {} (cap {})\n",
            c.live,
            c.total,
            c.facade_members,
            c.internal,
            c.stub,
            c.e2e_implemented,
            E2E_IMPLEMENTED_FLOOR,
            c.fuzz_curated,
            c.unfiled,
            MAX_UNFILED,
        );
        let kinds = bridge::ErrorKind::ALL.len();
        let uncovered = self.error_kind_violations().len();
        let _ = writeln!(
            out,
            "- Error kinds (issue #427): {kinds}, toast copy for {}, own presentation for {}, \
             uncovered: {uncovered} (floor 0)\n",
            self.sources.toast_copy.len(),
            self.sources.own_presentation.len(),
        );
        let _ = writeln!(out, "### Gaps\n");
        let _ = writeln!(
            out,
            "| Command | Status | Issue | Facade | UI badge |\n|---|---|---|---|---|"
        );
        for r in self
            .rows
            .iter()
            .filter(|r| r.meta.status != CommandStatus::Implemented)
        {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} |",
                r.wire,
                status_name(r.meta.status),
                issue_cell(r.meta.status.issue().unwrap_or(UNFILED)),
                facade_cell(&r.facade),
                if r.ui_badges.is_empty() {
                    "—".to_string()
                } else {
                    r.ui_badges
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
        }
        let violations = self.violations();
        let _ = writeln!(out, "\n### Floor\n");
        if violations.is_empty() && extra.is_empty() {
            let _ = writeln!(out, "PASS — every floor check holds.");
        } else {
            for line in violations.iter().chain(extra) {
                let _ = writeln!(out, "- FAIL: {line}");
            }
        }
        let _ = writeln!(
            out,
            "\n<details><summary>Full matrix ({} commands)</summary>\n",
            c.total
        );
        let _ = writeln!(
            out,
            "| Command | Status | Logged | Read-only | Story | Facade | UI | e2e | Fuzz |\n\
             |---|---|---|---|---|---|---|---|---|"
        );
        for r in &self.rows {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                r.wire,
                match r.meta.status {
                    CommandStatus::Implemented => "implemented".to_string(),
                    other => format!(
                        "{} {}",
                        status_name(other),
                        issue_cell(other.issue().unwrap_or(UNFILED))
                    ),
                },
                yes(r.meta.logged),
                yes(r.meta.read_only),
                story_name(r.meta.story),
                facade_cell(&r.facade),
                match (r.ui_controls, r.ui_badges.len()) {
                    (0, 0) => "—".to_string(),
                    (n, 0) => format!("{n} control(s)"),
                    (n, b) => format!("{n} control(s), {b} badge(s)"),
                },
                yes(r.e2e),
                if r.fuzz_curated { "curated" } else { "blind" },
            );
        }
        let _ = writeln!(out, "\n</details>");
        out
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "—" }
}

fn facade_cell(t: &Option<FacadeTarget>) -> String {
    match t {
        Some(FacadeTarget::Member(m)) => format!("`{m}()`"),
        Some(FacadeTarget::Internal) => "internal".to_string(),
        Some(FacadeTarget::Stub) => "stub".to_string(),
        None => "UNCLASSIFIED".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_matrix() -> Matrix {
        Matrix::build(load(&default_root()).expect("read the repository sources"))
    }

    /// Issue #342 — the floor, on this repository: every gap cites an
    /// issue, every partial renders its badge, no stub is exposed (facade
    /// or UI), the facade maps agree with meta and each other, and the e2e
    /// coverage ratchet holds.
    #[test]
    fn floor_holds_on_this_repository() {
        let m = repo_matrix();
        let v = m.violations();
        assert!(v.is_empty(), "parity floor violations:\n{}", v.join("\n"));
    }

    /// Issue #427 - the error-kind floor reads the real tables and trips on
    /// a kind with neither copy nor a declared own presentation.
    #[test]
    fn error_kinds_need_copy_or_an_own_presentation() {
        let m = repo_matrix();
        assert!(
            m.sources.toast_copy.contains("InTableCell"),
            "{:?}",
            m.sources.toast_copy
        );
        assert!(m.sources.own_presentation.contains("Protected"));
        assert_eq!(
            m.sources.presentation_keys.len(),
            bridge::ErrorKind::ALL.len(),
            "ERROR_KIND_PRESENTATION lists every kind"
        );
        assert!(m.error_kind_violations().is_empty());
        let mut broken = m.clone();
        broken.sources.toast_copy.remove("InTableCell");
        let v = broken.error_kind_violations();
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("InTableCell"), "{v:?}");
        let mut unknown = m;
        unknown.sources.toast_copy.insert("Bogus".into());
        assert!(unknown.error_kind_violations()[0].contains("unknown ErrorKind Bogus"));
    }

    /// The scanner really sees the shell: the facade map is fully parsed,
    /// the File menu / history / image-wrap attributes and the #137 badge
    /// are found, and the fuzz list is read.
    #[test]
    fn sources_are_actually_read() {
        let m = repo_matrix();
        assert_eq!(m.sources.facade.len(), CommandKind::ALL.len());
        assert!(m.sources.members.contains_key("closeDocument"));
        assert_eq!(m.sources.fuzz_listed.len(), CommandKind::ALL.len());
        let row = |w: &str| m.rows.iter().find(|r| r.wire == w).unwrap();
        assert!(row("OPEN_DOCUMENT").ui_controls >= 1);
        assert!(row("UNDO").ui_controls >= 1);
        assert_eq!(row("SET_IMAGE_WRAP").ui_badges, vec![137]);
        assert!(row("INSERT_TEXT").e2e);
        assert!(row("INSERT_TEXT").fuzz_curated);
    }

    /// "A stub never counts as live": references (facade, UI, e2e) do not
    /// make a stub live, and exposing one without a badge is a violation.
    #[test]
    fn a_stub_never_counts_as_live() {
        let mut sources = load(&default_root()).unwrap();
        sources.e2e.insert("INIT".into());
        sources.ui.push(UiElement {
            file: "synthetic.tsx".into(),
            line: 1,
            commands: vec!["INIT".into()],
            pending_issue: None,
            badge_text: false,
        });
        sources
            .facade
            .insert("INIT".into(), FacadeTarget::Member("requestStats".into()));
        let m = Matrix::build(sources);
        let baseline = repo_matrix().counts();
        assert_eq!(
            m.counts().live,
            baseline.live,
            "a referenced stub is still not live"
        );
        let v = m.violations().join("\n");
        assert!(v.contains("INIT is a stub exposed without"), "{v}");
        assert!(v.contains("a stub must map to 'stub'"), "{v}");
    }

    /// Badge consistency: a badge on an implemented command is stale, and
    /// a badge must cite the issue meta cites.
    #[test]
    fn badges_must_match_meta() {
        let mut sources = load(&default_root()).unwrap();
        sources.ui.push(UiElement {
            file: "synthetic.tsx".into(),
            line: 2,
            commands: vec!["INSERT_TEXT".into()],
            pending_issue: Some(1),
            badge_text: true,
        });
        sources.ui.push(UiElement {
            file: "synthetic.tsx".into(),
            line: 3,
            commands: vec!["SET_IMAGE_WRAP".into()],
            pending_issue: Some(999),
            badge_text: true,
        });
        sources.ui.push(UiElement {
            file: "synthetic.tsx".into(),
            line: 4,
            commands: Vec::new(),
            pending_issue: None,
            badge_text: true,
        });
        let v = Matrix::build(sources).violations().join("\n");
        assert!(
            v.contains("stale badge — INSERT_TEXT is implemented"),
            "{v}"
        );
        assert!(
            v.contains("badge cites #999 for SET_IMAGE_WRAP, meta cites #137"),
            "{v}"
        );
        assert!(
            v.contains("an \"Engine pending\" badge without data-nge-pending-issue"),
            "{v}"
        );
    }

    #[test]
    fn tag_end_skips_arrow_functions_and_strings() {
        let src = r#"<button onClick={() => go(a > b)} title="x > y" data-nge-command="UNDO">"#;
        assert_eq!(tag_end(src, 0), Some(src.len()));
    }

    #[test]
    fn scan_ui_reads_attributes_and_badges() {
        let src = r#"
            <span class="x" data-nge-command="SET_IMAGE_WRAP" data-nge-pending-issue="137">
                Engine pending
            </span>
            <button onClick={() => void cmd.undo()} data-nge-command="UNDO">u</button>
            /** a doc comment saying "Engine pending" is not a badge */
            <b>Engine pending</b>
        "#;
        let els = scan_ui("f.tsx", src);
        assert_eq!(els.len(), 3, "{els:?}");
        assert_eq!(els[0].commands, ["SET_IMAGE_WRAP"]);
        assert_eq!(els[0].pending_issue, Some(137));
        assert!(els[0].badge_text);
        assert_eq!(els[1].commands, ["UNDO"]);
        assert_eq!(els[1].pending_issue, None);
        assert!(els[2].badge_text && els[2].commands.is_empty());
    }

    #[test]
    fn parse_facade_map_reads_both_maps() {
        let src = "export const COMMAND_FACADE: X = {\n    PING: 'internal', // c\n    INIT: 'stub',\n    UNDO: 'undo',\n};\n\
                   export const FACADE_METHOD_COMMANDS: Y =\n    {\n        undo: ['UNDO'],\n        open: ['SET_ZOOM', 'OPEN_DOCUMENT'],\n        raw: 'raw',\n        canX: 'host',\n    };\n";
        let (f, m) = parse_facade_map(src);
        assert_eq!(f["PING"], FacadeTarget::Internal);
        assert_eq!(f["INIT"], FacadeTarget::Stub);
        assert_eq!(f["UNDO"], FacadeTarget::Member("undo".into()));
        assert_eq!(
            m["open"],
            MemberTarget::Commands(vec!["SET_ZOOM".into(), "OPEN_DOCUMENT".into()])
        );
        assert_eq!(m["raw"], MemberTarget::Raw);
        assert_eq!(m["canX"], MemberTarget::Host);
    }
}
