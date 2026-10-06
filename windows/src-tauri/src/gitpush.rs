// "Push" in the GitHub card: stage everything, commit, and push one local
// project to GitHub, using the token already stored in Coucou.
//
// Nothing is kept on disk that wasn't there before: the token is injected only
// into the one push command's URL argument (local to this machine), never
// written into the repo's git config, and it is stripped from anything shown
// back to the user.
//
// Git does the work — Coucou shells out to it rather than reimplementing it — so
// the user's own identity, hooks and .gitignore all apply as usual.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::secrets;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub branch: String,
    pub repo: String,
    /// Files committed this time (0 when there was nothing new to commit).
    pub committed: u32,
    /// True when a commit was created, false when the tree was already clean.
    pub did_commit: bool,
    pub summary: String,
}

/// Runs `git` in `dir`, returns (stdout, stderr, success). Never shows a console.
fn git(dir: &str, args: &[&str]) -> Result<(String, String, bool), String> {
    let git = find_git().ok_or_else(|| {
        "Git isn't installed (or not on PATH). Install it from https://git-scm.com, then try again.".to_string()
    })?;
    let out = Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    ))
}

fn find_git() -> Option<std::path::PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            let c = dir.join(format!("git{}", ext.to_lowercase()));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// What a folder is, git-wise — drives the push vs. create-repo choice.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoState {
    pub is_repo: bool,
    pub has_origin: bool,
    pub origin: String,
    /// A GitHub-legal repo name derived from the folder, for the create flow.
    pub suggested_name: String,
}

pub fn repo_state(dir: &str) -> Result<RepoState, String> {
    if !Path::new(dir).is_dir() {
        return Err("That folder doesn't exist.".into());
    }
    let is_repo = matches!(git(dir, &["rev-parse", "--is-inside-work-tree"])?, (o, _, true) if o == "true");
    let (origin, _, ok) = git(dir, &["remote", "get-url", "origin"])?;
    let has_origin = is_repo && ok && !origin.is_empty();
    let base = Path::new(dir).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(RepoState { is_repo, has_origin, origin: if has_origin { origin } else { String::new() }, suggested_name: sanitize_repo_name(&base) })
}

/// Turns a folder name into something GitHub accepts (letters, digits, -, _, .).
pub fn sanitize_repo_name(name: &str) -> String {
    let mut out: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(|c| c == '-' || c == '.').to_string();
    if out.is_empty() { "projet".into() } else { out }
}

/// Stage all, commit (if anything changed), and push `dir` to its origin.
pub fn push_project(dir: &str, message: &str) -> Result<PushResult, String> {
    if !Path::new(dir).is_dir() {
        return Err("That folder doesn't exist.".into());
    }

    // A real work tree, not just any folder.
    match git(dir, &["rev-parse", "--is-inside-work-tree"])? {
        (out, _, true) if out == "true" => {}
        _ => return Err("This folder isn't a Git repository (no .git). Open it in a terminal and run `git init` first.".into()),
    }

    let (origin, _, ok) = git(dir, &["remote", "get-url", "origin"])?;
    if !ok || origin.is_empty() {
        return Err("NO_ORIGIN".into());
    }

    let (changed, did_commit) = stage_and_commit(dir, message)?;
    let branch = current_branch(dir)?;
    let summary = do_push(dir, &branch, &origin)?;
    let repo = github_https(&origin).map(|(o, r)| format!("{o}/{r}")).unwrap_or(origin);
    Ok(PushResult { branch, repo, committed: changed, did_commit, summary })
}

/// Creates <owner>/<name> on GitHub (the API call is the caller's job), wires it
/// as origin, and pushes. `dir` may not be a repo yet — it gets `git init`.
pub fn create_and_push(dir: &str, message: &str, owner: &str, name: &str) -> Result<PushResult, String> {
    if !Path::new(dir).is_dir() {
        return Err("That folder doesn't exist.".into());
    }
    let is_repo = matches!(git(dir, &["rev-parse", "--is-inside-work-tree"])?, (o, _, true) if o == "true");
    if !is_repo {
        // -b main needs Git ≥ 2.28; fall back to a plain init for older ones.
        if !git(dir, &["init", "-b", "main"])?.2 {
            git(dir, &["init"])?;
        }
    }

    let (changed, did_commit) = stage_and_commit(dir, message)?;
    // Must have a commit now, or there's nothing to push.
    if current_branch(dir).is_err() {
        return Err("Nothing to push — the folder is empty.".into());
    }
    let mut branch = current_branch(dir)?;
    if branch == "master" {
        // New repos on GitHub default to main; rename so they line up.
        if git(dir, &["branch", "-M", "main"])?.2 {
            branch = "main".into();
        }
    }

    let clean = format!("https://github.com/{owner}/{name}.git");
    git(dir, &["remote", "remove", "origin"])?; // ignore "no origin"
    if !git(dir, &["remote", "add", "origin", &clean])?.2 {
        return Err("Could not set the origin remote.".into());
    }
    let summary = do_push(dir, &branch, &clean)?;
    Ok(PushResult {
        branch,
        repo: format!("{owner}/{name}"),
        committed: changed,
        did_commit,
        summary: format!("Created {owner}/{name} and {}", summary.chars().next().map(|c| c.to_lowercase().to_string() + &summary[c.len_utf8()..]).unwrap_or(summary)),
    })
}

