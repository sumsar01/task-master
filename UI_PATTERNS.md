# UI Patterns — Layout & Theming Reference

> Extracted from `code-reviewer`. Use this as a blueprint when building a new ratatui TUI app.

---

## Theme System

### The `Theme` struct

Define a single `Theme` struct with **named semantic slots** for every UI element. Render functions never hardcode colors — they always ask the theme. Pass `&Theme` down to every render function as a parameter.

```rust
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    // ── Chrome ────────────────────────────────────────────────────────────────
    pub border: Color,       // Primary border color (blocks, panels)
    pub border_dim: Color,   // Dimmer border (inner / secondary panels)

    // ── Text ─────────────────────────────────────────────────────────────────
    pub text: Color,         // Normal body text
    pub text_dim: Color,     // Secondary / muted text (timestamps, labels)
    pub text_accent: Color,  // Accent text (paths, branch names)

    // ── PR list ───────────────────────────────────────────────────────────────
    pub pr_number: Color,    // #123 PR number
    pub pr_author: Color,    // Author name
    pub pr_draft: Color,     // DRAFT badge text
    pub selection_bg: Color,
    pub selection_fg: Color,

    // ── Diff ──────────────────────────────────────────────────────────────────
    pub diff_added_fg: Color,
    pub diff_added_bg: Color,   // Full-row background tint
    pub diff_removed_fg: Color,
    pub diff_removed_bg: Color,
    pub diff_context: Color,    // Unchanged lines
    pub diff_hunk: Color,       // @@ hunk header lines

    // ── Tabs ─────────────────────────────────────────────────────────────────
    pub tab_active: Color,
    pub tab_inactive: Color,

    // ── Key hint badges ───────────────────────────────────────────────────────
    pub key_fg: Color,    // Dark fg so the badge bg is readable
    pub key_bg: Color,    // Badge background (usually a warm accent)
    pub key_desc: Color,  // Description text next to badge

    // ── Stats ─────────────────────────────────────────────────────────────────
    pub stats_added: Color,
    pub stats_removed: Color,

    // ── Misc ─────────────────────────────────────────────────────────────────
    pub section_header: Color,  // Section headings in overlays
    pub separator: Color,       // ── separator lines
}
```

### Convenience style builders

Add methods on `Theme` so render code stays clean:

```rust
impl Theme {
    pub fn border_style(&self) -> Style { Style::default().fg(self.border) }
    pub fn border_dim_style(&self) -> Style { Style::default().fg(self.border_dim) }
    pub fn text_style(&self) -> Style { Style::default().fg(self.text) }
    pub fn text_dim_style(&self) -> Style { Style::default().fg(self.text_dim) }
    pub fn text_accent_style(&self) -> Style { Style::default().fg(self.text_accent) }

    pub fn selection_style(&self) -> Style {
        Style::default().bg(self.selection_bg).fg(self.selection_fg)
    }

    pub fn diff_added_style(&self) -> Style {
        Style::default().fg(self.diff_added_fg).bg(self.diff_added_bg)
    }
    pub fn diff_removed_style(&self) -> Style {
        Style::default().fg(self.diff_removed_fg).bg(self.diff_removed_bg)
    }
    pub fn diff_context_style(&self) -> Style { Style::default().fg(self.diff_context) }
    pub fn diff_hunk_style(&self) -> Style {
        Style::default().fg(self.diff_hunk).add_modifier(Modifier::BOLD)
    }

    pub fn tab_active_style(&self) -> Style {
        Style::default()
            .fg(self.tab_active)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED)
    }
    pub fn tab_inactive_style(&self) -> Style { Style::default().fg(self.tab_inactive) }

    pub fn key_badge_style(&self) -> Style {
        Style::default().fg(self.key_fg).bg(self.key_bg).add_modifier(Modifier::BOLD)
    }
    pub fn key_desc_style(&self) -> Style { Style::default().fg(self.key_desc) }

    pub fn section_header_style(&self) -> Style {
        Style::default().fg(self.section_header).add_modifier(Modifier::BOLD)
    }
    pub fn separator_style(&self) -> Style { Style::default().fg(self.separator) }
}
```

