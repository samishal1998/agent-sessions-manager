//! The one place colours live.
//!
//! Named ANSI colours, not RGB: they take the palette the user's terminal
//! was themed with, and they survive a 16-colour terminal. Agent colours
//! match the web UI's marks, so the same session looks the same in both.

use ratatui::style::{Color, Modifier, Style};

use asm_core::model::AgentKind;

/// The one accent. Anything focused, selected or actionable is this colour.
pub const ACCENT: Color = Color::Cyan;
pub const FAINT: Color = Color::DarkGray;
pub const WARN: Color = Color::Yellow;
pub const BAD: Color = Color::Red;
pub const GOOD: Color = Color::Green;

pub fn agent(agent: AgentKind) -> Color {
    match agent {
        AgentKind::ClaudeCode => Color::LightYellow,
        AgentKind::OpenCode => Color::LightBlue,
        AgentKind::JCode => Color::Green,
        AgentKind::Codex => Color::Gray,
        AgentKind::Antigravity => Color::Magenta,
        _ => Color::Gray,
    }
}

/// A panel's border: the accent when it has the keyboard, faint otherwise,
/// so focus is visible without a word for it.
pub fn border(focused: bool) -> Style {
    if focused { Style::default().fg(ACCENT) } else { Style::default().fg(FAINT) }
}

pub fn title(focused: bool) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    if focused { style.fg(ACCENT) } else { style.fg(Color::Gray) }
}

pub fn selected_row() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::REVERSED)
}

pub fn key() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn label() -> Style {
    Style::default().fg(Color::Gray)
}

pub fn dim() -> Style {
    Style::default().fg(FAINT)
}
