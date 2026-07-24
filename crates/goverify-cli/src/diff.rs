//! `--diff-base` (phase-5b spec §5): git-worktree the base ref, extract
//! it (the extraction cache applies — the base tree's packages are
//! content-keyed like any other), compare position-blind function
//! hashes, and scope the report to changed functions plus their
//! transitive callers. All git access shells out to the `git` CLI
//! (dependency policy, spec §1: git is required only when the flag is
//! used). Failures here are hard errors — a silently-wrong report set
//! contradicts the explicit request (spec §5).

use std::path::{Path, PathBuf};
use std::process::Command;

use goverify_ir::{FuncId, Program};

pub struct BaseCheckout {
    /// The directory inside the worktree corresponding to the checked
    /// module (worktree root + the module's path prefix in the repo).
    pub module_dir: PathBuf,
    repo_dir: PathBuf,
    worktree: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        // Best-effort cleanup on every exit path (spec §5): deregister
        // the worktree; the TempDir removes the files themselves.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_dir)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&self.worktree)
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_dir)
            .args(["worktree", "prune"])
            .output();
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e} (--diff-base requires git on PATH)"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Check `git_ref` out into a temp worktree of the repo containing
/// `module_dir`.
pub fn checkout_base(module_dir: &Path, git_ref: &str) -> Result<BaseCheckout, String> {
    git(
        module_dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{git_ref}^{{commit}}"),
        ],
    )
    .map_err(|_| format!("--diff-base: unknown git ref {git_ref:?}"))?;
    // The module may sit below the repo root; mirror that inside the
    // worktree.
    let prefix = git(module_dir, &["rev-parse", "--show-prefix"])?;
    let repo_dir = PathBuf::from(git(module_dir, &["rev-parse", "--show-toplevel"])?);
    let tmp = tempfile::tempdir().map_err(|e| format!("--diff-base: tempdir: {e}"))?;
    let worktree = tmp.path().join("base");
    let worktree_str = worktree
        .to_str()
        .ok_or_else(|| "--diff-base: non-UTF-8 temp path".to_string())?;
    git(
        &repo_dir,
        &["worktree", "add", "--detach", worktree_str, git_ref],
    )?;
    let module_dir = worktree.join(&prefix);
    Ok(BaseCheckout {
        module_dir,
        repo_dir,
        worktree,
        _tmp: tmp,
    })
}

/// Changed = present now with a different position-blind hash, or
/// absent at the base (new function). Functions deleted at HEAD have no
/// current findings — nothing to report (spec §5). Externals hash by
/// name in both programs and so never count as changed.
pub fn changed_funcs(cur: &Program, base: &Program) -> Vec<FuncId> {
    cur.func_ids()
        .filter(|&f| match base.lookup_func(cur.func_name(f)) {
            None => true,
            Some(b) => base.func_semantic_hash(b) != cur.func_semantic_hash(f),
        })
        .collect()
}
