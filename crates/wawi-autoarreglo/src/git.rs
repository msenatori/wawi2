//! Git for the loop: a worktree out of the repo, one squash commit in, a
//! revert commit out. Nothing here resets, forces, rebases the owner's
//! branch or pushes. The only branches it deletes are its own
//! `autoarreglo/<id>`. Hooks are off for every call (`core.hooksPath`), so
//! nothing of the owner's runs on Wawi's behalf.

use std::path::Path;
use std::process::{Command, Stdio};

/// The branch prefix, and the trailers on the commits.
pub const BRANCH_PREFIX: &str = "autoarreglo/";
pub const TRAILER: &str = "Autoarreglo";
pub const REVERT_TRAILER: &str = "Autoarreglo-Revierte";
/// Who commits on the attempt's branch (never in the owner's history: the
/// squash commit is the owner's).
const BOT_NAME: &str = "Wawi (autoarreglo)";
const BOT_EMAIL: &str = "autoarreglo@wawi.local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Salida {
    pub code: i32,
    pub out: String,
    pub err: String,
}

/// Runs git in `dir`. GIT_DIR and friends are dropped, so the call is
/// about `dir` and nothing else.
pub fn run(dir: &Path, args: &[&str]) -> Salida {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "color.ui=false"])
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(key);
    }
    match command.output() {
        Ok(out) => Salida {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Salida {
            code: -1,
            out: String::new(),
            err: format!("no pude correr git: {e}"),
        },
    }
}

fn ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let s = run(dir, args);
    if s.code == 0 {
        Ok(s.out)
    } else {
        let why = if s.err.trim().is_empty() {
            &s.out
        } else {
            &s.err
        };
        Err(format!(
            "git {}: {}",
            args.iter().take(2).copied().collect::<Vec<_>>().join(" "),
            crate::truncate(why.trim(), 300)
        ))
    }
}

pub fn is_repo(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--git-dir"]).code == 0
}

/// A repo with no commit yet has no HEAD to branch from.
pub fn has_commits(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--verify", "-q", "HEAD"]).code == 0
}

pub fn head(dir: &Path) -> Result<String, String> {
    ok(dir, &["rev-parse", "HEAD"]).map(|s| s.trim().to_owned())
}

/// The checked-out branch, or `None` when HEAD is detached.
pub fn current_branch(dir: &Path) -> Option<String> {
    let s = run(dir, &["symbolic-ref", "--short", "-q", "HEAD"]);
    (s.code == 0).then(|| s.out.trim().to_owned())
}

/// Uncommitted changes, untracked files included (ignored ones are not).
pub fn dirty(dir: &Path) -> Result<Vec<String>, String> {
    Ok(ok(dir, &["status", "--porcelain"])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    run(
        dir,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
    .code
        == 0
}

pub fn worktree_add(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("no pude crear {e}"))?;
    }
    let path = path.to_string_lossy();
    ok(repo, &["worktree", "add", "-q", "-b", branch, &path, base]).map(|_| ())
}

/// Removes the worktree (if `branch` is given, the branch too). Only ever
/// an `autoarreglo/` branch.
pub fn worktree_remove(repo: &Path, path: &Path, branch: Option<&str>) -> Result<(), String> {
    if let Some(branch) = branch
        && !branch.starts_with(BRANCH_PREFIX)
    {
        return Err(format!("no borro {branch}: no es una rama de autoarreglo"));
    }
    if path.exists() {
        let p = path.to_string_lossy();
        run(repo, &["worktree", "remove", "--force", &p]);
        let _ = std::fs::remove_dir_all(path);
    }
    run(repo, &["worktree", "prune"]);
    if let Some(branch) = branch
        && branch_exists(repo, branch)
    {
        ok(repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

/// Commits everything the agent left. `None` when it changed nothing.
pub fn commit_all(worktree: &Path, message: &str) -> Result<Option<String>, String> {
    ok(worktree, &["add", "-A"])?;
    if run(worktree, &["diff", "--cached", "--quiet"]).code == 0 {
        return Ok(None);
    }
    ok(
        worktree,
        &[
            "-c",
            &format!("user.name={BOT_NAME}"),
            "-c",
            &format!("user.email={BOT_EMAIL}"),
            "commit",
            "-q",
            "--no-verify",
            "-m",
            message,
        ],
    )?;
    head(worktree).map(Some)
}

pub fn diff(dir: &Path, base: &str, tip: &str) -> Result<String, String> {
    ok(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            &format!("{base}..{tip}"),
        ],
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
}

pub fn numstat(dir: &Path, base: &str, tip: &str) -> Result<Vec<FileStat>, String> {
    let out = ok(
        dir,
        &[
            "diff",
            "--numstat",
            "--no-renames",
            &format!("{base}..{tip}"),
        ],
    )?;
    Ok(parse_numstat(&out))
}

pub fn parse_numstat(out: &str) -> Vec<FileStat> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (a, r, path) = (parts.next()?, parts.next()?, parts.next()?);
            let binary = a == "-" || r == "-";
            Some(FileStat {
                path: path.to_owned(),
                added: a.parse().unwrap_or(0),
                removed: r.parse().unwrap_or(0),
                binary,
            })
        })
        .collect()
}

