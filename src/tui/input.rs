use super::actions::execute_close;
use super::app::{ActionKind, App, ListEntry, Mode};
use crate::registry::Registry;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Burst detection
// ---------------------------------------------------------------------------

/// Inspect a batch of events and, if they look like a paste burst (≥ 3 events
/// that are all printable Key(Char) or Key(Enter)), collect them into a String.
///
/// Returns `None` if:
/// - There is already a real `Event::Paste` in the batch (let that be handled normally).
/// - The batch has fewer than 3 events (could be deliberate rapid typing).
/// - Any event is a non-character key like Ctrl+C, Escape, arrow keys, etc.
pub fn collect_char_burst(events: &[Event]) -> Option<String> {
    // If there's already a proper paste event, don't interfere.
    if events.iter().any(|e| matches!(e, Event::Paste(_))) {
        return None;
    }
    // Need at least 3 events to confidently call it a paste burst.
    if events.len() < 3 {
        return None;
    }
    // Every event must be a printable Key(Char) or Key(Enter).  Any
    // modifier-bearing key (Ctrl, Alt) or non-char key aborts the burst.
    let mut buf = String::with_capacity(events.len());
    for ev in events {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                // Reject modifier combos (Ctrl+anything, Alt+anything).
                if k.modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    return None;
                }
                match k.code {
                    KeyCode::Char(c) => buf.push(c),
                    KeyCode::Enter => buf.push('\n'),
                    // Any other key (Esc, arrows, backspace…) → not a paste burst.
                    _ => return None,
                }
            }
            // Non-key events (resize, focus, …) are fine to ignore; they don't
            // disqualify the burst.
            Event::Key(_) => return None, // Release/repeat events → abort
            _ => {}
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

pub fn handle_key(
    app: &mut App,
    registry: &Registry,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    // Theme picker consumes all keys when open.
    if app.show_theme_picker {
        return handle_theme_picker(app, registry, code);
    }
    // Help overlay: any key closes it.
    if app.show_help {
        app.show_help = false;
        return Ok(());
    }

    match &app.mode.clone() {
        Mode::Normal => handle_normal(app, registry, code),
        Mode::Prompt(kind) => handle_prompt(app, registry, code, modifiers, kind.clone()),
        Mode::ForceConfirm => handle_force_confirm(app, registry, code),
        Mode::ConfirmClose => handle_confirm_close(app, code),
    }
}

fn handle_theme_picker(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.theme_picker_move(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.theme_picker_move(-1);
        }
        KeyCode::Enter => {
            app.theme_picker_commit(registry);
        }
        KeyCode::Esc => {
            app.theme_picker_revert();
        }
        _ => {}
    }
    Ok(())
}

