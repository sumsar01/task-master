use crate::registry::Registry;
use anyhow::Result;
use std::process::Command;

/// Parsed token/cost data from `opencode stats` output.
#[derive(Debug, Default, Clone)]
pub struct StatsRow {
    pub sessions: u64,
    pub input: u64,  // raw tokens
    pub output: u64, // raw tokens
    pub cache_read: u64,
    pub cost_cents: u64,  // stored as integer cents to avoid float arithmetic
    pub cost_str: String, // original formatted string e.g. "$1.23"
}

/// Run `opencode stats --project <path> [--days N]` and parse the output.
/// Returns None if opencode is not on PATH or produces no output.
pub fn fetch_stats(project_path: &str, days: Option<u32>) -> Option<StatsRow> {
    let mut cmd = Command::new("opencode");
    cmd.arg("stats").arg("--project").arg(project_path);
    if let Some(d) = days {
        cmd.arg("--days").arg(d.to_string());
    }
    let output = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_stats_output(&text)
}

/// Parse the box-drawing table output of `opencode stats`.
///
/// We look for lines of the form `│ Key   value │` and extract the value.
pub fn parse_stats_output(text: &str) -> Option<StatsRow> {
    let mut row = StatsRow::default();
    let mut found_any = false;

    for line in text.lines() {
        // Strip box-drawing borders and trim whitespace.
        // Lines look like: │Sessions                                            421 │
        let inner = line.trim_start_matches('│').trim_end_matches('│').trim();

        if let Some(val) = extract_field(inner, "Sessions") {
            row.sessions = parse_token_count(&val);
            found_any = true;
        } else if let Some(val) = extract_field(inner, "Input") {
            row.input = parse_token_count(&val);
        } else if let Some(val) = extract_field(inner, "Output") {
            row.output = parse_token_count(&val);
        } else if let Some(val) = extract_field(inner, "Cache Read") {
            row.cache_read = parse_token_count(&val);
        } else if let Some(val) = extract_field(inner, "Total Cost") {
            row.cost_str = val.trim().to_string();
            row.cost_cents = parse_cost_cents(&row.cost_str);
        }
    }

    if found_any {
        Some(row)
    } else {
        None
    }
}

/// Extract the value for a given field label from a stripped table line.
/// Returns None if the line doesn't start with the label.
fn extract_field(line: &str, label: &str) -> Option<String> {
    if line.starts_with(label) {
        let rest = line[label.len()..].trim().to_string();
        if rest.is_empty() {
            None
        } else {
            Some(rest)
        }
    } else {
        None
    }
}

