use ratatui::style::{Color, Modifier, Style};

// ---------------------------------------------------------------------------
// Theme struct
// ---------------------------------------------------------------------------

/// All fields are public and intentionally defined even if not yet used in
/// all screens — reserved for future panels and overlays.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Theme {
    // ── Chrome ────────────────────────────────────────────────────────────────
    pub bg: Color,         // Base background (fills the whole terminal)
    pub border: Color,     // Primary border color (blocks, panels)
    pub border_dim: Color, // Dimmer border (inner / secondary panels)

    // ── Text ─────────────────────────────────────────────────────────────────
    pub text: Color,        // Normal body text
    pub text_dim: Color,    // Secondary / muted text (timestamps, labels)
    pub text_accent: Color, // Accent text (paths, branch names)

    // ── PR list ───────────────────────────────────────────────────────────────
    pub pr_number: Color, // #123 PR number
    pub pr_author: Color, // Author name
    pub pr_draft: Color,  // DRAFT badge text

    // ── List / Selection ─────────────────────────────────────────────────────
    pub selection_bg: Color,
    pub selection_fg: Color,

    // ── Diff ─────────────────────────────────────────────────────────────────
    pub diff_added_fg: Color,
    pub diff_added_bg: Color, // Full-row background tint
    pub diff_removed_fg: Color,
    pub diff_removed_bg: Color,
    pub diff_context: Color, // Unchanged lines
    pub diff_hunk: Color,    // @@ hunk header lines

    // ── Tabs ─────────────────────────────────────────────────────────────────
    pub tab_active: Color,
    pub tab_inactive: Color,

    // ── Key hint badges ───────────────────────────────────────────────────────
    pub key_fg: Color,   // Dark fg so the badge bg is readable
    pub key_bg: Color,   // Badge background (usually a warm accent)
    pub key_desc: Color, // Description text next to badge

    // ── Stats ─────────────────────────────────────────────────────────────────
    pub stats_added: Color,
    pub stats_removed: Color,

    // ── Misc ─────────────────────────────────────────────────────────────────
    pub section_header: Color, // Section headings in overlays
    pub separator: Color,      // ── separator lines

    // ── Phase colors ─────────────────────────────────────────────────────────
    pub phase_dev: Color,
    pub phase_plan_qa: Color,
    pub phase_done: Color,
    pub phase_idle: Color,
    pub phase_error: Color,
}

// ---------------------------------------------------------------------------
// Convenience style builders
// ---------------------------------------------------------------------------

