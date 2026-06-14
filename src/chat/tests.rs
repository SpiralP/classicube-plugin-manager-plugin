use classicube_helpers::color;

use super::*;

#[test]
fn empty_input_yields_empty_line() {
    assert_eq!(wrap_chat(""), vec![String::new()]);
}

#[test]
fn short_message_unchanged() {
    assert_eq!(wrap_chat("hello world"), vec!["hello world".to_string()]);
}

#[test]
fn exact_width_stays_on_one_line() {
    let s = "a".repeat(WRAP_WIDTH);
    assert_eq!(wrap_chat(&s), vec![s]);
}

#[test]
fn word_boundary_wrap() {
    let w = "a".repeat(50);
    let input = format!("{w} {w}");
    // 50 + 1 + 50 = 101 > 80 → wrap before second word.
    assert_eq!(wrap_chat(&input), vec![w.clone(), format!("> {w}")]);
}

#[test]
fn color_preserved_on_continuation() {
    let w = "x".repeat(40);
    let input = format!("&a{w} {w} {w}");
    let lines = wrap_chat(&input);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("&a"));
    assert!(lines[1].starts_with("> &a"));
    assert!(lines[2].starts_with("> &a"));
}

#[test]
fn most_recent_color_used_for_continuation() {
    let w = "y".repeat(60);
    let input = format!("&a{w} &c{w}");
    let lines = wrap_chat(&input);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("&a"));
    assert!(lines[1].starts_with("> &c"));
}

#[test]
fn hard_break_long_word() {
    let w = "z".repeat(WRAP_WIDTH * 2 + 5);
    let lines = wrap_chat(&w);
    // 165 z's: line 1 takes 80, line 2 takes 78 (80 - "> "), line 3 takes the
    // remaining 7 plus its own "> " prefix.
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].chars().count(), WRAP_WIDTH);
    assert_eq!(lines[1].chars().count(), WRAP_WIDTH);
    assert_eq!(lines[2].chars().count(), 7 + 2);
    assert!(lines[1].starts_with("> "));
    assert!(lines[2].starts_with("> "));
}

#[test]
fn continuation_prefix_added_after_wrap() {
    let w = "a".repeat(50);
    let input = format!("{w} {w}");
    let lines = wrap_chat(&input);
    assert_eq!(lines.len(), 2);
    assert!(
        !lines[0].starts_with("> "),
        "first line must not be prefixed"
    );
    assert!(
        lines[1].starts_with("> "),
        "continuation line must start with > "
    );
}

#[test]
fn embedded_newlines_split_independently() {
    assert_eq!(
        wrap_chat("first\nsecond\nthird"),
        vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ]
    );
}

#[test]
fn empty_segment_between_newlines() {
    assert_eq!(
        wrap_chat("a\n\nb"),
        vec!["a".to_string(), String::new(), "b".to_string()]
    );
}

#[test]
fn color_codes_dont_count_toward_width() {
    let mut input = String::new();
    for _ in 0..40 {
        input.push_str("&a&b");
    }
    input.push_str(&"x".repeat(80));
    let lines = wrap_chat(&input);
    assert_eq!(lines.len(), 1);
}

#[test]
fn leading_whitespace_kept_on_first_line() {
    let lines = wrap_chat("  &alime/lime");
    assert_eq!(lines, vec!["  &alime/lime".to_string()]);
}

#[test]
fn version_arrow_no_prev() {
    assert_eq!(
        version_arrow(None, "v1.1.0", color::YELLOW),
        format!("{}v1.1.0", color::YELLOW),
    );
}

#[test]
fn version_arrow_same_version() {
    assert_eq!(
        version_arrow(Some("v1.1.0"), "v1.1.0", color::YELLOW),
        format!("{}v1.1.0", color::YELLOW),
    );
}

#[test]
fn version_arrow_distinct_prev() {
    assert_eq!(
        version_arrow(Some("v1.0.0"), "v1.1.0", color::YELLOW),
        format!(
            "{}v1.0.0 {}-> {}v1.1.0",
            color::YELLOW,
            color::PINK,
            color::YELLOW
        ),
    );
}

#[test]
fn realistic_error_message_wraps() {
    // anyhow chains can grow long; ensure they wrap and preserve color.
    let long_err = "io error: ".repeat(20);
    let input = format!("&cFailed to load config: &f{long_err}");
    let lines = wrap_chat(&input);
    assert!(lines.len() > 1);
    // First line carries the leading &c; later lines should re-emit &f (most recent).
    assert!(lines[0].starts_with("&c"));
    assert!(lines.iter().skip(1).all(|l| l.starts_with("> &f")));
}
