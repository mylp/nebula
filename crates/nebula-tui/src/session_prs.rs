//! The SESSION PRS window: `w` on a card lists the pull requests that
//! session registered (a `gh pr create` the hook saw, or `nebula pr add`),
//! then those of every session it spawned, under each one's name. Each
//! row is fetched by URL, so a card's pull requests can span repos that
//! have no project in nebula. Enter reads one's whole diff in the diff
//! modal, and Esc there comes back here.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};
use nebula_core::{Agent, AgentId, ClientRequest};
use ratatui::layout::Rect;

use crate::app::{App, Overlay, SessionRow};

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A session's name; `spawned` for any session below the card's own.
    Session {
        name: String,
        spawned: bool,
    },
    Pr {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionPrsView {
    /// The card's session name, for the title.
    pub name: String,
    pub rows: Vec<Row>,
    /// Index into `rows`; always a `Row::Pr`.
    pub selected: usize,
    pub area: Rect,
}

impl SessionPrsView {
    pub fn selected_url(&self) -> Option<&str> {
        match self.rows.get(self.selected) {
            Some(Row::Pr { url }) => Some(url),
            _ => None,
        }
    }

    pub fn has(&self, url: &str) -> bool {
        self.urls().any(|u| u == url)
    }

    fn urls(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().filter_map(|row| match row {
            Row::Pr { url } => Some(url.as_str()),
            Row::Session { .. } => None,
        })
    }

    /// Step `by` pull-request rows, stopping at either end.
    fn step(&mut self, by: i64) {
        let prs: Vec<usize> = (0..self.rows.len())
            .filter(|&i| matches!(self.rows[i], Row::Pr { .. }))
            .collect();
        let Some(at) = prs.iter().position(|&i| i == self.selected) else {
            return;
        };
        let to = (at as i64 + by).clamp(0, prs.len() as i64 - 1) as usize;
        self.selected = prs[to];
    }
}

/// `root`'s pull requests, then each session it spawned — and they spawned
/// — depth first, oldest first (ids are ULIDs, so they sort by creation).
/// A session with none of its own adds no heading, though its children
/// still show.
pub fn rows(agents: &[Agent], root: &AgentId) -> Vec<Row> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(agent) = agents.iter().find(|a| a.id == id) else {
            continue;
        };
        if !agent.prs.is_empty() {
            out.push(Row::Session {
                name: agent.name.clone(),
                spawned: &id != root,
            });
            out.extend(agent.prs.iter().map(|url| Row::Pr { url: url.clone() }));
        }
        let mut children: Vec<AgentId> = agents
            .iter()
            .filter(|a| a.spawned_by.as_ref() == Some(&id))
            .map(|a| a.id.clone())
            .collect();
        // Newest pushed first, so the oldest pops next.
        children.sort_by(|a, b| b.0.cmp(&a.0));
        stack.extend(children);
    }
    out
}

/// What `w` says on a card with nothing registered under it.
pub const NO_PRS: &str =
    "no pull requests on this session yet — a gh pr create in it, or nebula pr add <url>, adds one";

/// `w`: open the window on the session under the cursor, and ask GitHub
/// for the title and state of every row not already known.
pub fn open(app: &mut App) {
    let Some(SessionRow::Agent(agent)) = app.selected_session_row() else {
        app.chrome.flash = Some("move onto a session card to list its pull requests".into());
        app.chrome.dirty = true;
        return;
    };
    let rows = rows(&app.tree.agents, &agent.id);
    let Some(selected) = rows.iter().position(|r| matches!(r, Row::Pr { .. })) else {
        app.chrome.flash = Some(NO_PRS.into());
        app.chrome.dirty = true;
        return;
    };
    let view = SessionPrsView {
        name: agent.name.clone(),
        rows,
        selected,
        area: Rect::default(),
    };
    fetch_details(app, &view);
    app.modals.session_prs_back = None;
    app.modals.overlay = Some(Overlay::SessionPrs(view));
    app.chrome.dirty = true;
}

