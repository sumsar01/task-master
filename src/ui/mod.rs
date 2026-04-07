pub mod confirm_close;
pub mod help;
pub mod main_screen;
pub mod prompt;
pub mod theme;
pub mod theme_picker;
pub mod update_prompt;

use crate::tui::App;
use ratatui::{Frame, widgets::Block};

/// Top-level render dispatcher.
/// Draws the active screen, then overlays on top.
pub fn render(f: &mut Frame, app: &mut App) {
    let t = app.theme.clone();

    // Fill the entire frame with the theme background first, so no black
    // bleed-through appears behind any widget.
    f.render_widget(Block::default().style(t.bg_style()), f.area());

    // Only one screen for now — the main worktree list.
    main_screen::render(f, app, &t);

    // Overlays drawn last (on top of everything).
    if app.show_help {
        help::render(f, &t);
    }
    if app.show_theme_picker {
        theme_picker::render(f, app, &t);
    }

    // Prompt overlay is shown when the user is typing a command, and also in
    // ForceConfirm so the user can see what they're about to confirm.
    if matches!(
        app.mode,
        crate::tui::Mode::Prompt(_) | crate::tui::Mode::ForceConfirm
    ) {
        prompt::render(f, app, &t);
    }

    // Confirm-close modal — shown on top of everything when closing a window.
    if app.mode == crate::tui::Mode::ConfirmClose {
        confirm_close::render(f, app, &t);
    }

    // Update-available / downloading overlay — shown highest (last drawn).
    if matches!(
        app.mode,
        crate::tui::Mode::UpdateAvailable(_) | crate::tui::Mode::Updating
    ) {
        update_prompt::render(f, app, &t);
    }
}