#[allow(dead_code)]
impl Theme {
    pub fn bg_style(&self) -> Style {
        Style::default().bg(self.bg)
    }
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }
    pub fn border_dim_style(&self) -> Style {
        Style::default().fg(self.border_dim)
    }
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }
    pub fn text_dim_style(&self) -> Style {
        Style::default().fg(self.text_dim)
    }
    pub fn text_accent_style(&self) -> Style {
        Style::default().fg(self.text_accent)
    }

    pub fn selection_style(&self) -> Style {
        Style::default().bg(self.selection_bg).fg(self.selection_fg)
    }

    pub fn tab_active_style(&self) -> Style {
        Style::default()
            .fg(self.tab_active)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED)
    }
    pub fn tab_inactive_style(&self) -> Style {
        Style::default().fg(self.tab_inactive)
    }

    pub fn key_badge_style(&self) -> Style {
        Style::default()
            .fg(self.key_fg)
            .bg(self.key_bg)
            .add_modifier(Modifier::BOLD)
    }
    pub fn key_desc_style(&self) -> Style {
        Style::default().fg(self.key_desc)
    }

    pub fn section_header_style(&self) -> Style {
        Style::default()
            .fg(self.section_header)
            .add_modifier(Modifier::BOLD)
    }
    pub fn separator_style(&self) -> Style {
        Style::default().fg(self.separator)
    }

    pub fn title_style(&self) -> Style {
        Style::default()
            .fg(self.text_accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn diff_added_style(&self) -> Style {
        Style::default()
            .fg(self.diff_added_fg)
            .bg(self.diff_added_bg)
    }
    pub fn diff_removed_style(&self) -> Style {
        Style::default()
            .fg(self.diff_removed_fg)
            .bg(self.diff_removed_bg)
    }
    pub fn diff_context_style(&self) -> Style {
        Style::default().fg(self.diff_context)
    }
    pub fn diff_hunk_style(&self) -> Style {
        Style::default()
            .fg(self.diff_hunk)
            .add_modifier(Modifier::BOLD)
    }
}

// ---------------------------------------------------------------------------
// Phase color helper
// ---------------------------------------------------------------------------

impl Theme {
    pub fn phase_color(&self, phase: &str) -> Color {
        match phase {
            "dev" => self.phase_dev,
            "plan" | "qa" => self.phase_plan_qa,
            "review" | "ready" => self.phase_done,
            "idle" | "?" | "" => self.phase_idle,
            p if p.ends_with("stalled") => self.phase_error,
            p if p == "blocked" => self.phase_error,
            _ => self.text,
        }
    }
}

// ---------------------------------------------------------------------------
// Theme registry
// ---------------------------------------------------------------------------

/// (id, display name) — order is the order shown in the picker
pub const ALL_THEMES: &[(&str, &str)] = &[
    ("tokyonight", "Tokyo Night"),            // dark
    ("gruvbox", "Gruvbox Dark"),              // dark
    ("catppuccin", "Catppuccin Mocha"),       // dark
    ("nord", "Nord"),                         // dark
    ("dracula", "Dracula"),                   // dark
    ("rosepine", "Rosé Pine"),                // dark
    ("github_light", "GitHub Light"),         // light
    ("catppuccin_latte", "Catppuccin Latte"), // light
    ("rosepine_dawn", "Rosé Pine Dawn"),      // light
    ("gruvbox_light", "Gruvbox Light"),       // light
    ("solarized_light", "Solarized Light"),   // light
];

impl Theme {
    /// Resolve by id, falling back to the default on unknown names.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "tokyonight" => Self::tokyonight(),
            "gruvbox" => Self::gruvbox(),
            "catppuccin" => Self::catppuccin(),
            "nord" => Self::nord(),
            "dracula" => Self::dracula(),
            "rosepine" => Self::rosepine(),
            "github_light" => Self::github_light(),
            "catppuccin_latte" => Self::catppuccin_latte(),
            "rosepine_dawn" => Self::rosepine_dawn(),
            "gruvbox_light" => Self::gruvbox_light(),
            "solarized_light" => Self::solarized_light(),
            _ => Self::tokyonight(),
        }
    }

    /// Index in ALL_THEMES (for the picker cursor).
    pub fn index_of(name: &str) -> usize {
        let lower = name.to_lowercase();
        ALL_THEMES
            .iter()
            .position(|(id, _)| *id == lower.as_str())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Theme constructors
// ---------------------------------------------------------------------------

impl Theme {
    pub fn tokyonight() -> Self {
        Self {
            bg: Color::Rgb(26, 27, 38),                 // night bg
            border: Color::Rgb(86, 95, 137),            // storm border
            border_dim: Color::Rgb(54, 58, 79),         // dim border
            text: Color::Rgb(192, 202, 245),            // fg
            text_dim: Color::Rgb(86, 95, 137),          // comment
            text_accent: Color::Rgb(122, 162, 247),     // blue
            pr_number: Color::Rgb(187, 154, 247),       // purple
            pr_author: Color::Rgb(122, 162, 247),       // blue
            pr_draft: Color::Rgb(86, 95, 137),          // comment/muted
            selection_bg: Color::Rgb(44, 50, 75),       // highlight bg
            selection_fg: Color::Rgb(192, 202, 245),    // fg
            diff_added_fg: Color::Rgb(158, 206, 106),   // green
            diff_added_bg: Color::Rgb(31, 44, 28),      // dark green tint
            diff_removed_fg: Color::Rgb(247, 118, 142), // red
            diff_removed_bg: Color::Rgb(52, 28, 35),    // dark red tint
            diff_context: Color::Rgb(86, 95, 137),      // comment
            diff_hunk: Color::Rgb(122, 162, 247),       // blue
            tab_active: Color::Rgb(122, 162, 247),      // blue
            tab_inactive: Color::Rgb(86, 95, 137),      // comment
            key_fg: Color::Rgb(26, 27, 38),             // bg (dark for contrast)
            key_bg: Color::Rgb(187, 154, 247),          // purple
            key_desc: Color::Rgb(122, 162, 247),        // blue
            stats_added: Color::Rgb(158, 206, 106),     // green
            stats_removed: Color::Rgb(247, 118, 142),   // red
            section_header: Color::Rgb(187, 154, 247),  // purple
            separator: Color::Rgb(54, 58, 79),          // dim border
            phase_dev: Color::Rgb(224, 175, 104),       // yellow/orange
            phase_plan_qa: Color::Rgb(122, 162, 247),   // blue
            phase_done: Color::Rgb(158, 206, 106),      // green
            phase_idle: Color::Rgb(86, 95, 137),        // comment
            phase_error: Color::Rgb(247, 118, 142),     // red
        }
    }

    pub fn gruvbox() -> Self {
        Self {
            bg: Color::Rgb(40, 40, 40),               // bg0
            border: Color::Rgb(146, 131, 116),        // gruvbox4
            border_dim: Color::Rgb(80, 73, 69),       // bg3
            text: Color::Rgb(235, 219, 178),          // fg1
            text_dim: Color::Rgb(146, 131, 116),      // gruvbox4
            text_accent: Color::Rgb(131, 165, 152),   // aqua
            pr_number: Color::Rgb(250, 189, 47),      // yellow
            pr_author: Color::Rgb(131, 165, 152),     // aqua
            pr_draft: Color::Rgb(146, 131, 116),      // gruvbox4
            selection_bg: Color::Rgb(80, 73, 69),     // bg3
            selection_fg: Color::Rgb(235, 219, 178),  // fg1
            diff_added_fg: Color::Rgb(184, 187, 38),  // green
            diff_added_bg: Color::Rgb(36, 40, 25),    // dark green tint
            diff_removed_fg: Color::Rgb(251, 73, 52), // red
            diff_removed_bg: Color::Rgb(50, 22, 18),  // dark red tint
            diff_context: Color::Rgb(146, 131, 116),  // gruvbox4
            diff_hunk: Color::Rgb(131, 165, 152),     // aqua
            tab_active: Color::Rgb(250, 189, 47),     // yellow
            tab_inactive: Color::Rgb(146, 131, 116),  // gruvbox4
            key_fg: Color::Rgb(40, 40, 40),           // bg0
            key_bg: Color::Rgb(214, 93, 14),          // orange
            key_desc: Color::Rgb(131, 165, 152),      // aqua
            stats_added: Color::Rgb(184, 187, 38),    // green
            stats_removed: Color::Rgb(251, 73, 52),   // red
            section_header: Color::Rgb(250, 189, 47), // yellow
            separator: Color::Rgb(80, 73, 69),        // bg3
            phase_dev: Color::Rgb(250, 189, 47),      // yellow
            phase_plan_qa: Color::Rgb(131, 165, 152), // aqua
            phase_done: Color::Rgb(184, 187, 38),     // green
            phase_idle: Color::Rgb(146, 131, 116),    // gruvbox4
            phase_error: Color::Rgb(251, 73, 52),     // red
        }
    }

    pub fn catppuccin() -> Self {
        Self {
            bg: Color::Rgb(30, 30, 46),                 // base
            border: Color::Rgb(108, 112, 134),          // overlay0
            border_dim: Color::Rgb(88, 91, 112),        // surface2
            text: Color::Rgb(205, 214, 244),            // text
            text_dim: Color::Rgb(108, 112, 134),        // overlay0
            text_accent: Color::Rgb(137, 180, 250),     // blue
            pr_number: Color::Rgb(203, 166, 247),       // mauve
            pr_author: Color::Rgb(137, 180, 250),       // blue
            pr_draft: Color::Rgb(108, 112, 134),        // overlay0
            selection_bg: Color::Rgb(88, 91, 112),      // surface2
            selection_fg: Color::Rgb(205, 214, 244),    // text
            diff_added_fg: Color::Rgb(166, 227, 161),   // green
            diff_added_bg: Color::Rgb(30, 46, 38),      // dark green tint
            diff_removed_fg: Color::Rgb(243, 139, 168), // red
            diff_removed_bg: Color::Rgb(52, 28, 38),    // dark red tint
            diff_context: Color::Rgb(108, 112, 134),    // overlay0
            diff_hunk: Color::Rgb(137, 180, 250),       // blue
            tab_active: Color::Rgb(203, 166, 247),      // mauve
            tab_inactive: Color::Rgb(108, 112, 134),    // overlay0
            key_fg: Color::Rgb(30, 30, 46),             // base (dark)
            key_bg: Color::Rgb(245, 194, 231),          // pink
            key_desc: Color::Rgb(137, 180, 250),        // blue
            stats_added: Color::Rgb(166, 227, 161),     // green
            stats_removed: Color::Rgb(243, 139, 168),   // red
            section_header: Color::Rgb(203, 166, 247),  // mauve
            separator: Color::Rgb(88, 91, 112),         // surface2
            phase_dev: Color::Rgb(249, 226, 175),       // yellow
            phase_plan_qa: Color::Rgb(137, 180, 250),   // blue
            phase_done: Color::Rgb(166, 227, 161),      // green
            phase_idle: Color::Rgb(108, 112, 134),      // overlay0
            phase_error: Color::Rgb(243, 139, 168),     // red
        }
    }

    pub fn nord() -> Self {
        Self {
            bg: Color::Rgb(46, 52, 64),                // nord0
            border: Color::Rgb(76, 86, 106),           // nord3
            border_dim: Color::Rgb(59, 66, 82),        // nord1
            text: Color::Rgb(216, 222, 233),           // nord4
            text_dim: Color::Rgb(76, 86, 106),         // nord3
            text_accent: Color::Rgb(136, 192, 208),    // nord8
            pr_number: Color::Rgb(129, 161, 193),      // nord9
            pr_author: Color::Rgb(136, 192, 208),      // nord8
            pr_draft: Color::Rgb(76, 86, 106),         // nord3
            selection_bg: Color::Rgb(67, 76, 94),      // nord2
            selection_fg: Color::Rgb(216, 222, 233),   // nord4
            diff_added_fg: Color::Rgb(163, 190, 140),  // nord14 green
            diff_added_bg: Color::Rgb(32, 46, 36),     // dark green tint
            diff_removed_fg: Color::Rgb(191, 97, 106), // nord11 red
            diff_removed_bg: Color::Rgb(46, 30, 32),   // dark red tint
            diff_context: Color::Rgb(76, 86, 106),     // nord3
            diff_hunk: Color::Rgb(136, 192, 208),      // nord8
            tab_active: Color::Rgb(136, 192, 208),     // nord8
            tab_inactive: Color::Rgb(76, 86, 106),     // nord3
            key_fg: Color::Rgb(46, 52, 64),            // nord0 (dark)
            key_bg: Color::Rgb(163, 190, 140),         // nord14 green
            key_desc: Color::Rgb(136, 192, 208),       // nord8
            stats_added: Color::Rgb(163, 190, 140),    // nord14 green
            stats_removed: Color::Rgb(191, 97, 106),   // nord11 red
            section_header: Color::Rgb(129, 161, 193), // nord9
            separator: Color::Rgb(59, 66, 82),         // nord1
            phase_dev: Color::Rgb(235, 203, 139),      // nord13 yellow
            phase_plan_qa: Color::Rgb(136, 192, 208),  // nord8
            phase_done: Color::Rgb(163, 190, 140),     // nord14 green
            phase_idle: Color::Rgb(76, 86, 106),       // nord3
            phase_error: Color::Rgb(191, 97, 106),     // nord11 red
        }
    }

    pub fn dracula() -> Self {
        Self {
            bg: Color::Rgb(40, 42, 54),                // background
            border: Color::Rgb(98, 114, 164),          // selection
            border_dim: Color::Rgb(68, 71, 90),        // current line
            text: Color::Rgb(248, 248, 242),           // foreground
            text_dim: Color::Rgb(98, 114, 164),        // selection/comment
            text_accent: Color::Rgb(139, 233, 253),    // cyan
            pr_number: Color::Rgb(189, 147, 249),      // purple
            pr_author: Color::Rgb(139, 233, 253),      // cyan
            pr_draft: Color::Rgb(98, 114, 164),        // selection/comment
            selection_bg: Color::Rgb(68, 71, 90),      // current line
            selection_fg: Color::Rgb(248, 248, 242),   // foreground
            diff_added_fg: Color::Rgb(80, 250, 123),   // green
            diff_added_bg: Color::Rgb(24, 50, 34),     // dark green tint
            diff_removed_fg: Color::Rgb(255, 85, 85),  // red
            diff_removed_bg: Color::Rgb(50, 24, 24),   // dark red tint
            diff_context: Color::Rgb(98, 114, 164),    // selection/comment
            diff_hunk: Color::Rgb(139, 233, 253),      // cyan
            tab_active: Color::Rgb(189, 147, 249),     // purple
            tab_inactive: Color::Rgb(98, 114, 164),    // selection
            key_fg: Color::Rgb(40, 42, 54),            // background (dark)
            key_bg: Color::Rgb(255, 121, 198),         // pink
            key_desc: Color::Rgb(139, 233, 253),       // cyan
            stats_added: Color::Rgb(80, 250, 123),     // green
            stats_removed: Color::Rgb(255, 85, 85),    // red
            section_header: Color::Rgb(189, 147, 249), // purple
            separator: Color::Rgb(68, 71, 90),         // current line
            phase_dev: Color::Rgb(241, 250, 140),      // yellow
            phase_plan_qa: Color::Rgb(139, 233, 253),  // cyan
            phase_done: Color::Rgb(80, 250, 123),      // green
            phase_idle: Color::Rgb(98, 114, 164),      // selection
            phase_error: Color::Rgb(255, 85, 85),      // red
        }
    }

    pub fn rosepine() -> Self {
        Self {
            bg: Color::Rgb(25, 23, 36),                 // base
            border: Color::Rgb(110, 106, 134),          // muted
            border_dim: Color::Rgb(64, 60, 83),         // overlay
            text: Color::Rgb(224, 222, 244),            // text
            text_dim: Color::Rgb(110, 106, 134),        // muted
            text_accent: Color::Rgb(156, 207, 216),     // foam
            pr_number: Color::Rgb(196, 167, 231),       // iris
            pr_author: Color::Rgb(156, 207, 216),       // foam
            pr_draft: Color::Rgb(110, 106, 134),        // muted
            selection_bg: Color::Rgb(64, 60, 83),       // overlay
            selection_fg: Color::Rgb(224, 222, 244),    // text
            diff_added_fg: Color::Rgb(49, 116, 143),    // pine
            diff_added_bg: Color::Rgb(22, 44, 54),      // dark pine tint
            diff_removed_fg: Color::Rgb(235, 111, 146), // love/red
            diff_removed_bg: Color::Rgb(50, 24, 34),    // dark love tint
            diff_context: Color::Rgb(110, 106, 134),    // muted
            diff_hunk: Color::Rgb(156, 207, 216),       // foam
            tab_active: Color::Rgb(196, 167, 231),      // iris
            tab_inactive: Color::Rgb(110, 106, 134),    // muted
            key_fg: Color::Rgb(25, 23, 36),             // base (dark)
            key_bg: Color::Rgb(235, 188, 186),          // rose
            key_desc: Color::Rgb(156, 207, 216),        // foam
            stats_added: Color::Rgb(49, 116, 143),      // pine
            stats_removed: Color::Rgb(235, 111, 146),   // love/red
            section_header: Color::Rgb(196, 167, 231),  // iris
            separator: Color::Rgb(64, 60, 83),          // overlay
            phase_dev: Color::Rgb(246, 193, 119),       // gold
            phase_plan_qa: Color::Rgb(156, 207, 216),   // foam
            phase_done: Color::Rgb(49, 116, 143),       // pine
            phase_idle: Color::Rgb(110, 106, 134),      // muted
            phase_error: Color::Rgb(235, 111, 146),     // love/red
        }
    }

    // ── Light themes ──────────────────────────────────────────────────────────

    pub fn github_light() -> Self {
        Self {
            bg: Color::Rgb(255, 255, 255),              // canvas default
            border: Color::Rgb(208, 215, 222),          // border default
            border_dim: Color::Rgb(225, 228, 232),      // border muted
            text: Color::Rgb(31, 35, 40),               // fg default
            text_dim: Color::Rgb(101, 109, 118),        // fg muted
            text_accent: Color::Rgb(9, 105, 218),       // accent fg (blue)
            pr_number: Color::Rgb(130, 80, 223),        // done fg (purple)
            pr_author: Color::Rgb(9, 105, 218),         // accent fg
            pr_draft: Color::Rgb(101, 109, 118),        // muted
            selection_bg: Color::Rgb(218, 232, 252),    // accent subtle
            selection_fg: Color::Rgb(31, 35, 40),       // fg default
            diff_added_fg: Color::Rgb(31, 111, 31),     // success fg
            diff_added_bg: Color::Rgb(230, 255, 237),   // success subtle
            diff_removed_fg: Color::Rgb(207, 34, 46),   // danger fg
            diff_removed_bg: Color::Rgb(255, 235, 233), // danger subtle
            diff_context: Color::Rgb(101, 109, 118),    // fg muted
            diff_hunk: Color::Rgb(9, 105, 218),         // accent fg
            tab_active: Color::Rgb(9, 105, 218),        // accent fg
            tab_inactive: Color::Rgb(101, 109, 118),    // fg muted
            key_fg: Color::Rgb(255, 255, 255),          // white (for dark badge bg)
            key_bg: Color::Rgb(9, 105, 218),            // accent fg
            key_desc: Color::Rgb(9, 105, 218),          // accent fg
            stats_added: Color::Rgb(31, 111, 31),       // success fg
            stats_removed: Color::Rgb(207, 34, 46),     // danger fg
            section_header: Color::Rgb(130, 80, 223),   // done fg (purple)
            separator: Color::Rgb(208, 215, 222),       // border default
            phase_dev: Color::Rgb(154, 103, 0),         // attention fg
            phase_plan_qa: Color::Rgb(9, 105, 218),     // accent fg
            phase_done: Color::Rgb(31, 111, 31),        // success fg
            phase_idle: Color::Rgb(101, 109, 118),      // fg muted
            phase_error: Color::Rgb(207, 34, 46),       // danger fg
        }
    }

    pub fn catppuccin_latte() -> Self {
        Self {
            bg: Color::Rgb(239, 241, 245),              // base
            border: Color::Rgb(172, 176, 190),          // overlay0
            border_dim: Color::Rgb(188, 192, 204),      // surface2
            text: Color::Rgb(76, 79, 105),              // text
            text_dim: Color::Rgb(172, 176, 190),        // overlay0
            text_accent: Color::Rgb(30, 102, 245),      // blue
            pr_number: Color::Rgb(136, 57, 239),        // mauve
            pr_author: Color::Rgb(30, 102, 245),        // blue
            pr_draft: Color::Rgb(172, 176, 190),        // overlay0
            selection_bg: Color::Rgb(188, 192, 204),    // surface2
            selection_fg: Color::Rgb(76, 79, 105),      // text
            diff_added_fg: Color::Rgb(64, 160, 43),     // green
            diff_added_bg: Color::Rgb(220, 240, 215),   // light green tint
            diff_removed_fg: Color::Rgb(210, 15, 57),   // red
            diff_removed_bg: Color::Rgb(250, 215, 220), // light red tint
            diff_context: Color::Rgb(172, 176, 190),    // overlay0
            diff_hunk: Color::Rgb(30, 102, 245),        // blue
            tab_active: Color::Rgb(136, 57, 239),       // mauve
            tab_inactive: Color::Rgb(172, 176, 190),    // overlay0
            key_fg: Color::Rgb(239, 241, 245),          // base (light)
            key_bg: Color::Rgb(136, 57, 239),           // mauve
            key_desc: Color::Rgb(30, 102, 245),         // blue
            stats_added: Color::Rgb(64, 160, 43),       // green
            stats_removed: Color::Rgb(210, 15, 57),     // red
            section_header: Color::Rgb(136, 57, 239),   // mauve
            separator: Color::Rgb(188, 192, 204),       // surface2
            phase_dev: Color::Rgb(223, 142, 29),        // yellow
            phase_plan_qa: Color::Rgb(30, 102, 245),    // blue
            phase_done: Color::Rgb(64, 160, 43),        // green
            phase_idle: Color::Rgb(172, 176, 190),      // overlay0
            phase_error: Color::Rgb(210, 15, 57),       // red
        }
    }

    pub fn rosepine_dawn() -> Self {
        Self {
            bg: Color::Rgb(250, 244, 237),              // base
            border: Color::Rgb(152, 147, 165),          // muted
            border_dim: Color::Rgb(218, 218, 226),      // overlay (lighter)
            text: Color::Rgb(87, 82, 121),              // text
            text_dim: Color::Rgb(152, 147, 165),        // muted
            text_accent: Color::Rgb(86, 148, 159),      // foam
            pr_number: Color::Rgb(144, 122, 169),       // iris
            pr_author: Color::Rgb(86, 148, 159),        // foam
            pr_draft: Color::Rgb(152, 147, 165),        // muted
            selection_bg: Color::Rgb(218, 218, 226),    // overlay
            selection_fg: Color::Rgb(87, 82, 121),      // text
            diff_added_fg: Color::Rgb(40, 105, 131),    // pine
            diff_added_bg: Color::Rgb(210, 232, 238),   // pine tint
            diff_removed_fg: Color::Rgb(180, 99, 122),  // love/red
            diff_removed_bg: Color::Rgb(238, 218, 224), // love tint
            diff_context: Color::Rgb(152, 147, 165),    // muted
            diff_hunk: Color::Rgb(86, 148, 159),        // foam
            tab_active: Color::Rgb(144, 122, 169),      // iris
            tab_inactive: Color::Rgb(152, 147, 165),    // muted
            key_fg: Color::Rgb(250, 244, 237),          // base (light)
            key_bg: Color::Rgb(180, 99, 122),           // love/rose
            key_desc: Color::Rgb(86, 148, 159),         // foam
            stats_added: Color::Rgb(40, 105, 131),      // pine
            stats_removed: Color::Rgb(180, 99, 122),    // love/red
            section_header: Color::Rgb(144, 122, 169),  // iris
            separator: Color::Rgb(218, 218, 226),       // overlay
            phase_dev: Color::Rgb(234, 157, 52),        // gold
            phase_plan_qa: Color::Rgb(86, 148, 159),    // foam
            phase_done: Color::Rgb(40, 105, 131),       // pine
            phase_idle: Color::Rgb(152, 147, 165),      // muted
            phase_error: Color::Rgb(180, 99, 122),      // love/red
        }
    }

    pub fn gruvbox_light() -> Self {
        Self {
            bg: Color::Rgb(249, 245, 215),              // bg0 light
            border: Color::Rgb(102, 92, 84),            // gruvbox4
            border_dim: Color::Rgb(189, 174, 147),      // bg3 (light)
            text: Color::Rgb(60, 56, 54),               // fg1
            text_dim: Color::Rgb(102, 92, 84),          // gruvbox4
            text_accent: Color::Rgb(66, 123, 88),       // aqua
            pr_number: Color::Rgb(177, 98, 134),        // purple
            pr_author: Color::Rgb(7, 102, 120),         // aqua dark
            pr_draft: Color::Rgb(102, 92, 84),          // gruvbox4
            selection_bg: Color::Rgb(213, 196, 161),    // bg2
            selection_fg: Color::Rgb(60, 56, 54),       // fg1
            diff_added_fg: Color::Rgb(121, 116, 14),    // green
            diff_added_bg: Color::Rgb(229, 233, 190),   // green tint
            diff_removed_fg: Color::Rgb(157, 0, 6),     // red
            diff_removed_bg: Color::Rgb(252, 220, 208), // red tint
            diff_context: Color::Rgb(102, 92, 84),      // gruvbox4
            diff_hunk: Color::Rgb(7, 102, 120),         // aqua dark
            tab_active: Color::Rgb(181, 118, 20),       // yellow
            tab_inactive: Color::Rgb(102, 92, 84),      // gruvbox4
            key_fg: Color::Rgb(249, 245, 215),          // bg0 (light)
            key_bg: Color::Rgb(175, 58, 3),             // orange
            key_desc: Color::Rgb(7, 102, 120),          // aqua dark
            stats_added: Color::Rgb(121, 116, 14),      // green
            stats_removed: Color::Rgb(157, 0, 6),       // red
            section_header: Color::Rgb(181, 118, 20),   // yellow
            separator: Color::Rgb(189, 174, 147),       // bg3 (light)
            phase_dev: Color::Rgb(181, 118, 20),        // yellow
            phase_plan_qa: Color::Rgb(7, 102, 120),     // aqua dark
            phase_done: Color::Rgb(121, 116, 14),       // green
            phase_idle: Color::Rgb(102, 92, 84),        // gruvbox4
            phase_error: Color::Rgb(157, 0, 6),         // red
        }
    }

    pub fn solarized_light() -> Self {
        Self {
            bg: Color::Rgb(253, 246, 227),              // base3
            border: Color::Rgb(147, 161, 161),          // base1 (highlights)
            border_dim: Color::Rgb(238, 232, 213),      // base2 (background highlights)
            text: Color::Rgb(101, 123, 131),            // base00 (body text)
            text_dim: Color::Rgb(147, 161, 161),        // base1 (comments)
            text_accent: Color::Rgb(38, 139, 210),      // blue
            pr_number: Color::Rgb(108, 113, 196),       // violet
            pr_author: Color::Rgb(38, 139, 210),        // blue
            pr_draft: Color::Rgb(147, 161, 161),        // base1
            selection_bg: Color::Rgb(238, 232, 213),    // base2
            selection_fg: Color::Rgb(101, 123, 131),    // base00
            diff_added_fg: Color::Rgb(133, 153, 0),     // green
            diff_added_bg: Color::Rgb(223, 232, 190),   // green tint
            diff_removed_fg: Color::Rgb(220, 50, 47),   // red
            diff_removed_bg: Color::Rgb(252, 220, 218), // red tint
            diff_context: Color::Rgb(147, 161, 161),    // base1
            diff_hunk: Color::Rgb(38, 139, 210),        // blue
            tab_active: Color::Rgb(38, 139, 210),       // blue
            tab_inactive: Color::Rgb(147, 161, 161),    // base1
            key_fg: Color::Rgb(253, 246, 227),          // base3 (light bg)
            key_bg: Color::Rgb(211, 54, 130),           // magenta
            key_desc: Color::Rgb(38, 139, 210),         // blue
            stats_added: Color::Rgb(133, 153, 0),       // green
            stats_removed: Color::Rgb(220, 50, 47),     // red
            section_header: Color::Rgb(108, 113, 196),  // violet
            separator: Color::Rgb(238, 232, 213),       // base2
            phase_dev: Color::Rgb(181, 137, 0),         // yellow
            phase_plan_qa: Color::Rgb(38, 139, 210),    // blue
            phase_done: Color::Rgb(133, 153, 0),        // green
            phase_idle: Color::Rgb(147, 161, 161),      // base1
            phase_error: Color::Rgb(220, 50, 47),       // red
        }
    }
}
