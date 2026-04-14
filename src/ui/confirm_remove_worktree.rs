use crate::tui::App;
use crate::ui::theme::Theme;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Small fixed-size centered modal for the remove-worktree confirmation.
fn modal_rect(r: Rect) -> Rect {
    let width: u16 = 58;
    let height: u16 = 10;

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(r.height.saturating_sub(height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(r.width.saturating_sub(width) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vert[1])[1]
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

pub fn render(f: &mut Frame, app: &App, t: &Theme) {
    let area = modal_rect(f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(" Remove Worktree ", t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let wt_name = app
        .selected_worktree()
        .map(|w| w.window_name.as_str())
        .unwrap_or("?");

    let mut lines: Vec<Line> = vec![Line::from("")];

    lines.push(Line::from(vec![
        Span::raw("  Permanently remove  "),
        Span::styled(wt_name, t.text_style().add_modifier(Modifier::BOLD)),
        Span::raw("  ?"),
    ]));

    lines.push(Line::from(Span::styled(
        "  This deletes the git worktree directory and",
        t.text_style().fg(t.phase_error),
    )));
    lines.push(Line::from(Span::styled(
        "  removes the entry from task-master.toml.",
        t.text_style().fg(t.phase_error),
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  y", t.key_badge_style()),
        Span::styled("  confirm remove", t.text_style()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Esc / any key", t.key_badge_style()),
        Span::styled("  cancel", t.text_dim_style()),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}
