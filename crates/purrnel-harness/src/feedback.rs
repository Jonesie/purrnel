//! Feedback from the person to the developers: a GitHub issue on the project's repository.
//!
//! The UI's form normally opens GitHub's own "new issue" page, where GitHub handles the login and the
//! person presses Submit. If a GitHub token is configured (`PURRNEL_GITHUB_TOKEN` or
//! `~/.config/purrnel/github_token`) the harness posts the issue itself. Issues on a public repository
//! are public, and the form says so.

use crate::hub::Hub;
use crate::protocol::Out;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_TITLE: usize = 120;
const MAX_DETAILS: usize = 4000;
const COOLDOWN: Duration = Duration::from_secs(20);

#[derive(Debug, PartialEq)]
pub struct Issue {
    pub title: String,
    pub body: String,
    pub labels: Vec<&'static str>,
}

/// Tidies what was typed into an issue.
pub fn build_issue(kind: &str, title: &str, details: &str, version: &str) -> Result<Issue, String> {
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TITLE).collect();
    if title.is_empty() {
        return Err("Please give it a short title.".into());
    }
    let details: String = details.trim().chars().take(MAX_DETAILS).collect();
    let labels = match kind {
        "bug" => vec!["bug"],
        "idea" => vec!["enhancement"],
        "question" => vec!["question"],
        _ => vec![],
    };
    let mut body = String::new();
    if !details.is_empty() {
        body.push_str(&details);
        body.push_str("\n\n");
    }
    body.push_str(&format!("---\n_Sent from Purrnel {version}_"));
    Ok(Issue { title, body, labels })
}

/// `owner/name`, nothing that could change the URL it is put into.
pub fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    matches!(repo.split_once('/'), Some((a, b)) if ok(a) && ok(b))
}

/// Posts the issue and tells the UI how it went.
pub async fn submit(hub: &Arc<Hub>, kind: &str, title: &str, details: &str) {
    let result = post(hub, kind, title, details).await;
    hub.emit(match result {
        Ok(url) => Out::FeedbackResult { ok: true, url: Some(url), error: None },
        Err(e) => Out::FeedbackResult { ok: false, url: None, error: Some(e) },
    });
}

async fn post(hub: &Arc<Hub>, kind: &str, title: &str, details: &str) -> Result<String, String> {
    let token = hub.cfg.github_token.clone().ok_or("No GitHub token is set, so I can't post for you.")?;
    let repo = &hub.cfg.repo;
    if !valid_repo(repo) {
        return Err("The feedback repository isn't set up properly.".into());
    }
    let issue = build_issue(kind, title, details, env!("CARGO_PKG_VERSION"))?;
    {
        let mut last = hub.last_feedback.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < COOLDOWN) {
            return Err("Hold on a moment before sending another.".into());
        }
        *last = Some(Instant::now());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let url = format!("{}/repos/{}/issues", hub.cfg.github_api.trim_end_matches('/'), repo);
    let resp = client
        .post(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "purrnel")
        .json(&json!({"title": issue.title, "body": issue.body, "labels": issue.labels}))
        .send()
        .await
        .map_err(|e| if e.is_connect() || e.is_timeout() { "I can't reach GitHub right now.".to_string() } else { e.to_string() })?;
    match resp.status().as_u16() {
        201 => {
            let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            Ok(v["html_url"].as_str().unwrap_or_default().to_string())
        }
        401 | 403 => Err(format!("GitHub refused the token. It needs permission to create issues on {repo}.")),
        404 => Err(format!("GitHub can't find {repo}, or the token can't see it.")),
        410 => Err(format!("Issues are turned off on {repo}.")),
        s => Err(format!("GitHub said {s}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_issue() {
        let i = build_issue("bug", "  The dog   is  huge ", "  It filled the room.\n", "0.1.0").unwrap();
        assert_eq!(i.title, "The dog is huge");
        assert_eq!(i.labels, vec!["bug"]);
        assert!(i.body.starts_with("It filled the room.\n\n---\n"));
        assert!(i.body.ends_with("Purrnel 0.1.0_"));
        assert_eq!(build_issue("idea", "x", "", "1").unwrap().labels, vec!["enhancement"]);
        assert!(build_issue("other", "x", "", "1").unwrap().labels.is_empty());
    }

    #[test]
    fn needs_a_title_and_limits_length() {
        assert!(build_issue("bug", "   ", "details", "1").is_err());
        assert_eq!(build_issue("bug", &"t".repeat(500), "", "1").unwrap().title.len(), MAX_TITLE);
        let big = build_issue("bug", "t", &"d".repeat(9000), "1").unwrap();
        assert!(big.body.len() < MAX_DETAILS + 100);
    }

    #[test]
    fn only_sane_repo_names_are_used() {
        assert!(valid_repo("Jonesie/purrnel"));
        assert!(valid_repo("a-b/c_d.e"));
        assert!(!valid_repo("Jonesie"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("a/../b"));
        assert!(!valid_repo("a/b?x=1"));
        assert!(!valid_repo("/b"));
    }
}