### Theme registry

11 bundled themes — 6 dark, 5 light:

```rust
/// (id, display name) — order is the order shown in the picker
pub const ALL_THEMES: &[(&str, &str)] = &[
    ("tokyonight",      "Tokyo Night"),        // dark
    ("gruvbox",         "Gruvbox Dark"),        // dark
    ("catppuccin",      "Catppuccin Mocha"),    // dark
    ("nord",            "Nord"),                // dark
    ("dracula",         "Dracula"),             // dark
    ("rosepine",        "Rosé Pine"),           // dark
    ("github_light",    "GitHub Light"),        // light
    ("catppuccin_latte","Catppuccin Latte"),    // light
    ("rosepine_dawn",   "Rosé Pine Dawn"),      // light
    ("gruvbox_light",   "Gruvbox Light"),       // light
    ("solarized_light", "Solarized Light"),     // light
];

impl Theme {
    /// Resolve by id, falling back to the default on unknown names.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "tokyonight"       => Self::tokyonight(),
            "gruvbox"          => Self::gruvbox(),
            "catppuccin"       => Self::catppuccin(),
            "nord"             => Self::nord(),
            "dracula"          => Self::dracula(),
            "rosepine"         => Self::rosepine(),
            "github_light"     => Self::github_light(),
            "catppuccin_latte" => Self::catppuccin_latte(),
            "rosepine_dawn"    => Self::rosepine_dawn(),
            "gruvbox_light"    => Self::gruvbox_light(),
            "solarized_light"  => Self::solarized_light(),
            _                  => Self::tokyonight(),  // default fallback
        }
    }

    /// Index in ALL_THEMES (for the picker cursor).
    pub fn index_of(name: &str) -> usize {
        let lower = name.to_lowercase();
        ALL_THEMES.iter().position(|(id, _)| *id == lower.as_str()).unwrap_or(0)
    }
}
```

### Theme constructor pattern

Use `Color::Rgb(r, g, b)` throughout. Add inline comments with the official palette name so the file is self-documenting. Each theme also provides a `make_syntax()` method that maps the 25 tree-sitter highlight capture slots to colors (see Syntax Highlighting section).

```rust
pub fn tokyonight() -> Self {
    Self {
        border:       Color::Rgb(86, 95, 137),   // storm border
        border_dim:   Color::Rgb(54, 58, 79),    // dim border
        text:         Color::Rgb(192, 202, 245),  // fg
        text_dim:     Color::Rgb(86, 95, 137),   // comment
        text_accent:  Color::Rgb(122, 162, 247),  // blue
        // ... etc
    }
}
```

### Config persistence

Store the theme name in the app config (TOML):

```toml
[ui]
theme = "tokyonight"
```

```rust
#[derive(Deserialize, Serialize)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String { "tokyonight".to_string() }
```

---

## Layout Patterns

### Screen routing

Single top-level `render` function dispatches to screens, then draws overlays on top:

```rust
pub fn render(f: &mut Frame, app: &mut App) {
    let t = app.theme.clone();

    match &app.screen {
        Screen::PrList   => pr_list::render(f, app, &t),
        Screen::PrDetail => pr_detail::render(f, app, &t),
    }

    // Overlays drawn last (on top of everything)
    if app.show_help         { help::render(f, &t); }
    if app.show_theme_picker { theme_picker::render(f, app, &t); }
}
```

### Standard 3-row screen (list / main view)

```
┌──────────────────────┐
│ header  (2 rows)     │  app name, repo, active filter badge
├──────────────────────┤
│ content (min/flex)   │  scrollable list or main widget
├──────────────────────┤
│ status bar (1 row)   │  key hint badges
└──────────────────────┘
```

```rust
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(2),  // header
        Constraint::Min(0),     // content
        Constraint::Length(1),  // status bar
    ])
    .split(area);
```

### Standard 4-row screen (detail / tabbed view)

```
┌──────────────────────┐
│ item header (6 rows) │  bordered block: title, meta, stats, review badge
├──────────────────────┤
│ tabs       (2 rows)  │  Diff │ Comments │ Difftastic
├──────────────────────┤
│ content    (flex)    │  swapped by active tab; may include file tree sidebar
├──────────────────────┤
│ status bar (1 row)   │  key hint badges
└──────────────────────┘
```

