//! Terminal output helpers: aligned tables, humanized timestamps, ~-paths.

use jiff::Timestamp;

use asm_core::fmt::human_bytes;
use asm_core::model::{Project, Session, SessionLocation, SessionStatus};

pub fn session_table(sessions: &[Session]) {
    let rows: Vec<Row> = sessions.iter().map(|session| Row { session, depth: 0, below: 0, parent: None }).collect();
    session_rows(&rows, false);
}

/// One line of `asm list`: a session, how deep it sits under its parent,
/// and how many sessions are hidden under it.
pub struct Row<'a> {
    pub session: &'a Session,
    pub depth: usize,
    pub below: usize,
    /// The id its parent goes by, if it has one: shown in the PARENT column.
    pub parent: Option<String>,
}

/// The session table, with children indented under their parent and named
/// by it (`with_parent`), or with a `+N` after the status of a session that
/// has children that are not shown.
pub fn session_rows(rows: &[Row], with_parent: bool) {
    let (headers, cells) = session_cells(rows, with_parent);
    print_table(&headers, &cells);
}

fn session_cells(rows: &[Row], with_parent: bool) -> (Vec<&'static str>, Vec<Vec<String>>) {
    let cells = rows
        .iter()
        .map(|Row { session: s, depth, below, parent }| {
            let title = truncate(s.title.as_deref().unwrap_or("(untitled)"), 48);
            let mut row = vec![s.handle.agent.to_string(), s.short_id().to_string()];
            if with_parent {
                row.push(parent.clone().unwrap_or_default());
            }
            row.extend([
                if *depth == 0 { title } else { format!("{}↳ {title}", "  ".repeat(depth - 1)) },
                home_relative(&s.project_root.display().to_string()),
                s.git_branch.clone().unwrap_or_default(),
                s.updated.map(humanize).unwrap_or_default(),
                s.size_bytes.map(human_bytes).unwrap_or_default(),
                match below {
                    0 => status_label(&s.status).to_string(),
                    n => format!("{} +{n}", status_label(&s.status)),
                },
            ]);
            row
        })
        .collect();
    let mut headers = vec!["AGENT", "ID"];
    if with_parent {
        headers.push("PARENT");
    }
    headers.extend(["TITLE", "PROJECT", "BRANCH", "UPDATED", "SIZE", "STATUS"]);
    (headers, cells)
}

/// This machine's sessions and the hub's, one row per session, grouped by
/// the project both machines agree on.
pub fn remote_table(rows: &[asm_core::hub::actions::Row]) {
    use asm_core::hub::actions::RowState;
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let state = match r.state {
                // A backup the agent cannot restore is the one case the
                // shared wording does not cover: there is nothing to pull.
                RowState::Remote if !r.restorable => "On hub (backup)",
                _ => r.label,
            };
            vec![
                truncate(&home_relative(&r.project), 40),
                r.agent.to_string(),
                r.short_id.clone(),
                truncate(r.title.as_deref().unwrap_or("(untitled)"), 40),
                r.machine.clone(),
                r.updated.map(humanize).unwrap_or_default(),
                state.to_string(),
            ]
        })
        .collect();
    print_table(&["PROJECT", "AGENT", "ID", "TITLE", "MACHINE", "UPDATED", "STATE"], &rows);
}

pub fn machine_table(machines: &[asm_core::hub::store::Machine], this: Option<&str>) {
    let rows: Vec<Vec<String>> = machines
        .iter()
        .map(|m| {
            vec![
                if Some(m.id.as_str()) == this { format!("{} (this one)", m.name) } else { m.name.clone() },
                m.id.clone(),
                humanize(m.joined),
                m.last_seen.map(humanize).unwrap_or_default(),
            ]
        })
        .collect();
    print_table(&["MACHINE", "ID", "JOINED", "LAST SEEN"], &rows);
}

pub fn project_table(projects: &[Project]) {
    let rows: Vec<Vec<String>> = projects
        .iter()
        .map(|p| {
            let active = p.active_worktrees().len();
            vec![
                home_relative(&p.root.display().to_string()),
                p.agents.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", "),
                p.session_count.to_string(),
                human_bytes(p.size_bytes),
                // Only interesting once a repository has more than one
                // checkout; a plain directory has nothing to say here.
                match p.worktrees.len() {
                    0 | 1 => String::new(),
                    total => format!("{active}/{total}"),
                },
                p.last_updated.map(humanize).unwrap_or_default(),
            ]
        })
        .collect();
    print_table(&["PROJECT", "AGENTS", "SESSIONS", "SIZE", "WORKTREES", "UPDATED"], &rows);
}

/// Worktrees of a project, indented beneath it.
pub fn project_worktrees(project: &Project) {
    if project.worktrees.len() <= 1 {
        return;
    }
    for worktree in &project.worktrees {
        let branch = worktree.branch.as_deref().unwrap_or("detached");
        let marker = if worktree.is_main { "main" } else { "linked" };
        println!(
            "    {:<44} {:<10} {:<7} {} session(s)",
            home_relative(&worktree.path.display().to_string()),
            branch,
            marker,
            worktree.session_count
        );
    }
}

