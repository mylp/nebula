//! SESSION PRS — the pull requests one session made or took on, kept on
//! its row so they can be found from its card, whichever repositories they
//! are in and whatever checkout the session runs in.
//!
//! Two ways in: a Claude session's `gh pr create` (the `PostToolUse` hook
//! carries the command and what it printed — [`created_prs`]), and
//! `nebula pr add <url>` for a pull request the session took on rather
//! than opened. Unlike REPORTED PRS, which are display links on a
//! checkout, these belong to the session: every card sharing one checkout
//! keeps its own list.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use nebula_core::github_url::github_pr_url;
use nebula_core::AgentId;

use crate::registry::Daemon;

/// The pull requests a `PostToolUse` payload says were just created: a
/// Bash call whose command ran `gh pr create`, and each line of what it
/// printed to stdout that is a pull request URL and nothing else — how `gh`
/// reports one it made. A line that only mentions a URL (a search through
/// code that names `gh pr create`, a listing) is not a creation; nor is
/// another tool, another command, or a create that printed no URL.
pub fn created_prs(
    tool_name: Option<&str>,
    tool_input: Option<&serde_json::Value>,
    tool_response: Option<&serde_json::Value>,
) -> Vec<String> {
    if tool_name != Some("Bash") {
        return Vec::new();
    }
    let command = tool_input
        .and_then(|input| input.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    if !command.contains("gh pr create") {
        return Vec::new();
    }
    let stdout = match tool_response {
        Some(serde_json::Value::String(s)) => s.as_str(),
        Some(value) => value
            .get("stdout")
            .and_then(|s| s.as_str())
            .unwrap_or_default(),
        None => return Vec::new(),
    };
    let mut urls = Vec::new();
    for line in stdout.lines().map(str::trim) {
        if let Some(url) = github_pr_url(line).filter(|url| url == line) {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

impl Daemon {
    /// `nebula pr add`, and a capture: put `url` on the row's SESSION PRS
    /// and push the row to every subscriber when it changed.
    pub fn add_session_pr(self: &Arc<Self>, id: &AgentId, url: &str) -> Result<()> {
        let Some(url) = github_pr_url(url.trim()) else {
            bail!("not a GitHub pull request URL: {url}");
        };
        let changed = self
            .store
            .add_agent_pr(id, &url)?
            .context("agent not found")?;
        if changed {
            self.try_broadcast_agent(id);
        }
        Ok(())
    }

    /// `nebula pr remove`: take `url` off the row's SESSION PRS. A PR
    /// SESSION's own pull request stays: it is the session's launch scope,
    /// not an entry on the list.
    pub fn remove_session_pr(self: &Arc<Self>, id: &AgentId, url: &str) -> Result<()> {
        let Some(url) = github_pr_url(url.trim()) else {
            bail!("not a GitHub pull request URL: {url}");
        };
        let own = self.store.agent_pr_url(id)?;
        if own.as_deref().and_then(github_pr_url).as_deref() == Some(url.as_str()) {
            bail!("{url} is the pull request this session was started for, so it stays");
        }
        if !self
            .store
            .remove_agent_pr(id, &url)?
            .context("agent not found")?
        {
            bail!("{url} is not on this session's list");
        }
        self.try_broadcast_agent(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn created(command: &str, response: serde_json::Value) -> Vec<String> {
        created_prs(
            Some("Bash"),
            Some(&json!({ "command": command })),
            Some(&response),
        )
    }

    #[test]
    fn a_gh_pr_create_names_the_pull_requests_it_printed() {
        assert_eq!(
            created(
                "cd repos/api && gh pr create --title 'x' --body \"$(cat body.md)\"",
                json!({"stdout": "https://github.com/o/api/pull/42\n", "stderr": "", "interrupted": false}),
            ),
            ["https://github.com/o/api/pull/42"]
        );
        // Two creates in one call name both, once each.
        assert_eq!(
            created(
                "gh pr create -R o/api && gh pr create -R o/web && gh pr create -R o/api",
                json!({"stdout": "https://github.com/o/api/pull/42\n  https://github.com/o/web/pull/9  \nhttps://github.com/o/api/pull/42\n"}),
            ),
            [
                "https://github.com/o/api/pull/42",
                "https://github.com/o/web/pull/9"
            ]
        );
        // A response shaped as bare text still counts.
        assert_eq!(
            created("gh pr create", json!("https://github.com/o/api/pull/3")),
            ["https://github.com/o/api/pull/3"]
        );
    }

    /// `nebula pr add` / `remove` through the daemon: the URL is made
    /// canonical, the row is pushed to subscribers once per change, and
    /// what isn't a pull request — or isn't listed — is refused.
    #[tokio::test]
    async fn add_and_remove_push_the_row_and_refuse_what_they_cannot_do() {
        use nebula_core::{Entity, ServerEvent};
        let (daemon, id) = crate::prompt_history::tests::daemon_with_one_agent();
        let mut events = daemon.events.subscribe();
        let pushed =
            |events: &mut tokio::sync::broadcast::Receiver<ServerEvent>| match events.try_recv() {
                Ok(ServerEvent::EntityUpserted {
                    entity: Entity::Agent(a),
                }) => Some(a.prs),
                _ => None,
            };

        daemon
            .add_session_pr(&id, " https://github.com/o/api/pull/42/files ")
            .unwrap();
        assert_eq!(
            pushed(&mut events),
            Some(vec!["https://github.com/o/api/pull/42".to_string()])
        );
        daemon
            .add_session_pr(&id, "https://github.com/o/api/pull/42")
            .unwrap();
        assert_eq!(pushed(&mut events), None, "already listed: no push");

        assert!(daemon
            .add_session_pr(&id, "https://github.com/o/api/issues/1")
            .is_err());
        assert!(daemon
            .add_session_pr(&AgentId("ghost".into()), "https://github.com/o/api/pull/1")
            .is_err());
        assert!(daemon
            .remove_session_pr(&id, "https://github.com/o/api/pull/7")
            .is_err());

        daemon
            .remove_session_pr(&id, "https://github.com/o/api/pull/42")
            .unwrap();
        assert_eq!(pushed(&mut events), Some(vec![]));

        // A PR SESSION's own pull request is listed but stays.
        let own = "https://github.com/o/api/pull/9";
        let mut row = daemon.store.get_agent(&id).unwrap().unwrap();
        row.id = AgentId("pr-session".into());
        daemon
            .store
            .insert_agent_with_launch_context(&row, false, Some(own), None)
            .unwrap();
        let err = daemon
            .remove_session_pr(&row.id, own)
            .unwrap_err()
            .to_string();
        assert!(err.contains("stays"), "{err}");
        assert_eq!(daemon.store.get_agent(&row.id).unwrap().unwrap().prs, [own]);
    }

    /// A real Claude Code `PostToolUse` payload's tool fields (captured
    /// 2026-11-05), as the hook receiver hands them over.
    #[test]
    fn reads_claude_codes_own_bash_payload() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"tool_name":"Bash",
                "tool_input":{"command":"echo \"gh pr create (probe)\"; echo https://github.com/o/r/pull/12345","description":"Print probe text and sample PR URL"},
                "tool_response":{"stdout":"gh pr create (probe)\nhttps://github.com/o/r/pull/12345","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,
                                 "gitOperation":{"pr":{"number":12345,"url":"https://github.com/o/r/pull/12345","action":"created"}}}}"#,
        )
        .unwrap();
        assert_eq!(
            created_prs(
                payload["tool_name"].as_str(),
                Some(&payload["tool_input"]),
                Some(&payload["tool_response"]),
            ),
            ["https://github.com/o/r/pull/12345"]
        );
    }

    #[test]
    fn anything_but_a_gh_pr_create_names_nothing() {
        let printed = json!({"stdout": "https://github.com/o/api/pull/42"});
        assert!(created("gh pr view 42 --json url", printed.clone()).is_empty());
        assert!(created("gh pr create --dry-run", json!({"stdout": "would create"})).is_empty());
        assert!(created_prs(
            Some("Read"),
            Some(&json!({"command": "gh pr create"})),
            Some(&printed)
        )
        .is_empty());
        assert!(created_prs(Some("Bash"), None, Some(&printed)).is_empty());
        assert!(created_prs(
            Some("Bash"),
            Some(&json!({"command": "gh pr create"})),
            None
        )
        .is_empty());
        assert!(created(
            "gh pr create",
            json!({"stdout": "https://evil.example/o/r/pull/1 https://github.com/o/r/issues/2"})
        )
        .is_empty());
        // A search through code that names `gh pr create` prints lines that
        // mention a URL; none of them is a creation.
        assert!(created(
            "rg \"gh pr create\" crates/",
            json!({"stdout": "crates/x.rs:460: echo \"gh pr create\"; echo https://github.com/o/r/pull/12345\n"})
        )
        .is_empty());
        // Only stdout: git's `/pull/new/branch` hint and errors are stderr.
        assert!(created(
            "gh pr create",
            json!({"stdout": "", "stderr": "https://github.com/o/r/pull/7"})
        )
        .is_empty());
    }
}
