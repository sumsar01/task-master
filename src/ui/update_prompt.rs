use crate::tui::{App, Mode};
use crate::ui::theme::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

fn modal_rect(r: Rect) -> Rect {
    let width: u16 = 58;
    let height: u16 = 11;

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

    let (title, lines) = match &app.mode {
        Mode::UpdateAvailable(info) => {
            let title = " Update Available ";
            let lines: Vec<Line> = vec![
                Line::from(""),
                Line::from(vec![
                    Span::raw("  A new version of "),
                    Span::styled(
                        "task-master",
                        t.text_style().add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::raw(" is available."),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  Current  "),
                    Span::styled(format!("v{}", crate::VERSION), t.text_dim_style()),
                ]),
                Line::from(vec![
                    Span::raw("  Latest   "),
                    Span::styled(
                        format!("v{}", info.latest_version),
                        t.text_style()
                            .fg(t.phase_done)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  y / Enter", t.key_badge_style()),
                    Span::styled("  download and replace binary", t.text_style()),
                ]),
                Line::from(vec![
                    Span::styled("  n / Esc  ", t.key_badge_style()),
                    Span::styled("  skip for this session", t.text_dim_style()),
                ]),
            ];
            (title, lines)
        }
        Mode::Updating => {
            let title = " Updating… ";
            let lines: Vec<Line> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Downloading new binary, please wait…",
                    t.text_style(),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Do not close the terminal.",
                    t.text_dim_style(),
                )),
            ];
            (title, lines)
        }
        _ => return,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(title, t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}
