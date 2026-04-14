use crate::stats::format_tokens;
use crate::tui::{App, ListEntry};
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

/// Background color for the agent preview pane — always dark regardless of
/// the active theme, giving the pane a consistent "embedded terminal" look.
const PREVIEW_BG: Color = Color::Rgb(10, 10, 16);

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
        ("c", "close"),
        ("a", "attach"),
        ("N", "new wt"),
        ("D", "remove wt"),
        ("v", "supervise"),
        ("d", "detail"),
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
const ACTIONS_LINES: u16 = 21;

/// Fixed height of the detail pane when it shares the right column with
/// another pane (preview).  Border adds 2, so actual block height = DETAIL_LINES + 2.
const DETAIL_LINES: u16 = 10;

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

    match (app.show_detail, app.show_preview) {
        // ── Actions only ──────────────────────────────────────────────────────
        (false, false) => {
            render_actions(f, cols[1], app, t);
        }
        // ── Actions + Preview ─────────────────────────────────────────────────
        (false, true) => {
            let actions_height = (ACTIONS_LINES + 2).min(cols[1].height);
            let right_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(actions_height), Constraint::Min(0)])
                .split(cols[1]);
            render_actions(f, right_rows[0], app, t);
            render_preview(f, right_rows[1], app, t);
        }
        // ── Actions + Detail ──────────────────────────────────────────────────
        (true, false) => {
            let actions_height = (ACTIONS_LINES + 2).min(cols[1].height);
            let right_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(actions_height), Constraint::Min(0)])
                .split(cols[1]);
            render_actions(f, right_rows[0], app, t);
            render_detail(f, right_rows[1], app, t);
        }
        // ── Actions + Detail + Preview ────────────────────────────────────────
        (true, true) => {
            let actions_height = (ACTIONS_LINES + 2).min(cols[1].height);
            let detail_height =
                (DETAIL_LINES + 2).min(cols[1].height.saturating_sub(actions_height));
            let right_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(actions_height),
                    Constraint::Length(detail_height),
                    Constraint::Min(0),
                ])
                .split(cols[1]);
            render_actions(f, right_rows[0], app, t);
            render_detail(f, right_rows[1], app, t);
            render_preview(f, right_rows[2], app, t);
        }
    }
}

// ---------------------------------------------------------------------------
// Worktree list
// ---------------------------------------------------------------------------

