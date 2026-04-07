use crate::tui::App;
use crate::ui::theme::{ALL_THEMES, Theme};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

// ---------------------------------------------------------------------------
// Fixed-size centered popup
// ---------------------------------------------------------------------------

fn picker_rect(r: Rect) -> Rect {
    let height = (ALL_THEMES.len() as u16) + 4; // list rows + borders + title + hint
    let width = 36u16;

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
    let area = picker_rect(f.area());

    // Always clear behind the overlay first.
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = ALL_THEMES
        .iter()
        .enumerate()
        .map(|(i, (id, name))| {
            let is_cursor = i == app.theme_picker_cursor;
            let is_saved = *id == app.saved_theme_id.as_str();

            let prefix = if is_cursor { "▸ " } else { "  " };
            let suffix = if is_saved { " ✓" } else { "" };
            let label = format!("{prefix}{name}{suffix}");

            let style = if is_cursor {
                t.tab_active_style()
                    .remove_modifier(Modifier::UNDERLINED)
                    .add_modifier(Modifier::BOLD)
            } else {
                t.text_dim_style()
            };

            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    // Split inner area into hint line + list
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(
            Block::default()
                .borders(Borders::ALL)
                .border_style(t.border_style())
                .title(Span::styled(" Theme ", t.title_style()))
                .inner(area),
        );

    // Hint line
    let hint = Paragraph::new(Line::from(vec![
        Span::styled(" j/k", t.key_desc_style()),
        Span::styled("  ·  ", t.text_dim_style()),
        Span::styled("Enter", t.key_desc_style()),
        Span::raw(" commit"),
        Span::styled("  ·  ", t.text_dim_style()),
        Span::styled("Esc", t.key_desc_style()),
        Span::raw(" revert"),
    ]))
    .style(t.text_dim_style());

    let mut list_state = ListState::default();
    list_state.select(Some(app.theme_picker_cursor));

    let list = List::new(items).highlight_style(
        t.tab_active_style()
            .remove_modifier(Modifier::UNDERLINED)
            .add_modifier(Modifier::BOLD),
    );

    // Draw the outer block first.
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(t.border_style())
            .title(Span::styled(" Theme ", t.title_style())),
        area,
    );
    f.render_widget(hint, inner[0]);
    f.render_stateful_widget(list, inner[1], &mut list_state);
}
