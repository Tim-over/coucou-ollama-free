// "Throw Mochi at a window" lands here: a URL comes in, Coucou gathers what the
// page or repo is about and writes it as a Markdown file in the inbox. From
// there it rides the normal dropped-file pipeline — the chat attaches it and the
// chosen engine (Claude or a local Ollama model) analyses it from the user's
// prompt. Coucou does the fetching, so it works even with an offline model.

use serde::Serialize;
use serde_json::Value;

use crate::files::{self, DroppedFile};
use crate::secrets;

/// Context is capped so a huge README or page still fits a model's window.
const MAX_CONTEXT: usize = 40_000;
/// Files listed from a repo tree.
const MAX_TREE: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scanned {
    pub file: DroppedFile,
    /// "github" or "web" — lets the island word things.
    pub kind: String,
    /// A short human title (repo full name, or the page <title>).
    pub title: String,
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Coucou")
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())
}

pub async fn scan_url(url: &str) -> Result<Scanned, String> {
    let url = normalise(url)?;
    if let Some((owner, repo)) = github_repo(&url) {
        gather_github(&owner, &repo).await
    } else {
        gather_page(&url).await
    }
}

/// Adds https:// when missing and rejects anything that isn't a web address.
fn normalise(raw: &str) -> Result<String, String> {
    let s = raw.trim().trim_end_matches(['.', ',', ')']);
    let s = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    // Must have a host with a dot.
    let host = s.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("");
    if !host.contains('.') || host.len() < 3 {
        return Err("That doesn't look like a web address.".into());
    }
    let bare = host.split(':').next().unwrap_or(host);
    if bare == "localhost" || bare.ends_with(".localhost") || bare.starts_with("127.") || bare == "0.0.0.0" {
        return Err("That's a local address, not a web page.".into());
    }
    Ok(s)
}

/// owner/repo for a github.com project URL (not a gist, not a sub-page only).
fn github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("https://www.github.com/"))?;
    let mut parts = rest.split(['/', '?', '#']).filter(|s| !s.is_empty());
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    // Reserved first segments that aren't owners.
    if ["orgs", "features", "about", "pricing", "marketplace", "settings", "notifications", "sponsors"]
        .contains(&owner.as_str())
        || owner.is_empty()
        || repo.is_empty()
    {
        return None;
    }
    Some((owner, repo))
}

// ── GitHub ──────────────────────────────────────────────────────────────────

