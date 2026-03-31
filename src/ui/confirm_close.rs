use crate::tui::App;
use crate::ui::theme::Theme;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Small fixed-size centered modal — wide enough to hold the message,
/// tall enough for a border + content rows.
fn modal_rect(r: Rect) -> Rect {
    let width: u16 = 52;
    let height: u16 = 9;

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
        .title(Span::styled(" Close Window ", t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let wt_name = app
        .selected_worktree()
        .map(|w| w.window_name.as_str())
        .unwrap_or("?");
    let phase = app.selected_phase().to_string();
    let active = App::is_active_phase(&phase);

    let mut lines: Vec<Line> = vec![Line::from("")];

    if active {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{}", wt_name),
                t.text_style().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(
                format!(" is running  [{}]", phase),
                t.text_style().fg(t.phase_color(&phase)),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "  The agent will be killed.",
            t.text_style().fg(t.phase_error),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{}", wt_name),
                t.text_style().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(" is idle", t.text_dim_style()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  y", t.key_badge_style()),
        Span::styled(
            if active {
                "  kill agent and close"
            } else {
                "  close window"
            },
            t.text_style(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Esc / any key", t.key_badge_style()),
        Span::styled("  cancel", t.text_dim_style()),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}