pub fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    run(dir, &["merge-base", "--is-ancestor", a, b]).code == 0
}

/// Whether `branch` merges into HEAD with no conflict, without touching
/// the working tree (`git merge-tree --write-tree`).
pub fn merges_cleanly(repo: &Path, branch: &str) -> Result<(), String> {
    let s = run(
        repo,
        &["merge-tree", "--write-tree", "--name-only", "HEAD", branch],
    );
    match s.code {
        0 => Ok(()),
        1 => {
            let files: Vec<&str> = s.out.lines().skip(1).filter(|l| !l.is_empty()).collect();
            Err(format!(
                "choca con tu rama actual en {}",
                crate::truncate(&files.join(", "), 200)
            ))
        }
        _ => Err(format!(
            "no pude probar la mezcla: {}",
            crate::truncate(s.err.trim(), 200)
        )),
    }
}

/// Identity flags only when the repo has none (so the owner's own name is
/// used whenever it is set).
fn identity(repo: &Path) -> Vec<String> {
    let has = |key: &str| {
        run(repo, &["config", "--get", key])
            .out
            .trim()
            .chars()
            .next()
            .is_some()
    };
    let mut flags = Vec::new();
    if !has("user.name") {
        flags.extend(["-c".to_owned(), format!("user.name={BOT_NAME}")]);
    }
    if !has("user.email") {
        flags.extend(["-c".to_owned(), format!("user.email={BOT_EMAIL}")]);
    }
    flags
}

/// `branch` onto the current branch as ONE commit. The caller has checked
/// the tree is clean and the merge is clean; this checks again.
pub fn squash_apply(repo: &Path, branch: &str, message: &str) -> Result<String, String> {
    if !dirty(repo)?.is_empty() {
        return Err("tu árbol tiene cambios sin commitear".into());
    }
    merges_cleanly(repo, branch)?;
    ok(repo, &["merge", "--squash", "-q", branch])?;
    let mut args: Vec<String> = identity(repo);
    args.extend(
        ["commit", "-q", "--no-verify", "-m", message]
            .iter()
            .map(|s| (*s).to_owned()),
    );
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = ok(repo, &refs) {
        return Err(format!(
            "el commit falló ({e}); quedó la mezcla preparada en tu árbol, sin commit"
        ));
    }
    head(repo)
}

/// The `<id>` of `sha`'s `Autoarreglo: <id>` trailer, if it has one.
pub fn trailer_of(repo: &Path, sha: &str) -> Option<String> {
    let message = ok(repo, &["log", "-1", "--format=%B", sha]).ok()?;
    // The trailer is the last such line (the subject also starts with it).
    message.lines().rev().find_map(|l| {
        l.strip_prefix(&format!("{TRAILER}: "))
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.contains(' '))
    })
}

pub fn already_reverted(repo: &Path, sha: &str) -> bool {
    let needle = format!("{REVERT_TRAILER}: {sha}");
    ok(
        repo,
        &["log", "--format=%B", &format!("--grep={needle}"), "HEAD"],
    )
    .is_ok_and(|out| out.contains(&needle))
}

/// `git revert` of one autoarreglo commit, as a new commit. Never a reset.
pub fn revert(repo: &Path, sha: &str, id: &str, reason: &str) -> Result<String, String> {
    let sha = ok(
        repo,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
    )
    .map_err(|_| "ese commit no existe".to_owned())?
    .trim()
    .to_owned();
    if trailer_of(repo, &sha).as_deref() != Some(id) {
        return Err("ese commit no es de este arreglo".into());
    }
    if !is_ancestor(repo, &sha, "HEAD") {
        return Err("ese commit no está en tu rama actual".into());
    }
    if already_reverted(repo, &sha) {
        return Err("ya estaba deshecho".into());
    }
    if !dirty(repo)?.is_empty() {
        return Err("tu árbol tiene cambios sin commitear".into());
    }
    if let Err(e) = ok(repo, &["revert", "--no-edit", "--no-commit", &sha]) {
        run(repo, &["revert", "--abort"]);
        return Err(format!("no se puede deshacer limpio: {e}"));
    }
    let message = format!("Autoarreglo: deshacer {id}\n\n{reason}\n\n{REVERT_TRAILER}: {sha}\n");
    let mut args: Vec<String> = identity(repo);
    args.extend(
        ["commit", "-q", "--no-verify", "-m", &message]
            .iter()
            .map(|s| (*s).to_owned()),
    );
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = ok(repo, &refs) {
        run(repo, &["revert", "--abort"]);
        return Err(e);
    }
    head(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_lines() {
        let stats = parse_numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n");
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].added, 3);
        assert!(stats[1].binary);
    }

    #[test]
    fn never_deletes_a_branch_that_is_not_its_own() {
        let dir = std::env::temp_dir();
        assert!(worktree_remove(&dir, &dir.join("nada-aa"), Some("main")).is_err());
    }
}
