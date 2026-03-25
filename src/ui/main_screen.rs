use crate::stats::format_tokens;
use crate::tui::App;
use crate::ui::theme::Theme;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

const SPLIT_THRESHOLD: u16 = 160;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn render(f: &mut Frame, app: &mut App, t: &Theme) {
    let area = f.area();

    // 3-row layout: header (2) / content (flex) / status bar (1)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(0),    // content
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_header(f, outer[0], t);
    render_content(f, outer[1], app, t);
    render_statusbar(f, outer[2], app, t);
}

// ---------------------------------------------------------------------------
// Header (2 rows)
// ---------------------------------------------------------------------------

fn render_header(f: &mut Frame, area: Rect, t: &Theme) {
    // Row 1: app name
    // Row 2: key hint badges
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    // App name
    let title = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "task-master",
            Style::default()
                .fg(t.text_accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(title, rows[0]);

    // Key hints row
    let hints: &[(&str, &str)] = &[
        ("j/k", "navigate"),
        ("s", "spawn"),
        ("p", "plan"),
        ("x", "qa"),
        ("r", "reset"),
        ("a", "attach"),
        ("t", "theme"),
        ("?", "help"),
        ("q", "quit"),
    ];

    let mut spans = vec![Span::raw(" ")];
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", t.text_dim_style()));
        }
        spans.push(Span::styled(format!(" {key} "), t.key_badge_style()));
        spans.push(Span::styled(format!(" {desc}"), t.key_desc_style()));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

// ---------------------------------------------------------------------------
// Content: responsive two-panel layout
// ---------------------------------------------------------------------------

fn render_content(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let (left_pct, right_pct) = if area.width >= SPLIT_THRESHOLD {
        (50, 50)
    } else {
        (45, 55)
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Percentage(right_pct),
        ])
        .split(area);

    render_worktree_list(f, cols[0], app, t);
    render_actions(f, cols[1], app, t);
}

// ---------------------------------------------------------------------------
// Worktree list
// ---------------------------------------------------------------------------

fn render_worktree_list(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let selected = app.list_state.selected();

    let items: Vec<ListItem> = app
        .worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| {
            let phase = app.phases.get(i).map(|s| s.as_str()).unwrap_or("?");
            let phase_color = t.phase_color(phase);
            let is_selected = selected == Some(i);
            let base = if is_selected {
                t.selection_style()
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(format!("{:<20}", wt.window_name), base.fg(t.text)),
                Span::styled(format!("[{}]", phase), base.fg(phase_color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(t.border_style())
                .title(Span::styled(" Worktrees ", t.title_style())),
        )
        .highlight_style(t.selection_style().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

// ---------------------------------------------------------------------------
// Actions panel
// ---------------------------------------------------------------------------

fn render_actions(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let phase = app.selected_phase().to_string();
    let active = App::is_active_phase(&phase);
    let has_wt = !app.worktrees.is_empty();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    let spawn_warn = if active { "  ⚠ resets window" } else { "" };
    let spawn_label = format!("spawn agent{}", spawn_warn);
    let plan_label = format!("plan{}", spawn_warn);

    lines.push(action_line('s', &spawn_label, has_wt, false, t));
    lines.push(action_line('p', &plan_label, has_wt, false, t));
    lines.push(Line::from(""));
    lines.push(action_line('x', "qa  (enter PR #)", has_wt, !active, t));
    lines.push(Line::from(""));
    lines.push(action_line('r', "reset window", has_wt, !active, t));
    lines.push(action_line('a', "attach", has_wt, !active, t));
    lines.push(Line::from(""));

    // Separator
    lines.push(Line::from(Span::styled(
        " ".to_string() + &"─".repeat(32),
        t.separator_style(),
    )));
    lines.push(Line::from(""));
    lines.push(action_line('t', "theme picker", true, false, t));
    lines.push(action_line('?', "help", true, false, t));
    lines.push(action_line('q', "quit", true, false, t));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(" Actions ", t.title_style()));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn action_line<'a>(
    key: char,
    label: &'a str,
    _enabled: bool,
    disabled: bool,
    t: &Theme,
) -> Line<'a> {
    let label = label.to_string();
    if disabled {
        Line::from(vec![
            Span::styled("  -  ", t.text_dim_style()),
            Span::styled(label, t.text_dim_style()),
        ])
    } else {
        Line::from(vec![
            Span::styled(format!("  {}  ", key), t.key_badge_style()),
            Span::raw(" "),
            Span::styled(label, t.text_style()),
        ])
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

pub fn render_statusbar(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    use crate::tui::Mode;

    let (content, style) = match &app.mode {
        Mode::Prompt(crate::tui::ActionKind::Spawn) => (
            format!(" Prompt: {}_", app.input_buf),
            t.text_style().fg(t.phase_done),
        ),
        Mode::Prompt(crate::tui::ActionKind::Plan) => (
            format!(" Task: {}_", app.input_buf),
            t.text_style().fg(t.phase_done),
        ),
        Mode::Prompt(crate::tui::ActionKind::Qa) => (
            format!(" PR number: {}_", app.input_buf),
            t.text_style().fg(t.phase_done),
        ),
        Mode::ForceConfirm => (
            " Press Enter to force-spawn (discards uncommitted changes), Esc to cancel".to_string(),
            t.text_style().fg(t.phase_error),
        ),
        Mode::Normal => {
            if let Some(msg) = app.current_status() {
                (format!(" {}", msg), t.text_dim_style())
            } else {
                (stats_bar_text(app), t.text_dim_style())
            }
        }
    };

    f.render_widget(Paragraph::new(content).style(style), area);
}

fn stats_bar_text(app: &App) -> String {
    let idx = match app.selected() {
        Some(i) => i,
        None => return " No worktrees".to_string(),
    };
    let wt = match app.worktrees.get(idx) {
        Some(w) => w,
        None => return String::new(),
    };
    match app.stats_cache.get(&idx) {
        Some(stats) if stats.input > 0 || stats.sessions > 0 => {
            format!(
                " {}  ·  {} in / {} out  ·  {} sessions",
                wt.window_name,
                format_tokens(stats.input),
                format_tokens(stats.output),
                stats.sessions,
            )
        }
        _ => format!(" {}  ·  no usage data", wt.window_name),
    }
}
