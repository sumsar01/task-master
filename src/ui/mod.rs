pub mod help;
pub mod main_screen;
pub mod prompt;
pub mod theme;
pub mod theme_picker;

use crate::tui::App;
use ratatui::{widgets::Block, Frame};

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
}