fn render_worktree_list(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) {
    let selected = app.list_state.selected();

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .enumerate()
        .map(|(visual_idx, entry)| {
            let is_selected = selected == Some(visual_idx);
            let base = if is_selected {
                t.selection_style()
            } else {
                Style::default()
            };

            match entry {
                ListEntry::GroupHeader { name, collapsed } => {
                    let icon = if *collapsed { "○" } else { "●" };
                    let line = Line::from(vec![Span::styled(
                        format!("{} {} ", icon, name),
                        base.fg(t.text_accent).add_modifier(Modifier::BOLD),
                    )]);
                    ListItem::new(line)
                }
                ListEntry::ProjectHeader {
                    name, collapsed, ..
                } => {
                    let icon = if *collapsed { "▶" } else { "▼" };
                    let line = Line::from(vec![Span::styled(
                        format!("  {} {} ", icon, name),
                        base.fg(t.section_header).add_modifier(Modifier::BOLD),
                    )]);
                    ListItem::new(line)
                }
                ListEntry::Worktree { wt, worktree_idx } => {
                    let phase = app
                        .phases
                        .get(*worktree_idx)
                        .map(|s| s.as_str())
                        .unwrap_or("?");
                    let phase_color = t.phase_color(phase);
                    let line = Line::from(vec![
                        Span::styled(format!("      {:<18}", wt.window_name), base.fg(t.text)),
                        Span::styled(format!("[{}]", phase), base.fg(phase_color)),
                    ]);
                    ListItem::new(line)
                }
                ListEntry::EmptyProject => {
                    let line = Line::from(vec![Span::styled(
                        "      (no worktrees)",
                        Style::default().fg(t.text_dim),
                    )]);
                    ListItem::new(line)
                }
            }
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
    lines.push(action_line('m', "send message", has_wt, !active, t));
    lines.push(Line::from(""));
    lines.push(action_line('r', "reset window", has_wt, !active, t));
    lines.push(action_line('a', "attach", has_wt, !active, t));
    lines.push(action_line('c', "close window", has_wt, false, t));
    lines.push(Line::from(""));
    lines.push(action_line('N', "new worktree", has_wt, false, t));
    lines.push(action_line('D', "remove worktree", has_wt, !has_wt, t));
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
// Worktree detail pane
// ---------------------------------------------------------------------------

fn render_detail(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let title = match app.selected_worktree() {
        Some(wt) => format!(" {} — detail ", wt.window_name),
        None => " Detail ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(title, t.title_style()));

    if app.detail_lines.is_empty() {
        let msg = Paragraph::new(Span::styled("  No detail available", t.text_dim_style()));
        f.render_widget(msg.block(block), area);
        return;
    }

    let lines: Vec<Line> = app
        .detail_lines
        .iter()
        .map(|s| {
            if s.starts_with("Branch:") || s.starts_with("Status:") || s.starts_with("Recent") {
                Line::from(Span::styled(
                    format!("  {}", s),
                    t.text_style().add_modifier(Modifier::BOLD),
                ))
            } else if s.is_empty() {
                Line::from("")
            } else {
                Line::from(Span::styled(format!("  {}", s), t.text_dim_style()))
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ---------------------------------------------------------------------------
// Agent preview pane
// ---------------------------------------------------------------------------

fn render_preview(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    // Determine title from selected worktree + phase.
    let title = match app.selected_worktree() {
        Some(wt) => {
            let name = wt.window_name.as_str();
            let phase = app
                .selected_worktree_idx()
                .and_then(|i| app.phases.get(i))
                .map(|s| s.as_str())
                .unwrap_or("?");
            if app.preview_scroll > 0 {
                format!(" {}:{} ↑ scrolled ", name, phase)
            } else {
                format!(" {}:{} ", name, phase)
            }
        }
        None => " Preview ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(title, t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Fill the preview interior with a fixed dark background so opencode's
    // ANSI colors (designed for a dark canvas) read correctly on any theme.
    f.render_widget(
        Block::default().style(Style::default().bg(PREVIEW_BG)),
        inner,
    );

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

    // Parse each line's ANSI escape sequences into ratatui spans, then strip
    // background colors — the pane has its own uniform dark bg, so per-span
    // backgrounds from opencode would create patchy rectangles. Foreground
    // colors (greens, yellows, blues, reds, dim greys, near-whites) all read
    // fine on the dark canvas and are left untouched.
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
                                let style = normalize_ansi_style(span.style);
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
/// - Remove background color — the pane fills its own dark bg uniformly,
///   so per-span backgrounds from opencode would create patchy rectangles.
fn normalize_ansi_style(mut style: Style) -> Style {
    style.bg = None;
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
        Mode::Prompt(crate::tui::ActionKind::Send) => (
            " Send message…  Esc to cancel".to_string(),
            t.text_dim_style(),
        ),
        Mode::Prompt(crate::tui::ActionKind::AddWorktree) => (
            " New worktree name…  Esc to cancel".to_string(),
            t.text_dim_style(),
        ),
        Mode::ForceConfirm => (
            " Press Enter to force-spawn (discards uncommitted changes), Esc to cancel".to_string(),
            t.text_style().fg(t.phase_error),
        ),
        Mode::ConfirmClose => (String::new(), t.text_dim_style()),
        Mode::ConfirmRemoveWorktree => (String::new(), t.text_dim_style()),
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
    let wt_idx = match app.selected_worktree_idx() {
        Some(i) => i,
        None => return " No worktree selected".to_string(),
    };
    let wt = match app.worktrees.get(wt_idx) {
        Some(w) => w,
        None => return String::new(),
    };
    match app.stats_cache.get(&wt_idx) {
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
