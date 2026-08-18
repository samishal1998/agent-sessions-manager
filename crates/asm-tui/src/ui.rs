//! Rendering. Immediate mode: everything redraws each frame; the list and
//! preview only materialize their visible windows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use asm_core::model::SessionStatus;

use crate::app::{App, Mode};
use crate::worker::PreviewKind;

pub fn draw(frame: &mut Frame, app: &App) {
    let [main, status_bar] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    let [list_area, preview_area] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(main);

    if app.hits.is_some() {
        draw_hits(frame, app, list_area);
    } else {
        draw_list(frame, app, list_area);
    }
    draw_preview(frame, app, preview_area);
    draw_status(frame, app, status_bar);
    if app.show_doctor {
        draw_doctor(frame, app, frame.area());
    }
}

fn draw_doctor(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(8).clamp(20, 100);
    let height = (app.doctor.len() as u16 + 4).min(area.height.saturating_sub(4)).max(5);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let lines: Vec<Line> = app
        .doctor
        .iter()
        .map(|line| {
            let style = if line.contains('⚠') {
                Style::default().fg(Color::Yellow)
            } else if line.starts_with("  ") {
                Style::default().fg(Color::DarkGray)
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
                .border_style(Style::default().fg(Color::Yellow))
                .title(" store health — any key to close "),
        ),
        popup,
    );
}

fn draw_hits(frame: &mut Frame, app: &App, area: Rect) {
    let hits = app.hits.as_deref().unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {} matches for \"{}\" ", hits.len(), app.searched_for));

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
            Span::styled(marker, Style::default().fg(Color::Cyan)),
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
                Style::default().fg(Color::DarkGray),
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
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
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
    let border_style = if app.preview_focus {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    };
    let title = if app.filter.is_empty() {
        " sessions ".to_string()
    } else {
        format!(" sessions (filter: {}) ", app.filter)
    };
    let block = Block::default().borders(Borders::ALL).border_style(border_style).title(title);

    let rows: Vec<Row> = app
        .filtered
        .iter()
        .map(|&i| {
            let s = &app.sessions[i];
            let status = match s.status {
                SessionStatus::Live { .. } => Span::styled("live", Style::default().fg(Color::Green)),
                SessionStatus::Idle => Span::raw("idle"),
                SessionStatus::Archived => {
                    Span::styled("arch", Style::default().fg(Color::DarkGray))
                }
            };
            Row::new(vec![
                Cell::from(s.handle.agent.to_string()),
                Cell::from(s.short_id().to_string()),
                Cell::from(s.title.clone().unwrap_or_else(|| "(untitled)".into())),
                Cell::from(shorten(&s.project_root.display().to_string(), 28)),
                Cell::from(s.updated.map(super::ui::ago).unwrap_or_default()),
                Cell::from(status),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Length(8),
            Constraint::Fill(2),
            Constraint::Length(28),
            Constraint::Length(8),
            Constraint::Length(4),
        ],
    )
    .header(
        Row::new(["agent", "id", "title", "project", "updated", "st"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
    .block(block);

    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.preview_focus {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let title = app
        .selected_session()
        .map(|s| format!(" {} — {} ", s.short_id(), s.title.as_deref().unwrap_or("(untitled)")))
        .unwrap_or_else(|| " transcript ".to_string());
    let block = Block::default().borders(Borders::ALL).border_style(border_style).title(title);

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
                PreviewKind::Role => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                PreviewKind::Text => Style::default(),
                PreviewKind::Tool => Style::default().fg(Color::Yellow),
                PreviewKind::Meta => Style::default().fg(Color::DarkGray),
            };
            Line::styled(line.text.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let content = match app.mode {
        Mode::Filter => format!("filter: {}▏  (enter apply, esc cancel)", app.input),
        Mode::Rename => format!("new title: {}▏  (enter apply, esc cancel)", app.input),
        Mode::ConfirmDelete => {
            let target = app
                .pending_session()
                .map(|s| format!("{} \"{}\"", s.short_id(), s.title.as_deref().unwrap_or("?")))
                .unwrap_or_default();
            format!("delete {target} and all sidecars? backed up first. (y/N)")
        }
        Mode::Export => format!("export IR to: {}▏  (enter write, esc cancel)", app.input),
        Mode::Move => format!("move to project dir: {}▏  (enter move, esc cancel)", app.input),
        Mode::ConfirmImport => {
            let what = app
                .pending_session()
                .zip(app.import_target())
                .map(|(s, t)| format!("import {} into {t}", s.short_id()))
                .unwrap_or_default();
            format!("{what}? full mode, idempotent. (y/N)")
        }
        Mode::Search => format!("search transcripts: {}▏  (enter search, esc cancel)", app.input),
        Mode::Normal if app.hits.is_some() => {
            format!("{}  —  ⏎ jump to session · / new search · esc back to list", app.status)
        }
        Mode::Normal => {
            let scanning = if app.scanning { " ⟳" } else { "" };
            let health = match app.doctor_warnings {
                0 => String::new(),
                n => format!("  ⚠ {n} (D)"),
            };
            format!(
                "{}{scanning}{health}  —  ⏎ resume · r rename · a (un)archive · d delete · \
                 e export · m move · i import · s search · / filter · D health · ⇥ focus · \
                 R rescan · q quit",
                app.status
            )
        }
    };
    frame.render_widget(Paragraph::new(content).dark_gray().reversed(), area);
}

fn shorten(path: &str, max: usize) -> String {
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
