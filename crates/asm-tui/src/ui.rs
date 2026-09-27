//! Rendering. Immediate mode: everything redraws each frame; the list and
//! preview only materialize their visible windows.
//!
//! Two rules the layout keeps: the footer is laid out before the panels, so
//! the keys are never the thing that gets clipped, and a hint line that
//! does not fit loses whole hints from the end rather than half a word.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap,
};

use asm_core::model::SessionStatus;

use crate::app::{App, Mode, PickerKind};
use crate::theme;
use crate::worker::PreviewKind;

pub fn draw(frame: &mut Frame, app: &App) {
    let filters = app.active_filters();
    // On a very short terminal the keys are worth more than another row of
    // sessions, so the footer keeps at least its hint line.
    let wanted = if filters.is_empty() { 2 } else { 3 };
    let footer_height = wanted.min(frame.area().height.saturating_sub(1)).max(1);
    let [main, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer_height)]).areas(frame.area());

    // The transcript is opt-in, and while it is closed the list has the
    // whole width for the columns that were being squeezed.
    if app.preview_open {
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(main);
        draw_main(frame, app, list_area);
        draw_preview(frame, app, preview_area);
    } else {
        draw_main(frame, app, main);
    }
    draw_footer(frame, app, footer, &filters);

    if app.help_open {
        draw_help(frame, app, frame.area());
    } else if app.picker.is_some() {
        draw_picker(frame, app, frame.area());
    } else if app.show_doctor {
        draw_doctor(frame, app, frame.area());
    } else if app.report.is_some() {
        draw_report(frame, app, frame.area());
    }
}

fn draw_main(frame: &mut Frame, app: &App, area: Rect) {
    if app.hits.is_some() {
        draw_hits(frame, app, area);
    } else {
        draw_list(frame, app, area);
    }
}

/// A centred box, at most `width` by `height`, that always leaves a margin.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(Span::styled(format!(" {} ", title.into()), theme::title(focused)))
}

/// What a batch could not do. Only drawn when something was skipped or
/// failed — a clean run says so in the status bar and stays out of the way.
fn draw_report(frame: &mut Frame, app: &App, area: Rect) {
    let Some((verb, report)) = app.report.as_ref() else { return };
    let problems = report.problems();
    let area = popup(area, 100, problems.len() as u16 + 4);
    frame.render_widget(Clear, area);

    let lines: Vec<Line> = problems
        .iter()
        .map(|line| {
            let style = if line.contains(": failed") {
                Style::default().fg(theme::BAD)
            } else {
                Style::default().fg(theme::WARN)
            };
            Line::styled(line.clone(), style)
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BAD))
                .title(format!(" {} — esc to close ", report.summary(verb))),
        ),
        area,
    );
}

fn draw_doctor(frame: &mut Frame, app: &App, area: Rect) {
    let area = popup(area, 100, app.doctor.len() as u16 + 4);
    frame.render_widget(Clear, area);

    let lines: Vec<Line> = app
        .doctor
        .iter()
        .map(|line| {
            let style = if line.contains('⚠') {
                Style::default().fg(theme::WARN)
            } else if line.starts_with("  ") {
                theme::dim()
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            Line::styled(line.clone(), style)
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::WARN))
                .title(" store health — any key to close "),
        ),
        area,
    );
}

