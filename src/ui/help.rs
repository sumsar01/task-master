use crate::ui::theme::Theme;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// Percentage-based centered popup
// ---------------------------------------------------------------------------

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}

// ---------------------------------------------------------------------------
// Help content
// ---------------------------------------------------------------------------

const KEYBINDINGS: &[(&str, &str, &str)] = &[
    // (key, description, section)
    ("Navigation", "", "section"),
    ("j / ↓", "move down", ""),
    ("k / ↑", "move up", ""),
    ("", "", ""),
    ("Actions", "", "section"),
    ("s", "spawn dev agent (prompts for task)", ""),
    ("p", "start planning agent (prompts for task)", ""),
    ("x", "run QA agent (prompts for PR #)", ""),
    ("r", "reset active window to idle", ""),
    ("a", "attach to active window", ""),
    ("v", "start / restart supervisor", ""),
    ("", "", ""),
    ("Worktrees", "", "section"),
    (
        "E",
        "spawn ephemeral worktree (auto-named, prompts for task)",
        "",
    ),
    ("N", "create new named worktree", ""),
    ("D", "remove selected worktree (destructive)", ""),
    ("X", "remove merged ephemeral worktrees", ""),
    ("c", "close tmux window for worktree", ""),
    ("", "", ""),
    ("Preview", "", "section"),
    ("w", "toggle agent preview pane", ""),
    ("J", "scroll preview toward tail", ""),
    ("K", "scroll preview up (into history)", ""),
    ("", "", ""),
    ("UI", "", "section"),
    ("t", "open theme picker", ""),
    ("?", "toggle this help", ""),
    ("", "", ""),
    ("General", "", "section"),
    ("q / Q", "quit", ""),
    ("Esc", "cancel / close overlay", ""),
];

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

pub fn render(f: &mut Frame, t: &Theme) {
    let area = centered_rect(60, 70, f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(" Help ", t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::from("")];

    for (key, desc, kind) in KEYBINDINGS {
        if *kind == "section" {
            lines.push(Line::from(Span::styled(
                format!("  {}", key),
                t.section_header_style(),
            )));
        } else if key.is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:>8}  ", key),
                    t.key_badge_style().add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, t.text_style()),
            ]));
        }
    }

    // Footer hint
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press ? or Esc to close",
        t.text_dim_style(),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}