fn handle_normal(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    // If keys are arriving faster than 10 ms apart we are almost certainly in
    // the middle of a paste burst that the terminal chose not to wrap in
    // bracketed-paste markers (or the burst slipped past collect_char_burst
    // because it contained fewer than 3 events).  In that case, suppress any
    // side-effecting single-character command so random letters in the pasted
    // text don't quit the app, open the theme picker, or trigger agent spawns.
    let is_burst = app.last_key_at.elapsed() < Duration::from_millis(10);

    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') if !is_burst => {
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
            app.load_stats_for_selected();
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
            app.load_stats_for_selected();
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        // ── Collapse / expand project section ─────────────────────────────────
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(i) = app.selected() {
                if matches!(app.entries.get(i), Some(ListEntry::ProjectHeader { .. })) {
                    app.toggle_collapse(i, registry);
                }
            }
        }
        // ── Preview pane ──────────────────────────────────────────────────────
        KeyCode::Char('w') if !is_burst => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        // Scroll preview up (further into history) — only when preview visible.
        KeyCode::Char('K') => {
            if app.show_preview && !app.preview_lines.is_empty() {
                app.preview_scroll =
                    (app.preview_scroll + 5).min(app.preview_lines.len().saturating_sub(1));
            }
        }
        // Scroll preview down (toward tail); 0 = auto-tail.
        KeyCode::Char('J') => {
            if app.show_preview {
                app.preview_scroll = app.preview_scroll.saturating_sub(5);
            }
        }
        KeyCode::Char('t') if !is_burst => {
            app.open_theme_picker();
        }
        KeyCode::Char('?') if !is_burst => {
            app.show_help = !app.show_help;
        }
        KeyCode::Char('s') if !is_burst => {
            if !app.require_worktree_selected() {
                return Ok(());
            }
            let phase = app.selected_phase().to_string();
            if App::is_active_phase(&phase) {
                app.set_status(format!(
                    "Warning: {} is [{}] — spawning will kill the running agent. Type prompt and Enter to confirm, Esc to cancel.",
                    app.selected_worktree().map(|w| w.window_name.as_str()).unwrap_or("?"),
                    phase
                ));
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Spawn);
        }
        KeyCode::Char('p') if !is_burst => {
            if !app.require_worktree_selected() {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Plan);
        }
        KeyCode::Char('x') if !is_burst => {
            if !app.require_worktree_selected() {
                return Ok(());
            }
            let phase = app.selected_phase().to_string();
            if App::is_active_phase(&phase) {
                app.set_status(format!(
                    "Warning: {} is [{}] — QA will overwrite the running agent. Enter PR number and press Enter, Esc to cancel.",
                    app.selected_worktree().map(|w| w.window_name.as_str()).unwrap_or("?"),
                    phase
                ));
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Qa);
        }
        KeyCode::Char('m') if !is_burst => {
            let phase = app.selected_phase().to_string();
            if !app.require_worktree_selected() {
                return Ok(());
            }
            if !App::is_active_phase(&phase) {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Send);
        }
        KeyCode::Char('r') if !is_burst => {
            let phase = app.selected_phase().to_string();
            if !app.require_worktree_selected() {
                return Ok(());
            }
            if !App::is_active_phase(&phase) {
                return Ok(());
            }
            if let Some(wt) = app.selected_worktree() {
                let name = wt.window_name.clone();
                match crate::cmd_reset(&name) {
                    Ok(()) => {
                        app.set_status(format!("Reset {} to idle.", name));
                        app.refresh_phases();
                    }
                    Err(e) => app.set_status(format!("Reset failed: {}", e)),
                }
            }
        }
        KeyCode::Char('a') if !is_burst => {
            let phase = app.selected_phase().to_string();
            if !app.require_worktree_selected() {
                return Ok(());
            }
            if !App::is_active_phase(&phase) {
                return Ok(());
            }
            if let Some(wt) = app.selected_worktree() {
                let full_name = format!("{}:{}", wt.window_name, phase);
                let window_name = wt.window_name.clone();
                super::actions::attach_to_window(&app.session, &window_name, &full_name);
            }
        }
        KeyCode::Char('v') if !is_burst => match crate::supervise::cmd_supervise(registry) {
            Ok(()) => {
                let _ = crate::tmux::select_tui_window(&app.session, &app.tui_window_name);
                app.set_status("Supervisor started in 'supervisor' window.".to_string());
                app.refresh_phases();
            }
            Err(e) => app.set_status(format!("Supervise failed: {}", e)),
        },
        KeyCode::Char('c') if !is_burst => {
            if !app.require_worktree_selected() {
                return Ok(());
            }
            app.mode = Mode::ConfirmClose;
        }
        _ => {}
    }
    Ok(())
}