/// Every key, grouped. The footer shows the handful that fit; this is the
/// rest, and it is why the footer does not have to list them all.
const HELP: [(&str, &[(&str, &str)]); 4] = [
    (
        "moving around",
        &[
            ("j / k, ↑ ↓", "up and down the list"),
            ("g / G", "first and last"),
            ("→ / ←", "open and close the transcript"),
            ("⇥", "move between the list and the transcript"),
            ("PgUp / PgDn", "scroll the transcript"),
            ("⏎", "resume the session in its own agent"),
        ],
    ),
    (
        "narrowing the list",
        &[
            ("/", "filter by title, id, project or agent"),
            ("A", "pick agents (space toggles, ⏎ closes)"),
            ("P", "pick a project (type to narrow)"),
            ("s", "search inside every transcript"),
            ("esc", "back out: the selection, the transcript, the filters"),
        ],
    ),
    (
        "acting on sessions",
        &[
            ("␣", "select, and step down"),
            ("*", "select everything shown"),
            ("r", "rename"),
            ("a", "archive, or bring back an archived one"),
            ("d", "delete (backed up first)"),
            ("m", "move to another project directory"),
            ("e", "export the session IR"),
            ("i", "import into the other agent"),
            ("p", "push to the hub"),
            ("c", "reply to the session"),
        ],
    ),
    (
        "the rest",
        &[
            ("R", "rescan the stores"),
            ("D", "store health"),
            ("?", "this help"),
            ("q", "quit — as does esc with nothing left to back out of"),
        ],
    ),
];

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for (group, keys) in HELP {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(group.to_string(), Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)));
        for (key, what) in keys {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{key:<12}"), theme::key()),
                Span::styled((*what).to_string(), theme::label()),
            ]));
        }
    }
    let area = popup(area, 74, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    let inner = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner) as u16;
    let scroll = app.help_scroll.min(max_scroll);
    let more = if max_scroll > 0 { " j/k scrolls · " } else { " " };
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::ACCENT))
                .title(Span::styled(" keys ", theme::title(true)))
                .title_bottom(Span::styled(format!("{more}any other key closes "), theme::dim())),
        ),
        area,
    );
}

/// The agent or project picker: what the list can be narrowed to, with how
/// many sessions each choice has.
fn draw_picker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = &app.picker else { return };
    let rows = app.picker_rows();
    let project = picker.kind == PickerKind::Project;
    let area = popup(area, 66, rows.len() as u16 + if project { 5 } else { 4 });
    frame.render_widget(Clear, area);

    let title = if project { " project " } else { " agents " };
    let hint = if project {
        " type to narrow · ⏎ pick · esc close "
    } else {
        // Each toggle applies at once, so esc is "close", not "cancel".
        " ␣ toggles · ⏎ done · esc close "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Span::styled(title, theme::title(true)))
        .title_bottom(Span::styled(hint, theme::dim()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [query_area, rows_area] = Layout::vertical([
        Constraint::Length(if project { 1 } else { 0 }),
        Constraint::Min(0),
    ])
    .areas(inner);
    if project {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▸ ", theme::key()),
                Span::raw(picker.query.clone()),
                Span::styled("▏", theme::dim()),
            ])),
            query_area,
        );
    }

    let visible = rows_area.height as usize;
    let first = picker.cursor.saturating_sub(visible.saturating_sub(1));
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(i, row)| {
            let here = i == picker.cursor;
            let mark = match (project, row.on) {
                (true, true) => "◉ ",
                (true, false) => "○ ",
                (false, true) => "[x] ",
                (false, false) => "[ ] ",
            };
            let name = match row.agent {
                Some(agent) => Style::default().fg(theme::agent(agent)),
                None => Style::default(),
            };
            let width = rows_area.width as usize;
            let count = format!("{}", row.count);
            // The count is the point of the row, so the label gives way.
            let room = width.saturating_sub(mark.chars().count() + count.chars().count() + 2);
            let label = shorten(&row.label, room);
            let pad = room.saturating_sub(label.chars().count()) + 1;
            Line::from(vec![
                Span::styled(mark, if row.on { theme::key() } else { theme::dim() }),
                Span::styled(label, if here { name.add_modifier(Modifier::BOLD) } else { name }),
                Span::raw(" ".repeat(pad)),
                Span::styled(count, theme::dim()),
            ])
            .style(if here { Style::default().bg(Color::DarkGray) } else { Style::default() })
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_area);
}

