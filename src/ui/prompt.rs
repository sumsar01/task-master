use crate::tui::{ActionKind, App, Mode};
use crate::ui::theme::Theme;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

/// Returns a bottom-anchored overlay rect that is `height` rows tall and
/// spans the full terminal width.
fn prompt_rect(frame_area: Rect, height: u16) -> Rect {
    let h = height.min(frame_area.height);
    Rect {
        x: 0,
        y: frame_area.height.saturating_sub(h),
        width: frame_area.width,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Public render entry
// ---------------------------------------------------------------------------

pub fn render(f: &mut Frame, app: &App, t: &Theme) {
    let (title, _is_prompt) = match &app.mode {
        Mode::Prompt(ActionKind::Spawn) => (" Spawn Agent ", true),
        Mode::Prompt(ActionKind::Plan) => (" Plan Task ", true),
        Mode::Prompt(ActionKind::Qa) => (" QA — Enter PR # ", true),
        Mode::Prompt(ActionKind::Send) => (" Send Message ", true),
        Mode::Prompt(ActionKind::SendBuild) => (" Send Message (Build Mode) ", true),
        Mode::Prompt(ActionKind::AddWorktree) => (" New Worktree — Enter name ", true),
        Mode::Prompt(ActionKind::AddProject) => {
            use crate::tui::AddProjectStep;
            match &app.add_project_step {
                Some(AddProjectStep::Name) => (" Add Project — Enter full name ", true),
                Some(AddProjectStep::Short) => (" Add Project — Enter short name ", true),
                Some(AddProjectStep::Url) => (" Add Project — Enter git repo URL ", true),
                Some(AddProjectStep::Account) => (" Add Project — Enter gh account ", true),
                Some(AddProjectStep::Group) => (" Add Project — Enter group (Tab to cycle) ", true),
                Some(AddProjectStep::Context) => {
                    (" Add Project — Enter bounded context (Tab to cycle) ", true)
                }
                None => return,
            }
        }
        Mode::Prompt(ActionKind::SpawnEphemeral) => (" Spawn Ephemeral — Enter task prompt ", true),
        _ => return,
    };

    // Compute how many content rows we need.
    // Inner width = terminal_width - 2 (borders)
    let inner_width = (f.area().width as usize).saturating_sub(2).max(1);
    let input = &app.input_buf;
    // Count visual lines: each hard newline starts a new line, and soft-wrap
    // every `inner_width` columns.
    let content_rows = {
        let mut rows = 0usize;
        for line in input.split('\n') {
            let chars = line.chars().count();
            rows += (chars / inner_width) + 1;
        }
        // Always at least one row even for empty input.
        rows.max(1)
    };
    // Overlay height = border top + hint line + content rows (min 1, max 8) + border bottom
    let overlay_height = (2 + 1 + content_rows.clamp(1, 8)) as u16;

    let area = prompt_rect(f.area(), overlay_height);
    f.render_widget(Clear, area);

    // Outer block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border_style())
        .title(Span::styled(title, t.title_style()));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split inner into: hint row (1) + input rows (rest)
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // ── Hint line ────────────────────────────────────────────────────────────
    let has_history = !app.input_history.is_empty();
    let mut hint_spans = vec![
        Span::styled(" Enter", t.key_desc_style()),
        Span::raw(" submit"),
        Span::styled("  ·  ", t.text_dim_style()),
        Span::styled("Esc", t.key_desc_style()),
        Span::raw(" cancel"),
    ];
    if has_history {
        hint_spans.extend([
            Span::styled("  ·  ", t.text_dim_style()),
            Span::styled("↑↓", t.key_desc_style()),
            Span::raw(" move/history"),
        ]);
    } else {
        hint_spans.extend([
            Span::styled("  ·  ", t.text_dim_style()),
            Span::styled("↑↓", t.key_desc_style()),
            Span::raw(" move"),
        ]);
    }
    hint_spans.extend([
        Span::styled("  ·  ", t.text_dim_style()),
        Span::styled("^A/^E", t.key_desc_style()),
        Span::raw(" start/end"),
        Span::styled("  ·  ", t.text_dim_style()),
        Span::styled("^W", t.key_desc_style()),
        Span::raw(" del-word"),
    ]);

    f.render_widget(
        Paragraph::new(Line::from(hint_spans)).style(t.text_dim_style()),
        rows[0],
    );

    // ── Input area with cursor ────────────────────────────────────────────────
    // Build spans: text before cursor, cursor char (reversed), text after cursor.
    let cursor_pos = app.cursor_pos.min(input.len());
    let before = &input[..cursor_pos];
    let after = &input[cursor_pos..];

    // The cursor character is the first char after the cursor position,
    // or a space if we're at the end.
    let (cursor_char, after_cursor) = if after.is_empty() {
        (" ", "")
    } else {
        let boundary = after
            .char_indices()
            .nth(1)
            .map(|(i, _)| i)
            .unwrap_or(after.len());
        (&after[..boundary], &after[boundary..])
    };

    // We need to build multi-line content: split on '\n' and render lines.
    // The cursor splits the text somewhere — find which line the cursor lands on.
    let lines_before: Vec<&str> = before.split('\n').collect();
    let last_before = lines_before.last().copied().unwrap_or("");

    let mut lines: Vec<Line> = Vec::new();

    // All lines before the cursor's line
    for line in &lines_before[..lines_before.len().saturating_sub(1)] {
        lines.push(Line::from(Span::styled(*line, t.text_style())));
    }

    // The cursor line: last_before + cursor_char (reversed) + first part of after
    let after_parts: Vec<&str> = after_cursor.splitn(2, '\n').collect();
    let after_on_cursor_line = after_parts.first().copied().unwrap_or("");
    let after_remaining = after_parts.get(1).copied().unwrap_or("");

    let cursor_line = Line::from(vec![
        Span::styled(last_before, t.text_style()),
        Span::styled(cursor_char, t.text_style().add_modifier(Modifier::REVERSED)),
        Span::styled(after_on_cursor_line, t.text_style()),
    ]);
    lines.push(cursor_line);

    // Any remaining lines after the cursor line
    for line in after_remaining.split('\n') {
        lines.push(Line::from(Span::styled(line, t.text_style())));
    }

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.prompt_scroll as u16, 0)),
        rows[1],
    );
}