fn current_branch(dir: &str) -> Result<String, String> {
    match git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])? {
        (b, _, true) if b == "HEAD" => Err("You're on a detached HEAD (no branch). Check out a branch first.".into()),
        (b, _, true) if !b.is_empty() => Ok(b),
        _ => Err("This repository has no commits yet.".into()),
    }
}

/// Stage everything and commit when the tree changed. Returns (files, committed).
fn stage_and_commit(dir: &str, message: &str) -> Result<(u32, bool), String> {
    git(dir, &["add", "-A"])?;
    let (status, _, _) = git(dir, &["status", "--porcelain"])?;
    let changed = status.lines().filter(|l| !l.trim().is_empty()).count() as u32;
    if changed == 0 {
        return Ok((0, false));
    }
    let msg = if message.trim().is_empty() { "Update from Coucou" } else { message.trim() };
    // Fall back to an identity only if the user hasn't set one, so a fresh Git
    // install doesn't block the commit.
    let mut args: Vec<String> = Vec::new();
    if git(dir, &["config", "user.name"])?.0.is_empty() {
        args.push("-c".into());
        args.push("user.name=Coucou".into());
    }
    if git(dir, &["config", "user.email"])?.0.is_empty() {
        args.push("-c".into());
        args.push("user.email=coucou@localhost".into());
    }
    args.extend(["commit".into(), "-m".into(), msg.into()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (_, err, ok) = git(dir, &refs)?;
    if !ok {
        return Err(format!("Commit failed: {}", sanitize(&err, "")));
    }
    Ok((changed, true))
}

/// Pushes HEAD to `origin`'s branch. For github.com the stored token
/// authenticates over HTTPS without touching the repo's saved remote; any other
/// host goes through the system's own git credentials.
fn do_push(dir: &str, branch: &str, origin: &str) -> Result<String, String> {
    let token = secrets::get("github-token");
    let (push_args, redact): (Vec<String>, String) = match (github_https(origin), &token) {
        (Some((owner, repo)), Some(tok)) => (
            vec![
                "push".into(),
                format!("https://x-access-token:{tok}@github.com/{owner}/{repo}.git"),
                format!("HEAD:{branch}"),
            ],
            tok.clone(),
        ),
        _ => (vec!["push".into(), "origin".into(), format!("HEAD:{branch}")], String::new()),
    };
    let refs: Vec<&str> = push_args.iter().map(String::as_str).collect();
    // credential.helper= disables any interactive prompt so a bad token fails
    // fast instead of hanging on a hidden dialog.
    let mut full = vec!["-c", "credential.helper="];
    full.extend(refs);
    let (out, err, ok) = git(dir, &full)?;
    if !ok {
        return Err(push_hint(&sanitize(&err, &redact)));
    }
    let repo = github_https(origin).map(|(o, r)| format!("{o}/{r}")).unwrap_or_else(|| origin.to_string());
    let detail = sanitize(&format!("{err}\n{out}"), &redact);
    if detail.contains("up-to-date") || detail.contains("up to date") {
        Ok(format!("{repo} is already up to date."))
    } else {
        Ok(format!("Pushed to {repo} ({branch})."))
    }
}

/// owner/repo from an https or ssh github.com remote, else None.
fn github_https(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest).trim_end_matches('/');
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Never let the token appear in a message shown to the user or the log.
fn sanitize(text: &str, token: &str) -> String {
    let mut t = text.to_string();
    if !token.is_empty() {
        t = t.replace(token, "***");
    }
    // Belt and braces: scrub any token embedded in an https URL.
    while let Some(start) = t.find("x-access-token:") {
        if let Some(at) = t[start..].find('@') {
            t.replace_range(start.."x-access-token:".len() + start, "x-access-token:");
            t.replace_range(start + "x-access-token:".len()..start + at, "***");
        } else {
            break;
        }
    }
    t
}

/// Turns git's push errors into something a non-expert can act on.
fn push_hint(err: &str) -> String {
    let low = err.to_lowercase();
    if low.contains("authentication") || low.contains("403") || low.contains("permission") || low.contains("denied") {
        return "GitHub refused the push: the token is wrong or lacks the 'repo' scope. Settings… → GitHub → paste a token with repo access.".into();
    }
    if low.contains("non-fast-forward") || low.contains("rejected") || low.contains("fetch first") {
        return "GitHub has commits you don't have locally. Pull them first (git pull), then push again.".into();
    }
    if low.contains("could not resolve host") || low.contains("unable to access") {
        return "Couldn't reach GitHub. Check your internet connection and try again.".into();
    }
    format!("Push failed: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_remotes() {
        assert_eq!(github_https("https://github.com/abcx/coucou.git"), Some(("abcx".into(), "coucou".into())));
        assert_eq!(github_https("git@github.com:abcx/coucou.git"), Some(("abcx".into(), "coucou".into())));
        assert_eq!(github_https("https://github.com/abcx/coucou"), Some(("abcx".into(), "coucou".into())));
        assert_eq!(github_https("https://gitlab.com/abcx/coucou.git"), None);
    }

    #[test]
    fn token_is_scrubbed() {
        let s = sanitize("fatal: https://x-access-token:ghp_SECRET@github.com/a/b.git failed; ghp_SECRET", "ghp_SECRET");
        assert!(!s.contains("ghp_SECRET"), "{s}");
        assert!(s.contains("***"));
    }
}
