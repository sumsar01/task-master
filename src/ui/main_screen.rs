use crate::stats::format_tokens;
use crate::tui::App;
use crate::ui::theme::Theme;
use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
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
        ("v", "supervise"),
        ("w", "preview"),
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

/// Number of lines the Actions panel body occupies (excluding its border).
/// Keep in sync with `render_actions` content. Border adds 2, so the block
/// height passed to `Constraint::Length` is ACTIONS_LINES + 2.
const ACTIONS_LINES: u16 = 17;

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

    if app.show_preview {
        // Split the right column: Actions at natural height, preview fills rest.
        let actions_height = (ACTIONS_LINES + 2).min(cols[1].height);
        let right_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(actions_height), Constraint::Min(0)])
            .split(cols[1]);
        render_actions(f, right_rows[0], app, t);
        render_preview(f, right_rows[1], app, t);
    } else {
        render_actions(f, cols[1], app, t);
    }
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
    lines.push(action_line('v', "supervise", true, false, t));
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
// Agent preview pane
// ---------------------------------------------------------------------------

fn render_preview(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    // Determine title from selected worktree + phase.
    let title = match app.list_state.selected() {
        Some(i) => {
            let name = app
                .worktrees
                .get(i)
                .map(|w| w.window_name.as_str())
                .unwrap_or("?");
            let phase = app.phases.get(i).map(|s| s.as_str()).unwrap_or("?");
            let scroll_hint = if app.preview_scroll > 0 {
                format!(" {}:{} ↑ scrolled ", name, phase)
            } else {
                format!(" {}:{} ", name, phase)
            };
            scroll_hint
        }
        None => " Preview ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(title, t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.preview_lines.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "  No output captured — window may be idle or not open",
            t.text_dim_style(),
        ));
        f.render_widget(msg, inner);
        return;
    }

    // Visible height determines how many lines we can show.
    let visible = inner.height as usize;
    if visible == 0 {
        return;
    }

    let total = app.preview_lines.len();

    // `preview_scroll` counts lines from the bottom (0 = tail).
    // Compute the index of the first visible line.
    let bottom_line = total.saturating_sub(app.preview_scroll);
    let first_line = bottom_line.saturating_sub(visible);
    let slice = &app.preview_lines[first_line..bottom_line];

    // Parse each line's ANSI escape sequences into ratatui spans, then apply
    // color normalization:
    //   - Strip background colors (they clash with the task-master theme bg).
    //   - Remap near-white foreground colors (R+G+B > 570) to the theme text
    //     color. opencode uses near-white (238,238,238) / white (255,255,255)
    //     for most body text — on our themed background that stays readable,
    //     but remapping to the theme color makes it look native.
    //   - All structural colors (greens, yellows, blues, reds, dim greys) have
    //     R+G+B ≤ 531 and are left untouched.
    let theme_fg = t.text;
    let lines: Vec<Line> = slice
        .iter()
        .flat_map(|raw| {
            match raw.as_bytes().into_text() {
                Ok(text) => text
                    .lines
                    .into_iter()
                    .map(|line| {
                        let spans = line
                            .spans
                            .into_iter()
                            .map(|span| {
                                let style = normalize_ansi_style(span.style, theme_fg);
                                Span::styled(span.content, style)
                            })
                            .collect::<Vec<_>>();
                        Line::from(spans)
                    })
                    .collect::<Vec<_>>(),
                Err(_) => {
                    // Fallback: render as plain themed text.
                    vec![Line::from(Span::styled(raw.clone(), t.text_style()))]
                }
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

/// Normalize a span's style for rendering inside the preview pane:
/// - Remove background color (avoids clashing with the task-master theme).
/// - Remap near-white foreground (R+G+B > 570) to the theme's text color so
///   body text looks native rather than washed-out bright white.
fn normalize_ansi_style(mut style: Style, theme_fg: Color) -> Style {
    // Strip background.
    style.bg = None;

    // Remap near-white foreground to theme color.
    if let Some(Color::Rgb(r, g, b)) = style.fg {
        if (r as u16) + (g as u16) + (b as u16) > 570 {
            style.fg = Some(theme_fg);
        }
    }

    style
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

pub fn render_statusbar(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    use crate::tui::Mode;

    let (content, style) = match &app.mode {
        // Prompt mode: the overlay handles all input display. Show a subtle
        // hint in the status bar so the bottom row isn't blank/confusing.
        Mode::Prompt(crate::tui::ActionKind::Spawn) => (
            " Spawning prompt…  Esc to cancel".to_string(),
            t.text_dim_style(),
        ),
        Mode::Prompt(crate::tui::ActionKind::Plan) => (
            " Plan prompt…  Esc to cancel".to_string(),
            t.text_dim_style(),
        ),
        Mode::Prompt(crate::tui::ActionKind::Qa) => {
            (" QA prompt…  Esc to cancel".to_string(), t.text_dim_style())
        }
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
