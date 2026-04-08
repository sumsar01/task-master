/// Lightweight template engine for agent prompt files.
///
/// Agent prompts can live either inline (as Rust string constants) or as
/// `.opencode/agents/<name>.md` files on disk. The file format mirrors the
/// supervisor's `supervisor.md`: optional YAML frontmatter (`--- ... ---`)
/// followed by the prompt body. Only the body is used as the prompt text —
/// frontmatter is stripped before rendering.
///
/// Variables are injected via `{{token}}` placeholders replaced at render time.
/// No external crate is required; the substitution is a sequential `str::replace`.
use std::path::Path;

/// Strip YAML frontmatter from the start of a markdown string.
///
/// If the string begins with `---\n`, everything up to and including the
/// closing `---\n` (or `---` at end-of-file) is removed. The returned slice
/// starts at the first character after the closing delimiter.
///
/// If no frontmatter is present the original string is returned unchanged.
pub fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content;
    }

    // Find the closing `---` delimiter. It must appear on its own line after
    // the opening `---`.
    let after_open = content.trim_start_matches("---").trim_start_matches('\n');
    if let Some(close_pos) = after_open.find("\n---") {
        // Skip past `\n---` and an optional trailing newline.
        let rest = &after_open[close_pos + 4..];
        rest.trim_start_matches('\n')
    } else {
        // Malformed or no closing delimiter — return content as-is.
        content
    }
}

/// Render a template by replacing `{{key}}` placeholders with their values.
///
/// Substitutions are applied in the order provided; earlier entries take
/// precedence if keys overlap. Unknown placeholders are left unchanged so
/// callers can detect them easily.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_owned();
    for (key, value) in vars {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

/// Try to load a template from `<base_dir>/.opencode/agents/<name>.md`.
///
/// Returns `Some(content)` if the file exists and can be read, `None` if the
/// file is absent. Propagates `None` (not an error) on read failures so callers
/// can fall back to the built-in string without aborting.
pub fn load(base_dir: &Path, agent_name: &str) -> Option<String> {
    let path = base_dir
        .join(".opencode")
        .join("agents")
        .join(format!("{}.md", agent_name));
    std::fs::read_to_string(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // --- strip_frontmatter ---

    #[test]
    fn test_strip_frontmatter_removes_yaml_block() {
        let input = "---\nfoo: bar\n---\nHello world";
        assert_eq!(strip_frontmatter(input), "Hello world");
    }

    #[test]
    fn test_strip_frontmatter_multiline() {
        let input = "---\na: 1\nb: 2\nmodel: haiku\n---\nPrompt body here";
        assert_eq!(strip_frontmatter(input), "Prompt body here");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let input = "Just a plain prompt\nwith multiple lines.";
        assert_eq!(strip_frontmatter(input), input);
    }

    #[test]
    fn test_strip_frontmatter_leading_newline_stripped() {
        let input = "---\nfoo: bar\n---\n\nBody starts here";
        assert_eq!(strip_frontmatter(input), "Body starts here");
    }

    #[test]
    fn test_strip_frontmatter_missing_close_delimiter() {
        // Malformed: no closing ---; return content unchanged.
        let input = "---\nfoo: bar\nBody without close";
        assert_eq!(strip_frontmatter(input), input);
    }

    // --- render ---

    #[test]
    fn test_render_basic_substitution() {
        let tmpl = "Hello {{name}}, your PR is #{{pr}}.";
        let result = render(tmpl, &[("name", "Alice"), ("pr", "42")]);
        assert_eq!(result, "Hello Alice, your PR is #42.");
    }

    #[test]
    fn test_render_unknown_placeholder_preserved() {
        let tmpl = "Known: {{known}}, Unknown: {{unknown}}";
        let result = render(tmpl, &[("known", "yes")]);
        assert_eq!(result, "Known: yes, Unknown: {{unknown}}");
    }

    #[test]
    fn test_render_multiple_occurrences() {
        let tmpl = "{{x}} and {{x}} again";
        let result = render(tmpl, &[("x", "hi")]);
        assert_eq!(result, "hi and hi again");
    }

    #[test]
    fn test_render_empty_vars() {
        let tmpl = "No vars here.";
        let result = render(tmpl, &[]);
        assert_eq!(result, "No vars here.");
    }

    #[test]
    fn test_render_value_with_curly_braces() {
        // Values containing braces should not trigger further substitution.
        let tmpl = "Result: {{val}}";
        let result = render(tmpl, &[("val", "{literal}")]);
        assert_eq!(result, "Result: {literal}");
    }

    // --- load ---

    #[test]
    fn test_load_returns_none_for_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(load(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn test_load_returns_content_for_existing_file() {
        let dir = TempDir::new().unwrap();
        let agents_dir = dir.path().join(".opencode").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(agents_dir.join("myagent.md"), "Hello from template").unwrap();

        let content = load(dir.path(), "myagent").unwrap();
        assert_eq!(content, "Hello from template");
    }

    #[test]
    fn test_load_and_strip_and_render() {
        let dir = TempDir::new().unwrap();
        let agents_dir = dir.path().join(".opencode").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(
            agents_dir.join("test.md"),
            "---\nmodel: haiku\n---\nYou are agent for PR #{{pr}}.",
        )
        .unwrap();

        let raw = load(dir.path(), "test").unwrap();
        let body = strip_frontmatter(&raw);
        let rendered = render(body, &[("pr", "99")]);
        assert_eq!(rendered, "You are agent for PR #99.");
    }
}