/// One `gh pr view <url>` per row with no fresh detail, landing in the
/// pane's own `pr_detail` cache. Reopening the window retries the ones
/// that failed last time.
fn fetch_details(app: &mut App, view: &SessionPrsView) {
    let Some(tx) = app.github.pr_detail_tx.clone() else {
        return; // tests: the loop installs it at startup
    };
    for url in view.urls() {
        let fresh =
            app.github.pr_detail.contains_key(url) && !app.github.pr_detail_stale.contains(url);
        if fresh || app.github.pr_detail_inflight.contains(url) {
            continue;
        }
        app.github.pr_detail_failed.remove(url);
        app.github.pr_detail_inflight.insert(url.to_string());
        let (tx, url) = (tx.clone(), url.to_string());
        tokio::spawn(async move {
            let detail = crate::pull_request::detail(None, &url).await;
            let _ = tx.send((url, detail));
        });
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent, out: &mut Vec<ClientRequest>) {
    let Some(Overlay::SessionPrs(view)) = &mut app.modals.overlay else {
        return;
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.modals.overlay = None,
        KeyCode::Down | KeyCode::Char('j') => view.step(1),
        KeyCode::Up | KeyCode::Char('k') => view.step(-1),
        KeyCode::Enter | KeyCode::Char('g') => {
            let back = view.clone();
            if let Some(url) = back.selected_url().map(str::to_string) {
                let title = label(app, &url);
                app.modals.session_prs_back = Some(back);
                crate::event_loop::request_pr_diff_for(app, number(&url), url, title);
            }
        }
        KeyCode::Char('o') => {
            if let Some(url) = view.selected_url().map(str::to_string) {
                crate::event_loop::open_link(app, &url, out);
            }
        }
        _ => return,
    }
    app.chrome.dirty = true;
}

/// The window to put back when the diff for `url` closes — only if that
/// diff was opened from it.
pub fn back_from_diff(app: &mut App, url: Option<&str>) -> Option<Overlay> {
    let back = app.modals.session_prs_back.take()?;
    url.filter(|u| back.has(u))?;
    Some(Overlay::SessionPrs(back))
}

/// The pull request number at the end of a canonical PR URL.
pub fn number(url: &str) -> u64 {
    url.rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// `repo#42`: the repo name and number, which is what tells rows apart
/// when a card's pull requests span repos.
pub fn repo_number(url: &str) -> String {
    let mut parts = url.rsplit('/');
    let n = parts.next().unwrap_or_default();
    let _pull = parts.next();
    let repo = parts.next().unwrap_or_default();
    format!("{repo}#{n}")
}

/// `#42 title` once GitHub has answered, `#42` until then.
fn label(app: &App, url: &str) -> String {
    let title = app
        .github
        .pr_detail
        .get(url)
        .map_or("", |d| d.title.as_str());
    crate::pull_request::numbered_label(number(url), title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, name: &str, parent: Option<&str>, prs: &[&str]) -> Agent {
        Agent {
            id: AgentId(id.into()),
            worktree_id: nebula_core::WorktreeId("w".into()),
            name: name.into(),
            status: nebula_core::AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            unseen: false,
            kind: nebula_core::AgentKind::Claude,
            custom_harness: None,
            model: None,
            effort: None,
            session_id: None,
            cloud_session_id: None,
            sort_order: 0,
            status_changed_at: 0,
            alive: true,
            issue_url: None,
            role: nebula_core::AgentRole::Worker,
            recent_prompts: Vec::new(),
            prs: prs.iter().map(|s| s.to_string()).collect(),
            spawned_by: parent.map(|p| AgentId(p.into())),
        }
    }

    const A: &str = "https://github.com/o/bjs/pull/1";
    const B: &str = "https://github.com/o/go-api/pull/2";
    const C: &str = "https://github.com/o/core-api/pull/3";

    #[test]
    fn a_card_lists_its_own_prs_then_its_spawns_oldest_first_through_grandchildren() {
        let agents = vec![
            agent("01A", "Golf", None, &[A]),
            agent("01D", "Late child", Some("01A"), &[B]),
            agent("01B", "Quiet child", Some("01A"), &[]),
            agent("01E", "Grandchild", Some("01B"), &[C]),
            agent("01C", "Unrelated", None, &["https://github.com/o/x/pull/9"]),
        ];
        let got = rows(&agents, &AgentId("01A".into()));
        let session = |name: &str, spawned| Row::Session {
            name: name.into(),
            spawned,
        };
        let pr = |url: &str| Row::Pr { url: url.into() };
        assert_eq!(
            got,
            vec![
                session("Golf", false),
                pr(A),
                session("Grandchild", true),
                pr(C),
                session("Late child", true),
                pr(B),
            ]
        );
    }

    #[test]
    fn a_spawned_card_opened_on_its_own_is_the_top_of_its_list() {
        let agents = vec![
            agent("root", "Golf", None, &[A]),
            agent("kid", "Grading", Some("root"), &[B]),
        ];
        let got = rows(&agents, &AgentId("kid".into()));
        assert_eq!(
            got[0],
            Row::Session {
                name: "Grading".into(),
                spawned: false
            }
        );
    }

    #[test]
    fn the_cursor_walks_only_pull_request_rows_and_stops_at_the_ends() {
        let agents = vec![
            agent("root", "Golf", None, &[A]),
            agent("kid", "Grading", Some("root"), &[B, C]),
        ];
        let rows = rows(&agents, &AgentId("root".into()));
        let mut view = SessionPrsView {
            rows,
            selected: 1,
            ..Default::default()
        };
        view.step(-1);
        assert_eq!(view.selected_url(), Some(A));
        view.step(1);
        assert_eq!(view.selected_url(), Some(B), "the heading is stepped over");
        view.step(5);
        assert_eq!(view.selected_url(), Some(C));
    }

    #[test]
    fn rows_name_the_repo_and_number() {
        assert_eq!(repo_number(B), "go-api#2");
        assert_eq!(number(C), 3);
    }
}