pub fn session_card(s: &Session) {
    let mut lines: Vec<(&str, String)> = vec![
        ("id", s.handle.native_id.clone()),
        ("agent", s.handle.agent.to_string()),
        ("title", s.title.clone().unwrap_or_else(|| "(untitled)".into())),
    ];
    if let Some(slug) = &s.slug {
        lines.push(("slug", slug.clone()));
    }
    lines.push(("project", home_relative(&s.project_root.display().to_string())));
    if let Some(branch) = &s.git_branch {
        lines.push(("branch", branch.clone()));
    }
    if let Some(model) = &s.model {
        lines.push(("model", model.clone()));
    }
    if let Some(created) = s.created {
        lines.push(("created", format!("{created}  ({})", humanize(created))));
    }
    if let Some(updated) = s.updated {
        lines.push(("updated", format!("{updated}  ({})", humanize(updated))));
    }
    lines.push(("status", status_label(&s.status).to_string()));
    if let Some(parent) = &s.parent {
        lines.push(("parent", parent.clone()));
    }
    if let Some(cost) = s.usage.cost_usd {
        lines.push(("last cost", format!("${cost:.2}")));
    }
    if let (Some(input), Some(output)) = (s.usage.input_tokens, s.usage.output_tokens) {
        lines.push(("last tokens", format!("{input} in / {output} out")));
    }
    if let Some(version) = &s.agent_version {
        lines.push(("agent version", version.clone()));
    }
    if let Some(size) = s.size_bytes {
        lines.push(("size", human_bytes(size)));
    }
    match &s.handle.location {
        SessionLocation::JsonlFile { path } => {
            lines.push(("transcript", home_relative(&path.display().to_string())));
        }
        SessionLocation::SqliteRow { db, .. } => {
            lines.push(("store", home_relative(&db.display().to_string())));
        }
        SessionLocation::Archive { dir } => {
            lines.push(("archive", home_relative(&dir.display().to_string())));
        }
    }

    let width = lines.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, value) in lines {
        println!("{key:width$}  {value}");
    }
}

fn status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Live { .. } => "live",
        SessionStatus::Idle => "idle",
        SessionStatus::Archived => "archived",
    }
}

pub fn humanize(ts: Timestamp) -> String {
    let seconds = Timestamp::now().as_second() - ts.as_second();
    match seconds {
        i64::MIN..0 => "in the future".to_string(),
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        86_400..2_592_000 => format!("{}d ago", seconds / 86_400),
        // RFC 3339 starts with YYYY-MM-DD.
        _ => ts.to_string().chars().take(10).collect(),
    }
}

/// `~/x` for a path under home, whether it arrives absolute or in the
/// hub's `${HOME}/x` form. Matched by path component, so a sibling such as
/// `/home/sami-old` is not mistaken for `/home/sami`.
fn home_relative(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("${HOME}")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("~{rest}");
    }
    if let Ok(home) = etcetera::home_dir()
        && let Ok(rest) = std::path::Path::new(path).strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.to_string()
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let render = |cells: &[String]| {
        let line: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<width$}", width = widths[i]))
            .collect();
        println!("{}", line.join("  ").trim_end());
    };
    render(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>());
    for row in rows {
        render(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asm_core::model::{AgentKind, SessionLocation, SessionRef};

    fn session(id: &str, title: &str, parent: Option<&str>) -> Session {
        Session {
            handle: SessionRef {
                agent: AgentKind::OpenCode,
                native_id: id.into(),
                location: SessionLocation::JsonlFile { path: "/x".into() },
            },
            title: Some(title.into()),
            slug: None,
            project_root: "/w/app".into(),
            git_branch: None,
            created: None,
            updated: None,
            model: None,
            usage: Default::default(),
            status: SessionStatus::Idle,
            parent: parent.map(str::to_string),
            agent_version: None,
            size_bytes: None,
        }
    }

    /// Without `--include-children` the table is the one it always was,
    /// except that a session with children says how many are hidden.
    #[test]
    fn a_session_with_hidden_children_ends_its_row_with_how_many() {
        let root = session("ses_root00000000", "Plan", None);
        let plain = session("ses_plain0000000", "Other", None);
        let rows = [Row { session: &root, depth: 0, below: 3, parent: None }, Row { session: &plain, depth: 0, below: 0, parent: None }];
        let (headers, cells) = session_cells(&rows, false);
        assert_eq!(headers, ["AGENT", "ID", "TITLE", "PROJECT", "BRANCH", "UPDATED", "SIZE", "STATUS"]);
        assert_eq!(cells[0].last().unwrap(), "idle +3");
        assert_eq!(cells[1].last().unwrap(), "idle", "no marker, no change");
        assert_eq!(cells[1].len(), headers.len());
    }

    /// With them, each child is indented under its parent and names it.
    #[test]
    fn children_are_indented_and_name_their_parent() {
        let root = session("ses_root00000000", "Plan", None);
        let kid = session("ses_kid000000000", "Review", Some("ses_root00000000"));
        let grand = session("ses_gran00000000", "Nested", Some("ses_kid000000000"));
        let rows = [
            Row { session: &root, depth: 0, below: 0, parent: None },
            Row { session: &kid, depth: 1, below: 0, parent: Some("ses_root".into()) },
            Row { session: &grand, depth: 2, below: 0, parent: Some("ses_kid0".into()) },
        ];
        let (headers, cells) = session_cells(&rows, true);
        assert_eq!(&headers[..4], ["AGENT", "ID", "PARENT", "TITLE"]);
        let column = |n: usize| cells.iter().map(|r| r[n].as_str()).collect::<Vec<_>>();
        assert_eq!(column(2), ["", "ses_root", "ses_kid0"]);
        assert_eq!(column(3), ["Plan", "↳ Review", "  ↳ Nested"]);
    }
}