async fn gather_github(owner: &str, repo: &str) -> Result<Scanned, String> {
    let client = http()?;
    let token = secrets::get("github-token");
    let get = |path: String, raw: bool| {
        let client = client.clone();
        let token = token.clone();
        async move {
            let mut req = client
                .get(format!("https://api.github.com/{path}"))
                .header("Accept", if raw { "application/vnd.github.raw+json" } else { "application/vnd.github+json" });
            if let Some(t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            req.send().await
        }
    };

    let meta_resp = get(format!("repos/{owner}/{repo}"), false)
        .await
        .map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
    if meta_resp.status().as_u16() == 404 {
        return Err(format!("{owner}/{repo} not found (or private without the right token)."));
    }
    if !meta_resp.status().is_success() {
        return Err(format!("GitHub returned {} for {owner}/{repo}.", meta_resp.status()));
    }
    let meta: Value = meta_resp.json().await.map_err(|e| e.to_string())?;

    let full = meta["full_name"].as_str().unwrap_or(&format!("{owner}/{repo}")).to_string();
    let desc = meta["description"].as_str().unwrap_or("").to_string();
    let lang = meta["language"].as_str().unwrap_or("").to_string();
    let stars = meta["stargazers_count"].as_i64().unwrap_or(0);
    let forks = meta["forks_count"].as_i64().unwrap_or(0);
    let open_issues = meta["open_issues_count"].as_i64().unwrap_or(0);
    let branch = meta["default_branch"].as_str().unwrap_or("main").to_string();
    let license = meta["license"]["name"].as_str().unwrap_or("").to_string();
    let homepage = meta["homepage"].as_str().unwrap_or("").to_string();
    let topics: Vec<String> = meta["topics"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    let updated = meta["pushed_at"].as_str().unwrap_or("").to_string();

    let mut out = String::new();
    out.push_str(&format!("# GitHub repository: {full}\n\n"));
    out.push_str(&format!("URL: https://github.com/{owner}/{repo}\n"));
    if !desc.is_empty() { out.push_str(&format!("Description: {desc}\n")); }
    if !lang.is_empty() { out.push_str(&format!("Main language: {lang}\n")); }
    out.push_str(&format!("Stars: {stars} · Forks: {forks} · Open issues: {open_issues}\n"));
    if !license.is_empty() { out.push_str(&format!("License: {license}\n")); }
    if !homepage.is_empty() { out.push_str(&format!("Homepage: {homepage}\n")); }
    if !topics.is_empty() { out.push_str(&format!("Topics: {}\n", topics.join(", "))); }
    if !updated.is_empty() { out.push_str(&format!("Last push: {updated}\n")); }

    // Languages breakdown.
    if let Ok(r) = get(format!("repos/{owner}/{repo}/languages"), false).await {
        if let Ok(langs) = r.json::<Value>().await {
            if let Some(map) = langs.as_object() {
                if !map.is_empty() {
                    let list: Vec<String> = map.keys().cloned().collect();
                    out.push_str(&format!("Languages: {}\n", list.join(", ")));
                }
            }
        }
    }

    // File tree (names only, capped) — gives the model the project's shape.
    if let Ok(r) = get(format!("repos/{owner}/{repo}/git/trees/{branch}?recursive=1"), false).await {
        if let Ok(tree) = r.json::<Value>().await {
            if let Some(items) = tree["tree"].as_array() {
                let paths: Vec<&str> = items
                    .iter()
                    .filter(|i| i["type"].as_str() == Some("blob"))
                    .filter_map(|i| i["path"].as_str())
                    .take(MAX_TREE)
                    .collect();
                if !paths.is_empty() {
                    out.push_str(&format!("\n## File tree ({} shown)\n", paths.len()));
                    for p in paths {
                        out.push_str(&format!("- {p}\n"));
                    }
                }
            }
        }
    }

    // README (raw).
    if let Ok(r) = get(format!("repos/{owner}/{repo}/readme"), true).await {
        if r.status().is_success() {
            if let Ok(text) = r.text().await {
                let text = text.trim();
                if !text.is_empty() {
                    out.push_str("\n## README\n");
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
    }

    let out = clip(&out, MAX_CONTEXT);
    let name = format!("{}-{} (GitHub).md", owner, repo);
    let file = files::ingest_bytes(&name, out.as_bytes())?;
    Ok(Scanned { file, kind: "github".into(), title: full })
}

// ── Web page ────────────────────────────────────────────────────────────────

async fn gather_page(url: &str) -> Result<Scanned, String> {
    let resp = http()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Couldn't open the page: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("The page returned {}.", resp.status()));
    }
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !(ctype.is_empty() || ctype.contains("html") || ctype.contains("text") || ctype.contains("xml")) {
        return Err(format!("That link is {ctype}, not a web page I can read."));
    }
    let html = resp.text().await.map_err(|e| e.to_string())?;
    let title = extract_title(&html).unwrap_or_else(|| url.to_string());
    let text = html_to_text(&html);
    if text.trim().len() < 40 {
        return Err("I couldn't find readable text on that page.".into());
    }

    let mut out = format!("# Web page: {title}\n\nURL: {url}\n\n{}", text);
    out = clip(&out, MAX_CONTEXT);
    let name = format!("{} (page web).md", safe_name(&title));
    let file = files::ingest_bytes(&name, out.as_bytes())?;
    Ok(Scanned { file, kind: "web".into(), title })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title")?;
    let gt = lower[start..].find('>')? + start + 1;
    let end = lower[gt..].find("</title>")? + gt;
    let t = unescape(html[gt..end].trim());
    if t.is_empty() { None } else { Some(t) }
}

/// Strips scripts, styles, comments and tags, then collapses whitespace.
fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for (open, close) in [("<script", "</script>"), ("<style", "</style>"), ("<!--", "-->"), ("<svg", "</svg>"), ("<head", "</head>")] {
        loop {
            let lower = s.to_lowercase();
            let Some(a) = lower.find(open) else { break };
            let Some(rel) = lower[a..].find(close) else { s.truncate(a); break };
            s.replace_range(a..a + rel + close.len(), " ");
        }
    }
    // Block tags become line breaks so paragraphs survive.
    for b in ["</p>", "</div>", "</li>", "</h1>", "</h2>", "</h3>", "</h4>", "<br>", "<br/>", "<br />", "</tr>"] {
        s = s.replace(b, "\n");
        s = s.replace(&b.to_uppercase(), "\n");
    }
    // Drop every remaining tag.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = unescape(&out);
    // Collapse runs of blank lines and trailing spaces.
    let mut tidy = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 { continue; }
        } else {
            blanks = 0;
        }
        tidy.push_str(&line);
        tidy.push('\n');
    }
    tidy.trim().to_string()
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&rsquo;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
}

fn safe_name(title: &str) -> String {
    let t: String = title.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(60).collect();
    if t.trim().is_empty() { "page".into() } else { t.trim().to_string() }
}

fn clip(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    let end = text.char_indices().nth(budget).map(|(i, _)| i).unwrap_or(text.len());
    let mut out = text[..end].to_string();
    out.push_str("\n\n(Contenu tronqué pour tenir dans la fenêtre du modèle.)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_github_repos() {
        assert_eq!(github_repo("https://github.com/abcx/coucou"), Some(("abcx".into(), "coucou".into())));
        assert_eq!(github_repo("https://github.com/abcx/coucou/tree/main/src"), Some(("abcx".into(), "coucou".into())));
        assert_eq!(github_repo("https://github.com/orgs/anthropics"), None);
        assert_eq!(github_repo("https://example.com/a/b"), None);
    }

    #[test]
    fn normalises_urls() {
        assert_eq!(normalise("github.com/a/b").unwrap(), "https://github.com/a/b");
        assert_eq!(normalise(" https://x.io/p. ").unwrap(), "https://x.io/p");
        assert!(normalise("localhost").is_err());
        assert!(normalise("not a url").is_err());
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><title>Hello &amp; Bye</title><style>x{}</style></head><body><script>bad()</script><h1>Titre</h1><p>Premier.</p><p>Deuxième ligne.</p></body></html>";
        assert_eq!(extract_title(html).unwrap(), "Hello & Bye");
        let text = html_to_text(html);
        assert!(text.contains("Titre"));
        assert!(text.contains("Premier."));
        assert!(text.contains("Deuxième ligne."));
        assert!(!text.contains("bad()"));
        assert!(!text.to_lowercase().contains("<script"));
    }
}