```rust
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(6),  // item header (2 borders + up to 4 content lines)
        Constraint::Length(2),  // tabs
        Constraint::Min(0),     // content
        Constraint::Length(1),  // status bar
    ])
    .split(area);
```

### File tree + content horizontal split

Used inside the content area on Diff and Difftastic tabs when `show_file_tree` is true:

```rust
const TREE_WIDTH: u16 = 36;

let [tree_area, content_area] = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Length(TREE_WIDTH), Constraint::Min(0)])
    .areas(area);
```

### Responsive two-panel diff layout

Switch between unified and side-by-side based on terminal width:

```rust
const SPLIT_THRESHOLD: u16 = 160;

if area.width >= SPLIT_THRESHOLD {
    render_side_by_side(f, data, area, t);
} else {
    render_unified(f, data, area, t);
}

// Side-by-side: 50/50 horizontal split
let chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
    .split(area);
```

---

## Overlay Patterns

### Percentage-based centered popup (e.g. help)

```rust
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}

// Usage (help overlay is 60% × 70%):
let area = centered_rect(60, 70, f.area());
f.render_widget(Clear, area);   // ← always Clear before drawing overlay
```

### Fixed-size centered popup (e.g. theme picker)

```rust
fn picker_rect(r: Rect) -> Rect {
    let width  = 36u16;
    let height = ALL_THEMES.len() as u16 + 4;  // entries + border + padding + status bar

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
```

---

## Widget Patterns

### Status bar with key hint badges

Consistent across all screens. Key label in a colored badge, description text alongside, separated by `·`:

```rust
fn render_statusbar(f: &mut Frame, area: Rect, t: &Theme) {
    let hints: &[(&str, &str)] = &[
        ("j/k",   "navigate"),
        ("Enter", "open"),
        ("q",     "quit"),
    ];

    let mut spans = vec![Span::raw(" ")];
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", t.text_dim_style()));
        }
        spans.push(Span::styled(format!(" {key} "), t.key_badge_style()));
        spans.push(Span::styled(format!(" {desc}"), t.key_desc_style()));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

### Tab bar

```rust
let titles: Vec<Line> = tab_names.iter().enumerate().map(|(i, name)| {
    if i == selected {
        Line::from(Span::styled(*name, t.tab_active_style()))
    } else {
        Line::from(Span::styled(*name, t.tab_inactive_style()))
    }
}).collect();

let tabs = Tabs::new(titles)
    .select(selected)
    .block(Block::default().borders(Borders::BOTTOM).border_style(t.border_dim_style()))
    .highlight_style(t.tab_active_style())
    .divider(Span::styled(" │ ", t.border_dim_style()));
```

### Bordered block with accent title

```rust
Block::default()
    .borders(Borders::ALL)
    .border_style(t.border_style())
    .title(Span::styled(
        " Title ",
        Style::default().fg(t.text_accent).add_modifier(Modifier::BOLD),
    ))
```

### List rows with per-column styling

Build each row as a `Line` with multiple `Span`s, each with its own style. For selected rows, apply `selection_style()` as the base and override fg per span:

```rust
let base = if selected { t.selection_style() } else { Style::default() };