fn draw_hits(frame: &mut Frame, app: &App, area: Rect) {
    let hits = app.hits.as_deref().unwrap_or_default();
    let block = panel(
        format!("{} matches for \"{}\"", hits.len(), app.searched_for),
        !app.preview_focus,
    );

    let inner_height = area.height.saturating_sub(2) as usize;
    // Two lines per hit; keep the selection on screen.
    let per_hit = 2;
    let visible = (inner_height / per_hit).max(1);
    let first = app.hit_selected.saturating_sub(visible.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    for (i, hit) in hits.iter().enumerate().skip(first).take(visible) {
        let selected = i == app.hit_selected;
        let marker = if selected { "▸ " } else { "  " };
        let head = Line::from(vec![
            Span::styled(marker, theme::key()),
            Span::styled(
                hit.title.clone().unwrap_or_else(|| "(untitled)".into()),
                if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                format!("  {} {} #{}", hit.agent, hit.role, hit.seq),
                theme::dim(),
            ),
            Span::styled(
                if hit.status == "archived" { "  archived" } else { "" },
                Style::default().fg(Color::Magenta),
            ),
        ]);
        lines.push(head);
        lines.push(Line::from(snippet_spans(&hit.snippet)));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Turn the control-character-delimited snippet into highlighted spans.
fn snippet_spans(snippet: &str) -> Vec<Span<'static>> {
    let flat = snippet.replace('\n', " ");
    let mut spans = vec![Span::raw("    ")];
    let mut highlighted = false;
    for chunk in flat.split_inclusive([asm_core::index::MATCH_START, asm_core::index::MATCH_END]) {
        let (text, next) = match chunk.chars().last() {
            Some(c) if c == asm_core::index::MATCH_START => (&chunk[..chunk.len() - 1], true),
            Some(c) if c == asm_core::index::MATCH_END => (&chunk[..chunk.len() - 1], false),
            _ => (chunk, highlighted),
        };
        if !text.is_empty() {
            let style = if highlighted {
                Style::default().fg(theme::WARN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(text.to_string(), style));
        }
        highlighted = next;
    }
    spans
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let mut title = format!("sessions  {}", app.filtered.len());
    if app.filtered.len() != app.sessions.len() {
        title.push_str(&format!(" of {}", app.sessions.len()));
    }
    if !app.selection.is_empty() {
        title.push_str(&format!("  ·  {} selected", app.selection.len()));
    }
    let block = panel(title, !app.preview_focus);

    // Columns give way to the title, widest and least useful first: a row
    // with no title says nothing about the session it stands for.
    let inner = area.width.saturating_sub(2);
    let project_width = inner.saturating_sub(75).clamp(0, 40);
    let show_project = project_width >= 12;
    let show_id = inner >= 62;
    let show_size = inner >= 50;
    let show_updated = inner >= 42;

    let rows: Vec<Row> = app
        .filtered
        .iter()
        .map(|&i| {
            let s = &app.sessions[i];
            let status = match s.status {
                SessionStatus::Live { .. } => {
                    Span::styled("● live", Style::default().fg(theme::GOOD))
                }
                SessionStatus::Idle => Span::styled("idle", theme::dim()),
                SessionStatus::Archived => Span::styled("arch", Style::default().fg(Color::Magenta)),
            };
            let mut cells = vec![
                Cell::from(Span::styled(if app.is_ticked(s) { "◉" } else { " " }, theme::key())),
                Cell::from(Span::styled(
                    s.handle.agent.to_string(),
                    Style::default().fg(theme::agent(s.handle.agent)),
                )),
            ];
            if show_id {
                cells.push(Cell::from(Span::styled(s.short_id().to_string(), theme::dim())));
            }
            cells.push(Cell::from(s.title.clone().unwrap_or_else(|| "(untitled)".into())));
            if show_project {
                cells.push(Cell::from(Span::styled(
                    shorten(&home_relative(&s.project_root), project_width as usize),
                    theme::label(),
                )));
            }
            if show_updated {
                cells.push(Cell::from(Span::styled(s.updated.map(ago).unwrap_or_default(), theme::dim())));
            }
            if show_size {
                cells.push(Cell::from(Span::styled(
                    s.size_bytes.map(asm_core::fmt::human_bytes).unwrap_or_default(),
                    theme::dim(),
                )));
            }
            cells.push(Cell::from(status));
            Row::new(cells)
        })
        .collect();

    let mut widths = vec![Constraint::Length(1), Constraint::Length(11)];
    let mut header = vec!["", "agent"];
    if show_id {
        widths.push(Constraint::Length(8));
        header.push("id");
    }
    widths.push(Constraint::Fill(2));
    header.push("title");
    if show_project {
        widths.push(Constraint::Length(project_width));
        header.push("project");
    }
    if show_updated {
        widths.push(Constraint::Length(10));
        header.push("updated");
    }
    if show_size {
        widths.push(Constraint::Length(8));
        header.push("size");
    }
    widths.push(Constraint::Length(6));
    header.push("state");

    let table = Table::new(rows, widths)
        .header(Row::new(header).style(theme::label().add_modifier(Modifier::BOLD)))
        .row_highlight_style(theme::selected_row())
        .block(block);

    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    // Titled by the transcript in it, which during a search is the match's
    // session rather than the row the cursor is on.
    let showing = app
        .preview_for
        .as_deref()
        .and_then(|id| app.sessions.iter().find(|s| s.handle.native_id == id))
        .or_else(|| app.selected_session());
    let title = showing
        .map(|s| {
            let size = s
                .size_bytes
                .map(|b| format!(" · {}", asm_core::fmt::human_bytes(b)))
                .unwrap_or_default();
            format!("{} — {}{size}", s.short_id(), s.title.as_deref().unwrap_or("(untitled)"))
        })
        .unwrap_or_else(|| "transcript".to_string());
    let block = panel(title, app.preview_focus)
        .title_bottom(Span::styled(" ← closes ", theme::dim()));

    let inner_height = area.height.saturating_sub(2) as usize;
    let total = app.preview.len();
    let max_scroll = total.saturating_sub(inner_height) as u16;
    let scroll = app.preview_scroll.min(max_scroll);

    let lines: Vec<Line> = app
        .preview
        .iter()
        .skip(scroll as usize)
        .take(inner_height)
        .map(|line| {
            let style = match line.kind {
                PreviewKind::Role => Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                PreviewKind::Text => Style::default(),
                PreviewKind::Tool => Style::default().fg(theme::WARN),
                PreviewKind::Meta => theme::dim(),
            };
            Line::styled(line.text.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// `key label` pairs, as many as fit. Whole hints are dropped from the end
/// rather than cut in half, and the ones that matter most come first.
fn hints(pairs: &[(&str, &str)], width: u16) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for (key, what) in pairs {
        let cost = key.chars().count() + what.chars().count() + 4;
        if used + cost > width as usize {
            break;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", theme::dim()));
        }
        spans.push(Span::styled((*key).to_string(), theme::key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*what).to_string(), theme::label()));
        used += cost;
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect, filters: &[String]) {
    // Bottom up: the hints are the last thing to go.
    let [filter_line, status_line, hint_line] = match area.height {
        0 => return,
        1 => [Rect { height: 0, ..area }, Rect { height: 0, ..area }, area],
        2 => {
            let [status, hint] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
            [Rect { height: 0, ..area }, status, hint]
        }
        _ => Layout::vertical([
            Constraint::Length(if filters.is_empty() { 0 } else { 1 }),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area),
    };

    if !filters.is_empty() {
        // "(esc clears)" is how to get out of it, so it is kept and the
        // filters themselves give way.
        let tail = "  (esc clears)";
        let room = (filter_line.width as usize).saturating_sub(tail.chars().count() + 9);
        let mut spans = vec![Span::styled(" showing ", theme::dim())];
        let mut used = 0usize;
        for (i, filter) in filters.iter().enumerate() {
            let text = shorten(filter, room.saturating_sub(used).min(40));
            if text.chars().count() + used > room {
                spans.push(Span::styled(" …", theme::dim()));
                break;
            }
            if i > 0 {
                spans.push(Span::styled(" · ", theme::dim()));
                used += 3;
            }
            used += text.chars().count();
            spans.push(Span::styled(text, Style::default().fg(theme::ACCENT)));
        }
        spans.push(Span::styled(tail, theme::dim()));
        frame.render_widget(Paragraph::new(Line::from(spans)), filter_line);
    }

    let prompt = |what: &str, value: &str| {
        Line::from(vec![
            Span::styled(format!(" {what} "), theme::key().add_modifier(Modifier::REVERSED)),
            Span::raw(format!(" {value}")),
            Span::styled("▏", Style::default().fg(theme::ACCENT)),
        ])
    };
    let (status, keys): (Line, Vec<(&str, &str)>) = match app.mode {
        Mode::Filter => (prompt("filter", &app.input), vec![("⏎", "done"), ("esc", "cancel")]),
        Mode::Rename => (prompt("new title", &app.input), vec![("⏎", "rename"), ("esc", "cancel")]),
        Mode::Search => (
            prompt("search transcripts", &app.input),
            vec![("⏎", "search"), ("esc", "cancel")],
        ),
        Mode::Export => (prompt("export to", &app.input), vec![("⏎", "write"), ("esc", "cancel")]),
        Mode::Move => (prompt("move to", &app.input), vec![("⏎", "move"), ("esc", "cancel")]),
        Mode::Send => {
            let agent = app
                .pending_session()
                .map(|s| s.handle.agent.to_string())
                .unwrap_or_else(|| "the agent".to_string());
            (prompt(&format!("reply to {agent}"), &app.input), vec![("⏎", "send"), ("esc", "cancel")])
        }
        Mode::BulkInput => {
            let verb = app.pending_bulk().map(|(a, _)| a.verb()).unwrap_or("");
            let n = app.pending_bulk().map(|(_, n)| n).unwrap_or(0);
            (
                prompt(&format!("{} {n} into", verb.to_lowercase()), &app.input),
                vec![("⏎", "go"), ("esc", "cancel")],
            )
        }
        Mode::ConfirmDelete => {
            let target = app
                .pending_session()
                .map(|s| format!("{} \"{}\"", s.short_id(), s.title.as_deref().unwrap_or("?")))
                .unwrap_or_default();
            (
                Line::styled(
                    format!(" delete {target} and all its sidecars? it is backed up first."),
                    Style::default().fg(theme::BAD).add_modifier(Modifier::BOLD),
                ),
                vec![("y", "delete"), ("any other key", "keep it")],
            )
        }
        Mode::ConfirmImport => {
            let what = app
                .pending_session()
                .zip(app.import_target())
                .map(|(s, t)| format!("import {} into {t}", s.short_id()))
                .unwrap_or_default();
            (
                Line::raw(format!(" {what}? full mode, idempotent.")),
                vec![("y", "import"), ("any other key", "cancel")],
            )
        }
        Mode::ConfirmBulk => {
            let what = app
                .pending_bulk()
                .map(|(a, n)| format!("{} {n} selected session(s)", a.verb().to_lowercase()))
                .unwrap_or_default();
            let destructive = app.pending_bulk().is_some_and(|(a, _)| a.is_destructive());
            (
                Line::styled(
                    format!(" {what}?{}", if destructive { " backed up first." } else { "" }),
                    if destructive {
                        Style::default().fg(theme::WARN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                vec![("y", "do it"), ("any other key", "cancel")],
            )
        }
        Mode::Picker => (
            status_line_of(app),
            match app.picker.as_ref().map(|p| p.kind) {
                Some(PickerKind::Agent) => vec![("␣", "toggle"), ("⏎", "done"), ("esc", "close")],
                _ => vec![("type", "to narrow"), ("⏎", "pick"), ("esc", "close")],
            },
        ),
        Mode::Normal if app.hits.is_some() => (
            status_line_of(app),
            vec![("⏎", "go to session"), ("/", "new search"), ("esc", "back"), ("?", "help")],
        ),
        Mode::Normal if !app.selection.is_empty() => (
            status_line_of(app),
            vec![
                ("a", "archive"),
                ("d", "delete"),
                ("p", "push"),
                ("m", "move"),
                ("e", "export"),
                ("i", "import"),
                ("esc", "clear"),
                ("?", "help"),
            ],
        ),
        Mode::Normal => (
            status_line_of(app),
            vec![
                ("⏎", "resume"),
                ("␣", "select"),
                (if app.preview_open { "←" } else { "→" }, "transcript"),
                ("A", "agents"),
                ("P", "project"),
                ("/", "filter"),
                ("s", "search"),
                ("c", "reply"),
                ("?", "help"),
            ],
        ),
    };
    frame.render_widget(Paragraph::new(status), status_line);
    frame.render_widget(Paragraph::new(hints(&keys, hint_line.width)), hint_line);
}

/// The left-hand side of the status line: what asm is doing, and anything
/// it wants the user to know about the stores.
fn status_line_of(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::raw(app.status.clone())];
    if app.scanning {
        spans.push(Span::styled("  ⟳ scanning", Style::default().fg(theme::ACCENT)));
    }
    if app.sending {
        spans.push(Span::styled("  ● replying (esc stops)", Style::default().fg(theme::GOOD)));
    }
    if app.doctor_warnings > 0 {
        spans.push(Span::styled(
            format!("  ⚠ {} (D)", app.doctor_warnings),
            Style::default().fg(theme::WARN),
        ));
    }
    Line::from(spans)
}

/// `~/…` for a path inside the home directory: the same shortening the web
/// UI does, and the form the user reads their own paths in.
pub fn home_relative(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match asm_core::paths::home() {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => shown,
        },
        None => shown,
    }
}

pub fn shorten(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .rev()
        .take(max.saturating_sub(1))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("…{tail}")
}

pub fn ago(ts: jiff::Timestamp) -> String {
    let seconds = jiff::Timestamp::now().as_second() - ts.as_second();
    match seconds {
        i64::MIN..0 => "future".into(),
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        86_400..2_592_000 => format!("{}d", seconds / 86_400),
        _ => ts.to_string().chars().take(10).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, PickerKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use asm_core::model::{AgentKind, Session, SessionLocation, SessionRef, SessionStatus, Usage};

    fn session(agent: AgentKind, id: &str, title: &str, project: &str) -> Session {
        Session {
            handle: SessionRef {
                agent,
                native_id: id.into(),
                location: SessionLocation::JsonlFile { path: format!("/tmp/{id}.jsonl").into() },
            },
            title: Some(title.into()),
            slug: None,
            project_root: project.into(),
            git_branch: None,
            created: None,
            updated: None,
            model: None,
            usage: Usage::default(),
            status: SessionStatus::Idle,
            parent: None,
            agent_version: None,
            size_bytes: Some(4096),
        }
    }

    /// An app with sessions and no worker thread behind it: drawing never
    /// asks the worker for anything.
    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let (_tx2, rx) = std::sync::mpsc::channel();
        let worker = crate::worker::Worker {
            tx,
            rx,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let mut app = App::new(worker);
        app.sessions = vec![
            session(AgentKind::ClaudeCode, "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43", "Trace the pool leak", "/home/u/code/mercury"),
            session(AgentKind::OpenCode, "ses_71bc0e4faa2196KmTqRvBnLd2", "Billing webhook retries", "/home/u/code/atlas-web"),
            session(AgentKind::JCode, "session_meridian_1788_abc", "Benchmark the pool", "/home/u/code/mercury"),
        ];
        app.filtered = (0..app.sessions.len()).collect();
        app.set_projects_for_test(
            ["/home/u/code/mercury", "/home/u/code/atlas-web"]
                .into_iter()
                .map(|root| asm_core::model::Project {
                    root: root.into(),
                    repo: None,
                    agents: Vec::new(),
                    worktrees: vec![asm_core::model::ProjectWorktree {
                        path: root.into(),
                        branch: None,
                        is_main: true,
                        session_count: 1,
                    }],
                    session_count: 1,
                    size_bytes: 0,
                    last_updated: None,
                })
                .collect(),
        );
        app.set_status("3 sessions".into());
        app
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Prints the screens, for looking at the layout without a terminal:
    /// `cargo test -p asm-tui -- --ignored --nocapture screens`.
    #[test]
    #[ignore]
    fn screens() {
        let mut app = app();
        println!("— list, transcript closed —\n{}\n", render(&app, 110, 14));
        app.preview_open = true;
        app.selection.insert((AgentKind::ClaudeCode, "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43".into()));
        println!("— transcript open, one selected —\n{}\n", render(&app, 110, 14));
        app.preview_open = false;
        app.selection.clear();
        app.open_picker_for_test(PickerKind::Project);
        println!("— project picker —\n{}\n", render(&app, 110, 14));
        app.picker = None;
        app.help_open = true;
        println!("— help —\n{}", render(&app, 110, 30));
    }

    /// Every width keeps a title: the columns give way, not the thing
    /// that says what the session is.
    #[test]
    fn the_title_survives_every_width() {
        for width in [40u16, 60, 74, 80, 90, 100, 120, 200] {
            let screen = render(&app(), width, 10);
            assert!(
                screen.contains("Trace the pool") || screen.contains("Trace the"),
                "width {width} lost the titles:\n{screen}"
            );
            for line in screen.lines() {
                assert!(line.chars().count() <= width as usize, "width {width} overflowed: {line}");
            }
        }
    }

    /// Even a terminal too short for a list keeps the keys on screen.
    #[test]
    fn a_tiny_terminal_still_shows_the_keys() {
        for height in [3u16, 4, 5, 8] {
            let screen = render(&app(), 80, height);
            assert_eq!(screen.lines().count(), height as usize);
            assert!(screen.lines().last().unwrap().contains("resume"), "height {height}:\n{screen}");
        }
    }

    /// The keys are laid out before the panels, so they are never what gets
    /// cut off — and a line too narrow for every hint drops whole hints.
    #[test]
    fn the_footer_always_shows_keys_and_never_half_a_word() {
        for width in [40u16, 60, 100, 200] {
            let screen = render(&app(), width, 12);
            let lines: Vec<&str> = screen.lines().collect();
            let hint = lines.last().copied().unwrap_or_default();
            assert!(hint.contains("resume"), "{width}: {hint}");
            assert!(hint.chars().count() <= width as usize, "{width}: {hint}");
            // Whole hints only: never "…" and never a dangling separator.
            assert!(!hint.ends_with('·') && !hint.contains('…'), "{width}: {hint}");
            assert!(screen.lines().count() == 12);
        }
    }

    #[test]
    fn the_transcript_is_closed_until_it_is_asked_for() {
        let mut app = app();
        let closed = render(&app, 100, 12);
        assert!(!closed.contains("← closes"), "no transcript pane:\n{closed}");
        assert!(closed.contains("→ transcript"), "the way to open it is on screen");
        // The list has the whole width for itself.
        let list_width = closed.lines().next().unwrap().chars().count();
        assert_eq!(list_width, 100, "{closed}");

        app.preview_open = true;
        let open = render(&app, 100, 12);
        assert!(open.contains("← closes"), "{open}");
        assert!(open.lines().next().unwrap().chars().count() == 100);
        // Both panels, side by side.
        assert!(open.lines().next().unwrap().matches('╮').count() == 2, "{open}");
    }

    #[test]
    fn help_lists_every_key_and_says_how_to_close() {
        let mut app = app();
        app.help_open = true;
        let screen = render(&app, 100, 44);
        for key in ["rename", "push to the hub", "pick agents", "store health", "quit"] {
            assert!(screen.contains(key), "{key} missing:\n{screen}");
        }
        assert!(screen.contains("closes"), "{screen}");

        // On a short terminal it says how to read the rest.
        let short = render(&app, 100, 20);
        assert!(short.contains("j/k scrolls"), "{short}");
        app.help_scroll = 40;
        assert!(render(&app, 100, 20).contains("quit"), "scrolling reaches the end");
    }

    #[test]
    fn the_pickers_show_what_each_choice_would_leave() {
        let mut app = app();
        app.open_picker_for_test(PickerKind::Agent);
        let agents = render(&app, 100, 16);
        for line in ["claude-code", "opencode", "jcode"] {
            assert!(agents.contains(line), "{line} missing:\n{agents}");
        }
        assert!(agents.contains("[ ]") && agents.contains("toggles"), "{agents}");

        app.open_picker_for_test(PickerKind::Project);
        let projects = render(&app, 100, 16);
        assert!(projects.contains("all projects"), "{projects}");
        assert!(projects.contains("mercury") && projects.contains("atlas-web"), "{projects}");
        // Two sessions live in mercury, one in atlas-web.
        assert!(projects.contains(" 2") && projects.contains(" 1"), "{projects}");
    }

    #[test]
    fn active_filters_are_named_above_the_status_line() {
        let mut app = app();
        app.agents.insert(AgentKind::ClaudeCode);
        app.filter = "pool".into();
        let screen = render(&app, 100, 12);
        assert!(screen.contains("showing"), "{screen}");
        assert!(screen.contains("claude-code") && screen.contains("\"pool\""), "{screen}");
        assert!(screen.contains("esc clears"), "{screen}");
    }
}
