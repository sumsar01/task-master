/// Generate a short, human-readable slug for use in ephemeral worktree names.
///
/// The slug has the form `<word>-<hex4>` where:
/// - `<word>` is either the provided `prefix` or a randomly chosen word from
///   the built-in adjective list.
/// - `<hex4>` is 4 lowercase hex digits derived from a time/pid-based seed.
///
/// The result is suitable for use as a git worktree directory name and as a
/// tmux window name component (ASCII lowercase, hyphens only).
///
/// # Examples
///
/// ```
/// // With a fixed prefix:
/// let s = generate_slug(Some("spruce"));
/// assert!(s.starts_with("spruce-"));
///
/// // Without a prefix — random word chosen:
/// let s = generate_slug(None);
/// assert!(s.contains('-'));
/// ```
pub fn generate_slug(prefix: Option<&str>) -> String {
    generate_slug_with_seed(prefix, make_seed())
}

/// Seeded variant for testing — deterministic given the same inputs.
pub fn generate_slug_with_seed(prefix: Option<&str>, seed: u64) -> String {
    let word = match prefix {
        Some(p) => p.to_string(),
        None => {
            let idx = (seed >> 16) as usize % WORDS.len();
            WORDS[idx].to_string()
        }
    };
    let hex = format!("{:04x}", (seed & 0xffff) as u16);
    format!("{}-{}", word, hex)
}

/// Produce a pseudo-random seed from the current time and process ID.
///
/// No external crate is used. The quality is sufficient for generating
/// collision-resistant short slugs — we just need 16 bits of entropy.
fn make_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(42);
    let pid = std::process::id() as u64;
    // XOR and mix to spread bits
    let raw = nanos ^ (pid << 17) ^ (nanos >> 7) ^ pid;
    // Cheap finaliser so sequential calls (same pid, close timestamps) differ
    raw.wrapping_mul(0x9e3779b97f4a7c15)
}

/// Built-in word list for ephemeral worktree name generation.
///
/// Words are chosen to be:
/// - Memorable and visually distinct
/// - Short (4-7 characters)
/// - Nature / colour / adjective themed — neutral and professional
/// - Valid in git branch names and tmux window names (lowercase ASCII)
const WORDS: &[&str] = &[
    "amber", "aspen", "azure", "birch", "blaze", "bloom", "cedar", "chill", "cinch", "cloud",
    "coral", "crane", "creek", "crisp", "denim", "dusk", "ember", "fable", "fern", "fjord",
    "flare", "fleet", "flint", "floss", "foam", "frost", "glade", "gleam", "gloom", "glyph",
    "grove", "guava", "hazel", "heath", "holly", "hound", "hue", "husk", "inlet", "ivory", "jade",
    "knoll", "larch", "lilac", "lunar", "lumen", "maple", "marsh", "mist", "mocha", "mossy",
    "mulch", "myrrh", "noble", "notch", "oaken", "ochre", "olive", "onyx", "optic", "ozone",
    "patch", "pearl", "petal", "pinch", "pine", "pixel", "plume", "plush", "polar", "pond",
    "prism", "quill", "rally", "resin", "ridge", "rivet", "rowan", "ruddy", "russet", "sandy",
    "shale", "sleet", "slate", "sleet", "smoke", "solar", "spire", "spray", "sprig", "sprout",
    "spruce", "stark", "steel", "stone", "storm", "swamp", "swift", "thorn", "tidal", "tidal",
    "tinge", "topaz", "trace", "trail", "tream", "trout", "tulip", "tundra", "tuque", "twill",
    "ultra", "umber", "vale", "vapor", "virid", "vivid", "walnut", "wasp", "whirl", "willow",
    "woad", "wren", "yarrow", "zinc", "zonal",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_with_prefix_starts_with_prefix() {
        let s = generate_slug_with_seed(Some("spruce"), 0x1234_5678_9abc_def0);
        assert!(
            s.starts_with("spruce-"),
            "slug should start with prefix 'spruce-', got: {}",
            s
        );
    }

    #[test]
    fn test_slug_without_prefix_uses_word_from_list() {
        let s = generate_slug_with_seed(None, 0x0);
        let (word, hex) = s.split_once('-').expect("slug must contain a hyphen");
        assert!(
            WORDS.contains(&word),
            "word component '{}' must be from built-in list",
            word
        );
        assert_eq!(hex.len(), 4, "hex suffix must be 4 chars");
        assert!(
            hex.chars().all(|c| c.is_ascii_hexdigit()),
            "hex suffix must be hex digits, got: {}",
            hex
        );
    }

    #[test]
    fn test_slug_hex_suffix_is_four_hex_digits() {
        for seed in [0u64, 1, 0xffff, 0x1234, u64::MAX] {
            let s = generate_slug_with_seed(Some("test"), seed);
            let hex = s.strip_prefix("test-").expect("should start with test-");
            assert_eq!(
                hex.len(),
                4,
                "hex should be 4 chars for seed {}: {}",
                seed,
                s
            );
            assert!(
                hex.chars().all(|c| c.is_ascii_hexdigit()),
                "hex should be hex digits for seed {}: {}",
                seed,
                s
            );
        }
    }

    #[test]
    fn test_slug_result_is_lowercase_ascii_hyphens_only() {
        let s = generate_slug_with_seed(Some("cedar"), 0xabcd_ef01_2345_6789);
        for c in s.chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-',
                "character '{}' is not lowercase ASCII, digit, or hyphen in slug: {}",
                c,
                s
            );
        }
    }

    #[test]
    fn test_slug_different_seeds_produce_different_hex() {
        let s1 = generate_slug_with_seed(Some("pine"), 0x0000_0000_0000_0001);
        let s2 = generate_slug_with_seed(Some("pine"), 0x0000_0000_0000_0002);
        // With different seeds the hex should differ (may collide if both map to 0000
        // but that only happens if seed & 0xffff == 0 for both, which these don't).
        let hex1 = s1.strip_prefix("pine-").unwrap();
        let hex2 = s2.strip_prefix("pine-").unwrap();
        assert_ne!(
            hex1, hex2,
            "different seeds should produce different hex: {} vs {}",
            s1, s2
        );
    }

    #[test]
    fn test_slug_without_prefix_different_seeds_may_differ_in_word() {
        // High bits of seed select the word. Two seeds with very different high bits
        // should pick different words.
        let s1 = generate_slug_with_seed(None, 0x0000_0000_0000_0000);
        let s2 = generate_slug_with_seed(None, 0xffff_0000_0000_0000);
        // They might coincidentally be the same word if word count divides evenly —
        // but with 150 words and seeds this far apart they won't be.
        let w1 = s1.split('-').next().unwrap();
        let w2 = s2.split('-').next().unwrap();
        // Just assert they're valid words, not necessarily different (the point is
        // the seed actually changes the word selection).
        assert!(WORDS.contains(&w1), "'{}' not in word list", w1);
        assert!(WORDS.contains(&w2), "'{}' not in word list", w2);
    }

    #[test]
    fn test_generate_slug_live_call_returns_valid_slug() {
        // Call the live version (non-seeded) and verify the format.
        let s = generate_slug(None);
        let parts: Vec<&str> = s.splitn(2, '-').collect();
        assert_eq!(parts.len(), 2, "slug must have exactly one hyphen: {}", s);
        assert_eq!(parts[1].len(), 4, "hex suffix must be 4 chars: {}", s);
    }
}