Line::from(vec![
    Span::styled(id_col,    base.fg(t.pr_number).add_modifier(Modifier::BOLD)),
    Span::styled(" ",       base),
    Span::styled(title_col, base.fg(t.text)),
    Span::styled(meta_col,  base.fg(t.text_dim)),
])
```

### Title truncation

```rust
fn truncate(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars.saturating_sub(1)].iter().collect::<String>() + "…"
    }
}
```

### Diff line rendering

```rust
let (prefix, style) = match line.kind {
    Added   => ("+", t.diff_added_style()),    // fg + bg tint
    Removed => ("-", t.diff_removed_style()),  // fg + bg tint
    Context => (" ", t.diff_context_style()),  // fg only
};
lines.push(Line::from(Span::styled(format!("{prefix}{}", line.content), style)));
```

### Section separator in scrollable content

```rust
lines.push(Line::from(Span::styled(
    " ".to_string() + &"─".repeat(48),
    t.separator_style(),
)));
```

### Loading / error state guard

Apply consistently at the top of every render function before touching data:

```rust
pub fn render(f: &mut Frame, app: &App, area: Rect, t: &Theme) {
    match &app.load_state {
        LoadState::Loading => {
            let p = Paragraph::new("  Loading…")
                .style(t.text_dim_style())
                .block(Block::default().borders(Borders::ALL).border_style(t.border_style()));
            f.render_widget(p, area);
            return;
        }
        LoadState::Error(e) => {
            let p = Paragraph::new(format!("  Error: {e}"))
                .style(t.diff_removed_style())
                .block(Block::default().borders(Borders::ALL).border_style(t.border_style()));
            f.render_widget(p, area);
            return;
        }
        LoadState::Idle => {}
    }
    // ... normal render
}
```

---

## PR List Row Layout

Each row is a single `Line` of ordered `Span`s. All span widths are fixed except the title, which fills remaining space:

| # | Content | Format | Style |
|---|---|---|---|
| 1 | PR number | `" #{number:<5}"` (7 chars total) | `pr_number`, bold |
| 2 | Spacer | `" "` | base |
| 3 | Title | `{title:<title_width}` (truncated with `…`) | `text` |
| 4 | Draft badge | `" ▸DRAFT"` (conditional) | `pr_draft`, bold |
| 5 | Spacer | `"  "` | base |
| 6 | Author | `{author:<20}` | `pr_author` |
| 7 | Spacer | `"  "` | base |
| 8 | Stats | `"{+add:>6} {-del:>6} {±total:>7}"` | `text_dim` |
| 9 | Review badge | `"  {badge}"` (conditional, see below) | decision color, bold |

**`title_width` calculation:**
```rust
let fixed = 7 + 1 + 2 + 20 + 2 + stats.len()
    + if pr.draft { 8 } else { 0 }
    + badge_text.len();            // "" when no decision yet
let title_width = inner_width.saturating_sub(fixed);
```

**Review badge values:**

| `review_decision` | List badge | Detail badge | Color |
|---|---|---|---|
| `"APPROVED"` | `✓ APPROVED` | `✓ APPROVED` | `Color::Green` |
| `"CHANGES_REQUESTED"` | `✗ CHANGES` | `✗ CHANGES REQUESTED` | `Color::Red` |
| `"REVIEW_REQUIRED"` | `? REVIEW` | `? REVIEW REQUIRED` | `Color::Yellow` |
| `None` | *(absent)* | *(absent)* | — |

In the detail header the badge appears as a 4th content line: `"Review: "` (dim) + badge (colored, bold).

---

## File Tree Sidebar

A collapsible sidebar (36 cols wide) showing changed files as a hierarchical directory tree. Available on the **Diff** and **Difftastic** tabs; hidden on Comments.

**Toggle:** `Space`. Hidden by default.

**Focus model:** Two focus states — `DetailFocus::Content` and `DetailFocus::FileTree`. `Tab` cycles focus when the tree is visible (on Diff/Difftastic). When tree has focus, the border uses `border_style()` (bright); when unfocused, `border_dim_style()`.

**Building the tree:**
```rust
// build_rows(file_paths) → DirNode prefix tree → Vec<TreeRow>
// Files before sub-directories at each level.
// Directories labeled "dirname/", files labeled basename.
// Indent: "  " × depth.
```

**Row states:**

| State | Style |
|---|---|
| Cursor + tree focused | `selection_bg`/`selection_fg`, bold |
| Active file (currently viewed) | `"▶ "` prefix, `text_accent`, bold |
| Directory | `text_dim` |
| Other file | `text` |

**Key bindings (tree focused):**

| Key | Action |
|---|---|
| `j` / Down | Move cursor down |
| `k` / Up | Move cursor up |
| `Enter` | Jump to file; return focus to content |
| `Space` / `Esc` / `Tab` | Return focus to content (tree stays visible) |

**Scroll:** Uses `ListState::select(Some(tree_cursor))` so the selected row is always in view.

---

## Theme Picker UX

- Opening the picker saves the current theme as `theme_picker_original`
- Navigating (j/k) applies the theme **live** immediately
- `Enter` commits: writes the id to config
- `Esc` reverts to `theme_picker_original`

```rust
KeyCode::Char('j') => {
    self.theme_picker_cursor += 1;
    self.theme = Theme::from_name(ALL_THEMES[self.theme_picker_cursor].0);
}
KeyCode::Enter => {
    self.config.ui.theme = ALL_THEMES[self.theme_picker_cursor].0.to_string();
    self.show_theme_picker = false;
}
KeyCode::Esc => {
    if let Some(original) = self.theme_picker_original.take() {
        self.theme = original;  // revert
    }
    self.show_theme_picker = false;
}
```

**Picker row rendering:** Active cursor row gets `"▸ "` prefix + `tab_active` color + bold. Currently saved theme gets a `" ✓"` suffix. All others get `"  "` prefix + `text_dim`.

---

## Help Overlay

**Geometry:** `centered_rect(60, 70, f.area())` — 60% wide × 70% tall.

**Dismiss:** Any keypress (checked before routing to screen handlers).

**Content structure:**

```
  PR List                        ← section_header color, bold
  j / k    Navigate up/down      ← key_badge_style + key_desc_style
  Enter    Open PR detail
  a        Toggle mine / all PRs
  r        Refresh PR list
  o        Open PR in browser
  q        Quit

  PR Detail
  Tab      Switch pane / focus
  Space    Toggle file tree
  j / k    Scroll
  n / N    Next / previous file
  c        Checkout PR branch
  o        Open PR in browser
  Esc / q  Back to list

  Both
  T        Open theme picker
  ?        Toggle this help