pub fn handle_prompt(
    app: &mut App,
    registry: &Registry,
    code: KeyCode,
    modifiers: KeyModifiers,
    kind: ActionKind,
) -> Result<()> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        // ── Cancel ────────────────────────────────────────────────────────────
        KeyCode::Esc => {
            app.reset_input();
            app.status_msg = None;
        }

        // ── Submit ────────────────────────────────────────────────────────────
        KeyCode::Enter => {
            // If a paste event arrived within 200 ms we're almost certainly
            // inside a bracketed-paste burst — treat the newline as literal
            // text rather than a submit trigger.  200 ms is intentionally
            // generous to accommodate slow/batched terminals.
            if app.last_paste_at.elapsed() < Duration::from_millis(200) {
                app.input_buf.insert(app.cursor_pos, '\n');
                app.cursor_pos += 1;
            } else {
                super::actions::execute_action(app, registry, &kind, false)?;
            }
        }

        // ── Cursor movement ───────────────────────────────────────────────────
        KeyCode::Left if ctrl => {
            // Move to start of previous word.
            app.cursor_pos = prev_word_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Right if ctrl => {
            // Move to end of next word.
            app.cursor_pos = next_word_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Left => {
            app.cursor_pos = prev_char_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Right => {
            app.cursor_pos = next_char_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Home => {
            app.cursor_pos = 0;
        }
        KeyCode::End => {
            app.cursor_pos = app.input_buf.len();
        }

        // ── Ctrl+A / Ctrl+E ───────────────────────────────────────────────────
        KeyCode::Char('a') if ctrl => {
            app.cursor_pos = 0;
        }
        KeyCode::Char('e') if ctrl => {
            app.cursor_pos = app.input_buf.len();
        }

        // ── Ctrl+U: clear from start to cursor ────────────────────────────────
        KeyCode::Char('u') if ctrl => {
            app.input_buf.drain(..app.cursor_pos);
            app.cursor_pos = 0;
        }

        // ── Ctrl+W: delete previous word ──────────────────────────────────────
        KeyCode::Char('w') if ctrl => {
            let new_pos = prev_word_boundary(&app.input_buf, app.cursor_pos);
            app.input_buf.drain(new_pos..app.cursor_pos);
            app.cursor_pos = new_pos;
        }

        // ── Backspace: delete char before cursor ──────────────────────────────
        KeyCode::Backspace => {
            let new_pos = prev_char_boundary(&app.input_buf, app.cursor_pos);
            if new_pos < app.cursor_pos {
                app.input_buf.drain(new_pos..app.cursor_pos);
                app.cursor_pos = new_pos;
            }
        }

        // ── Delete: delete char after cursor ──────────────────────────────────
        KeyCode::Delete => {
            let end = next_char_boundary(&app.input_buf, app.cursor_pos);
            if end > app.cursor_pos {
                app.input_buf.drain(app.cursor_pos..end);
            }
        }

        // ── History: Up/Down ──────────────────────────────────────────────────
        KeyCode::Up => {
            let len = app.input_history.len();
            if len == 0 {
                return Ok(());
            }
            let new_idx = match app.history_idx {
                None => {
                    // Save current draft before browsing.
                    app.history_draft = app.input_buf.clone();
                    len - 1
                }
                Some(0) => 0,
                Some(i) => i - 1,
            };
            app.history_idx = Some(new_idx);
            app.input_buf = app.input_history[new_idx].clone();
            app.cursor_pos = app.input_buf.len();
        }
        KeyCode::Down => {
            match app.history_idx {
                None => {}
                Some(i) if i + 1 >= app.input_history.len() => {
                    // Past the end: restore draft.
                    app.input_buf = app.history_draft.clone();
                    app.cursor_pos = app.input_buf.len();
                    app.history_idx = None;
                }
                Some(i) => {
                    app.history_idx = Some(i + 1);
                    app.input_buf = app.input_history[i + 1].clone();
                    app.cursor_pos = app.input_buf.len();
                }
            }
        }

        // ── Regular character insertion ────────────────────────────────────────
        KeyCode::Char(c) if !ctrl => {
            app.input_buf.insert(app.cursor_pos, c);
            app.cursor_pos += c.len_utf8();
            // Typing exits history browsing.
            app.history_idx = None;
        }

        _ => {}
    }
    Ok(())
}

pub fn handle_force_confirm(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.reset_input();
            app.status_msg = None;
        }
        KeyCode::Enter => {
            super::actions::execute_spawn(app, registry, true)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_close(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            execute_close(app)?;
        }
        _ => {
            // Any other key cancels.
            app.mode = Mode::Normal;
            app.status_msg = None;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cursor helpers
// ---------------------------------------------------------------------------

/// Returns the byte offset of the previous UTF-8 char boundary before `pos`.
pub fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Returns the byte offset of the next UTF-8 char boundary after `pos`.
pub fn next_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

/// Returns the byte offset of the start of the previous word (skips trailing
/// whitespace then alphanumeric chars).
pub fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut p = pos;
    // Skip whitespace backwards.
    while p > 0 && (bytes[p - 1] as char).is_whitespace() {
        p -= 1;
    }
    // Skip non-whitespace backwards.
    while p > 0 && !(bytes[p - 1] as char).is_whitespace() {
        p -= 1;
    }
    p
}

/// Returns the byte offset just past the end of the next word.
pub fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut p = pos;
    // Skip whitespace forwards.
    while p < len && (bytes[p] as char).is_whitespace() {
        p += 1;
    }
    // Skip non-whitespace forwards.
    while p < len && !(bytes[p] as char).is_whitespace() {
        p += 1;
    }
    p
}
