//! Shared helpers for the Python model runners (`tools/*.py`). The dashboard and the
//! `sessions`/`sleep-score`/`readiness-score` subcommands all locate the repo root,
//! resolve the DB path, and pick the venv python the same way — this is that logic in
//! one place so the four call sites can't drift.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Locate the repo root by walking up from the current dir (then the compiled-in
/// manifest dir) for a stable `marker` file, so the binary keeps working when invoked
/// from elsewhere in the checkout. Returns `None` if not found (soft-degrade callers).
pub fn repo_root(marker: &Path) -> Option<PathBuf> {
    let find = |start: &Path| -> Option<PathBuf> {
        start
            .ancestors()
            .find(|d| d.join(marker).is_file())
            .map(Path::to_path_buf)
    };
    std::env::current_dir()
        .ok()
        .and_then(|d| find(&d))
        .or_else(|| find(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

/// Like [`repo_root`] but errors with an actionable message when the marker (`what`)
/// can't be found — for subcommands that must fail rather than degrade.
pub fn require_repo_root(marker: &Path, what: &str) -> Result<PathBuf> {
    repo_root(marker)
        .ok_or_else(|| anyhow!("could not locate {what} — run from inside the open_oura checkout"))
}

/// Absolute DB path (Python children run with cwd = repo root). Errors on a missing DB
/// instead of letting a backend create an empty one.
pub fn resolve_db(db: &Path) -> Result<PathBuf> {
    let abs = db
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(db));
    if !abs.exists() {
        return Err(anyhow!(
            "database not found: {} (run `oura sync` first)",
            abs.display()
        ));
    }
    Ok(abs)
}

/// The repo venv's python (which has torch) if present, else `python3` on PATH.
pub fn venv_python(root: &Path) -> PathBuf {
    let venv = root.join(".venv/bin/python");
    if venv.is_file() {
        venv
    } else {
        PathBuf::from("python3")
    }
}