```

---

## Syntax Highlighting (`syntax.rs`)

Tree-sitter–based syntax highlighting used in the Difftastic tab to colorize tokens within diff output.

### Supported languages

| Language | Extensions |
|---|---|
| Rust | `.rs` |
| Python | `.py`, `.pyi` |
| JavaScript | `.js`, `.mjs`, `.cjs`, `.jsx` |
| TypeScript | `.ts` |
| TSX | `.tsx` |
| Go | `.go` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` |
| JSON | `.json` |
| Bash/Shell | `.sh`, `.bash`, `.zsh`, `.fish` |
| TOML | `.toml` |

Unknown extensions return `None`; caller falls back to plain ANSI diff colors.

### Capture slots (25 total)

```
attribute, comment, constant, constant.builtin, constructor,
delimiter, embedded, escape, function, function.builtin,
keyword, label, module, number, operator, property, punctuation,
string, string.special, tag, type, type.builtin,
variable, variable.builtin, variable.parameter
```

### How `highlight()` works

1. Match filename extension → grammar config
2. Run tree-sitter on the full source document
3. Collect `HighlightEvent::{Source, HighlightStart, HighlightEnd}` into flat `(byte_start, byte_end, hl_index?)` spans
4. Clip spans to line boundaries → `Vec<Vec<HlSpan>>` (one `Vec` per line)

### Theme integration

Each theme provides `make_syntax() -> [Color; 25]` mapping every capture slot to a `Color`. Logical grouping:

| Slots | Mapped to |
|---|---|
| `function`, `function.builtin`, `attribute` | function color |
| `keyword`, `tag` | keyword color |
| `string`, `string.special` | string color |
| `type`, `type.builtin`, `module` | type color |
| `constant`, `constant.builtin`, `variable.builtin` | constant color |
| `variable`, `variable.parameter`, `label`, `property` | variable color |
| `number` | number color |
| `operator` | operator color |
| `escape` | escape color |
| `comment` | comment color |
| `constructor` | type color |
| `delimiter`, `embedded`, `punctuation` | text color (fallback) |

### Difftastic + syntax overlay

The `parse_ansi_to_lines_with_syntax` function combines both:

1. Detects the side-by-side split column by scanning for paired line-number labels
2. Reconstructs old-file and new-file source strings from display output
3. Runs tree-sitter on both reconstructed sources
4. For diff-colored tokens (green = added, red = removed): uses diff color as **background**, tree-sitter token as **foreground**
5. For context tokens: applies tree-sitter fg directly
6. Line-number labels retain their original difftastic styling
7. Falls back to plain ANSI if split column undetectable or no grammar available

---

## File Structure

```
src/
├── app.rs              # App state, event loop, key handling, background tasks
├── config.rs           # Config load/save (theme name, show_all_prs, last_repo)
├── git.rs              # Git repo detection, GitHub remote URL parsing
├── github.rs           # GitHubClient, PullRequest/ReviewComment models, diff parser,
│                       #   GraphQL review decision fetch
├── syntax.rs           # SyntaxHighlighter — tree-sitter grammars and highlight()
└── ui/
    ├── mod.rs          # Top-level render dispatcher + overlay routing
    ├── theme.rs        # Theme struct, ALL_THEMES (11 themes), constructors, make_syntax()
    ├── theme_picker.rs # Theme picker overlay
    ├── help.rs         # Help overlay
    ├── pr_list.rs      # PR list screen
    ├── pr_detail.rs    # PR detail screen (header + tab dispatch)
    ├── diff.rs         # Diff tab — unified + side-by-side rendering
    ├── comments.rs     # Comments tab
    ├── difftastic.rs   # Difftastic tab — ANSI parser + syntax overlay
    └── file_tree.rs    # File tree sidebar — DirNode builder + List renderer
```

---

## Keybindings Reference

### Theme picker (intercepts all input when open)

| Key | Action |
|---|---|
| `j` / Down | Move cursor down (live-previews theme) |
| `k` / Up | Move cursor up (live-previews theme) |
| `Enter` | Apply and close |
| `Esc` | Cancel and restore original theme |

### Help overlay

| Key | Action |
|---|---|
| Any | Dismiss |

### PR List screen

| Key | Action |
|---|---|
| `j` / Down | Move cursor down |
| `k` / Up | Move cursor up |
| `Enter` | Open PR detail |
| `a` | Toggle mine / all PRs and reload |
| `r` | Refresh list |
| `o` | Open PR in browser |
| `T` | Open theme picker |
| `?` | Show help |
| `q` / `Q` | Quit |

### PR Detail screen — content focused

| Key | Action |
|---|---|
| `Tab` | If tree visible: cycle focus Content ↔ FileTree. Else: cycle tabs Diff → Comments → Difftastic |
| `Space` | Toggle file tree sidebar (Diff + Difftastic tabs only) |
| `j` / Down | Scroll content down |
| `k` / Up | Scroll content up |
| `n` | Next file (resets scroll) |
| `N` | Previous file (resets scroll) |
| `c` | Checkout PR branch |
| `o` | Open PR in browser |
| `T` | Open theme picker |
| `?` | Show help |
| `q` / `Esc` | Back to PR list |

### PR Detail screen — file tree focused

| Key | Action |
|---|---|
| `j` / Down | Move tree cursor down |
| `k` / Up | Move tree cursor up |
| `Enter` | Jump to selected file; return focus to content |
| `Space` / `Esc` / `Tab` | Return focus to content (tree stays visible) |

---

## Checklist for a New App

- [ ] Define `Theme` struct with named semantic slots
- [ ] Add convenience `*_style()` methods on `Theme`
- [ ] Define `ALL_THEMES` registry + `from_name` + `index_of`
- [ ] Persist theme name in config (default to a sensible theme)
- [ ] Top-level `render` draws screen then overlays
- [ ] Every screen: header + content + status bar layout
- [ ] Status bar uses `key_badge_style()` + `·` separators
- [ ] Overlays: `Clear` widget first, then draw
- [ ] Theme picker: live preview on navigate, revert on Esc, `✓` suffix on saved theme
- [ ] Loading/error guards at top of every data-dependent render fn
- [ ] List rows: `selection_style()` as base, override fg per span
- [ ] Collapsible sidebar: `Length(TREE_WIDTH)` + `Min(0)` horizontal split, focus toggle via `Tab`/`Space`
- [ ] Responsive diff: unified below threshold, side-by-side above