/// Parse a token count string that may use K/M suffixes.
/// e.g. "85.9M" → 85_900_000,  "4.4M" → 4_400_000,  "421" → 421
pub fn parse_token_count(s: &str) -> u64 {
    let s = s.trim().replace(',', "");
    if let Some(num) = s.strip_suffix('M') {
        (num.parse::<f64>().unwrap_or(0.0) * 1_000_000.0) as u64
    } else if let Some(num) = s.strip_suffix('K') {
        (num.parse::<f64>().unwrap_or(0.0) * 1_000.0) as u64
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
}

/// Parse a cost string like "$1.23" or "$0.00" into integer cents.
fn parse_cost_cents(s: &str) -> u64 {
    let s = s.trim().trim_start_matches('$');
    (s.parse::<f64>().unwrap_or(0.0) * 100.0).round() as u64
}

/// Format a raw token count back to a human-readable string.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format integer cents as a dollar string.
fn format_cost(cents: u64) -> String {
    format!("${:.2}", cents as f64 / 100.0)
}

/// A project-level summary with per-worktree breakdown.
struct ProjectStats {
    name: String,
    worktrees: Vec<(String, StatsRow)>, // (window_name, stats)
}

pub fn cmd_stats(registry: &Registry, days: Option<u32>) -> Result<()> {
    let days_label = match days {
        Some(d) => format!("last {} days", d),
        None => "all time".to_string(),
    };
    println!("Token usage — {}\n", days_label);

    // Group worktrees by project name.
    let mut projects: Vec<ProjectStats> = Vec::new();
    for project in &registry.projects {
        let mut ps = ProjectStats {
            name: project.name.clone(),
            worktrees: Vec::new(),
        };
        for wt_config in &project.worktrees {
            let window_name = format!("{}-{}", project.short, wt_config.name);
            if let Some(wt) = registry.find_worktree(&window_name) {
                let path_str = wt.abs_path.to_string_lossy().to_string();
                let stats = fetch_stats(&path_str, days).unwrap_or_default();
                ps.worktrees.push((window_name, stats));
            }
        }
        projects.push(ps);
    }

    // Column widths
    let name_w = 36usize;
    let tok_w = 18usize;
    let cost_w = 10usize;

    // Header
    println!(
        "{:<name_w$}  {:>tok_w$}  {:>cost_w$}",
        "WORKTREE",
        "TOKENS IN/OUT",
        "COST",
        name_w = name_w,
        tok_w = tok_w,
        cost_w = cost_w
    );
    println!("{}", "─".repeat(name_w + 2 + tok_w + 2 + cost_w));

    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cost_cents = 0u64;

    for ps in &projects {
        // Project subtotal
        let mut proj_input = 0u64;
        let mut proj_output = 0u64;
        let mut proj_cost_cents = 0u64;

        for (_, stats) in &ps.worktrees {
            proj_input += stats.input;
            proj_output += stats.output;
            proj_cost_cents += stats.cost_cents;
        }

        // Print project header line
        let proj_tok = format!(
            "{} / {}",
            format_tokens(proj_input),
            format_tokens(proj_output)
        );
        println!(
            "{:<name_w$}  {:>tok_w$}  {:>cost_w$}",
            ps.name,
            proj_tok,
            format_cost(proj_cost_cents),
            name_w = name_w,
            tok_w = tok_w,
            cost_w = cost_w
        );

        // Per-worktree lines (indented)
        for (window_name, stats) in &ps.worktrees {
            if stats.sessions == 0 && stats.input == 0 {
                let label = format!("  {}", window_name);
                println!(
                    "{:<name_w$}  {:>tok_w$}  {:>cost_w$}",
                    label,
                    "—",
                    "—",
                    name_w = name_w,
                    tok_w = tok_w,
                    cost_w = cost_w
                );
            } else {
                let label = format!("  {}", window_name);
                let tok = format!(
                    "{} / {}",
                    format_tokens(stats.input),
                    format_tokens(stats.output)
                );
                println!(
                    "{:<name_w$}  {:>tok_w$}  {:>cost_w$}",
                    label,
                    tok,
                    format_cost(stats.cost_cents),
                    name_w = name_w,
                    tok_w = tok_w,
                    cost_w = cost_w
                );
            }
        }

        total_input += proj_input;
        total_output += proj_output;
        total_cost_cents += proj_cost_cents;
    }

    // Total line
    println!("{}", "─".repeat(name_w + 2 + tok_w + 2 + cost_w));
    let total_tok = format!(
        "{} / {}",
        format_tokens(total_input),
        format_tokens(total_output)
    );
    println!(
        "{:<name_w$}  {:>tok_w$}  {:>cost_w$}",
        "TOTAL",
        total_tok,
        format_cost(total_cost_cents),
        name_w = name_w,
        tok_w = tok_w,
        cost_w = cost_w
    );

    println!("\nNote: token counts reflect sessions started from each worktree path.");
    println!("Run `opencode stats` for full cross-project breakdown including tool usage.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"
┌────────────────────────────────────────────────────────┐
│                       OVERVIEW                         │
├────────────────────────────────────────────────────────┤
│Sessions                                            421 │
│Messages                                         14,718 │
│Days                                                  7 │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│                    COST & TOKENS                       │
├────────────────────────────────────────────────────────┤
│Total Cost                                        $3.42 │
│Avg Cost/Day                                      $0.49 │
│Avg Tokens/Session                                 1.8M │
│Median Tokens/Session                            172.1K │
│Input                                             85.9M │
│Output                                             4.4M │
│Cache Read                                       654.5M │
│Cache Write                                           0 │
└────────────────────────────────────────────────────────┘
"#;

    const ZERO_OUTPUT: &str = r#"
┌────────────────────────────────────────────────────────┐
│                       OVERVIEW                         │
├────────────────────────────────────────────────────────┤
│Sessions                                              0 │
│Messages                                              0 │
│Days                                                  7 │
└────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────┐
│                    COST & TOKENS                       │
├────────────────────────────────────────────────────────┤
│Total Cost                                        $0.00 │
│Avg Cost/Day                                      $0.00 │
│Avg Tokens/Session                                    0 │
│Median Tokens/Session                                 0 │
│Input                                                 0 │
│Output                                                0 │
│Cache Read                                            0 │
│Cache Write                                           0 │
└────────────────────────────────────────────────────────┘
"#;

    #[test]
    fn test_parse_sessions() {
        let row = parse_stats_output(SAMPLE_OUTPUT).unwrap();
        assert_eq!(row.sessions, 421);
    }

    #[test]
    fn test_parse_input_tokens() {
        let row = parse_stats_output(SAMPLE_OUTPUT).unwrap();
        assert_eq!(row.input, 85_900_000);
    }

    #[test]
    fn test_parse_output_tokens() {
        let row = parse_stats_output(SAMPLE_OUTPUT).unwrap();
        assert_eq!(row.output, 4_400_000);
    }

    #[test]
    fn test_parse_cache_read() {
        let row = parse_stats_output(SAMPLE_OUTPUT).unwrap();
        assert_eq!(row.cache_read, 654_500_000);
    }

    #[test]
    fn test_parse_cost() {
        let row = parse_stats_output(SAMPLE_OUTPUT).unwrap();
        assert_eq!(row.cost_cents, 342);
        assert_eq!(row.cost_str, "$3.42");
    }

    #[test]
    fn test_parse_zero_sessions_returns_some() {
        // Zero-session output should still parse (sessions == 0 is valid)
        let row = parse_stats_output(ZERO_OUTPUT).unwrap();
        assert_eq!(row.sessions, 0);
        assert_eq!(row.input, 0);
        assert_eq!(row.cost_cents, 0);
    }

    #[test]
    fn test_parse_empty_string_returns_none() {
        assert!(parse_stats_output("").is_none());
    }

    #[test]
    fn test_parse_token_count_millions() {
        assert_eq!(parse_token_count("85.9M"), 85_900_000);
        assert_eq!(parse_token_count("1.0M"), 1_000_000);
        assert_eq!(parse_token_count("654.5M"), 654_500_000);
    }

    #[test]
    fn test_parse_token_count_thousands() {
        assert_eq!(parse_token_count("172.1K"), 172_100);
        assert_eq!(parse_token_count("4.4M"), 4_400_000);
    }

    #[test]
    fn test_parse_token_count_raw() {
        assert_eq!(parse_token_count("421"), 421);
        assert_eq!(parse_token_count("0"), 0);
    }

    #[test]
    fn test_parse_token_count_with_commas() {
        assert_eq!(parse_token_count("14,718"), 14718);
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(85_900_000), "85.9M");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(172_100), "172.1K");
        assert_eq!(format_tokens(1_000), "1.0K");
    }

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(342), "$3.42");
        assert_eq!(format_cost(0), "$0.00");
        assert_eq!(format_cost(100), "$1.00");
    }
}
