//! `cargo run -p parity [-- --verify-issues] [--root <repo>]`
//!
//! Prints the issue-#342 Command parity matrix as Markdown on stdout (CI
//! appends it to `$GITHUB_STEP_SUMMARY`) and exits non-zero when a floor
//! check fails. `--verify-issues` additionally asks `gh` that every issue a
//! gap cites is still OPEN (a `gh` failure is a warning, never a pass/fail
//! signal — the network is not the floor).

use parity::{Matrix, default_root, load};
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: parity [--verify-issues] [--root <repo root>]";

fn issue_state(issue: u32) -> Result<String, String> {
    let out = Command::new("gh")
        .args([
            "issue",
            "view",
            &issue.to_string(),
            "--json",
            "state",
            "-q",
            ".state",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() -> ExitCode {
    let mut root = default_root();
    let mut verify = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--verify-issues" => verify = true,
            "--root" => match args.next() {
                Some(path) => root = path.into(),
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("parity: unknown argument `{other}`\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let sources = match load(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "parity: cannot read the sources under {}: {e}",
                root.display()
            );
            return ExitCode::from(2);
        }
    };
    let matrix = Matrix::build(sources);
    let mut extra = Vec::new();
    if verify {
        for issue in matrix.cited_issues() {
            match issue_state(issue) {
                Ok(state) if state == "OPEN" => {}
                Ok(state) => extra.push(format!(
                    "#{issue} is {state} — a cited gap must point at an open issue"
                )),
                Err(e) => eprintln!("::warning::parity: could not verify issue #{issue}: {e}"),
            }
        }
    }
    print!("{}", matrix.render_markdown(&extra));
    let violations = matrix.violations();
    if violations.is_empty() && extra.is_empty() {
        ExitCode::SUCCESS
    } else {
        for v in violations.iter().chain(&extra) {
            eprintln!("parity floor: {v}");
        }
        ExitCode::FAILURE
    }
}
