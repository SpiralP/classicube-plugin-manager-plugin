use classicube_helpers::{async_manager, chat};

const WRAP_WIDTH: usize = 80;

pub fn print_wrapped<S: AsRef<str>>(s: S) {
    for line in wrap_chat(s.as_ref()) {
        chat::print(line);
    }
}

pub async fn print_async<S: Into<String> + Send + 'static>(s: S) {
    async_manager::run_on_main_thread(async move {
        print_wrapped(s.into());
    })
    .await;
}

fn wrap_chat(msg: &str) -> Vec<String> {
    if msg.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for segment in msg.split('\n') {
        wrap_segment(segment, &mut out);
    }
    out
}

#[derive(Debug)]
enum Atom {
    Color(String),
    Word(String),
    Whitespace(String),
}

fn tokenize(s: &str) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut buf = String::new();
    let mut buf_is_word: Option<bool> = None;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '&'
            && let Some(&next) = chars.peek()
            && next.is_ascii_alphanumeric()
        {
            push_buf(&mut atoms, &mut buf, &mut buf_is_word);
            let mut code = String::with_capacity(2);
            code.push('&');
            code.push(chars.next().unwrap());
            atoms.push(Atom::Color(code));
            continue;
        }
        let is_word = !c.is_whitespace();
        if buf_is_word != Some(is_word) {
            push_buf(&mut atoms, &mut buf, &mut buf_is_word);
            buf_is_word = Some(is_word);
        }
        buf.push(c);
    }
    push_buf(&mut atoms, &mut buf, &mut buf_is_word);

    atoms
}

fn push_buf(atoms: &mut Vec<Atom>, buf: &mut String, kind: &mut Option<bool>) {
    if buf.is_empty() {
        return;
    }
    let s = std::mem::take(buf);
    match kind.take() {
        Some(true) => atoms.push(Atom::Word(s)),
        Some(false) => atoms.push(Atom::Whitespace(s)),
        None => unreachable!("kind set when buf non-empty"),
    }
}

fn wrap_segment(segment: &str, out: &mut Vec<String>) {
    let atoms = tokenize(segment);
    if atoms.is_empty() {
        out.push(String::new());
        return;
    }

    let mut current = String::new();
    let mut visible: usize = 0;
    let mut active_color: Option<String> = None;
    let mut first_line = true;

    for atom in atoms {
        match atom {
            Atom::Color(code) => {
                current.push_str(&code);
                active_color = Some(code);
            }
            Atom::Whitespace(ws) => {
                let w = ws.chars().count();
                if visible == 0 {
                    if first_line {
                        // Preserve leading whitespace on the original line (e.g. list-row indent).
                        current.push_str(&ws);
                        visible += w;
                    }
                    // Otherwise: drop leading whitespace on a continuation line.
                    continue;
                }
                if visible + w > WRAP_WIDTH {
                    flush_line(
                        out,
                        &mut current,
                        &mut visible,
                        &mut first_line,
                        &active_color,
                    );
                    continue;
                }
                current.push_str(&ws);
                visible += w;
            }
            Atom::Word(word) => {
                let wlen = word.chars().count();
                if wlen > WRAP_WIDTH {
                    if visible > 0 {
                        flush_line(
                            out,
                            &mut current,
                            &mut visible,
                            &mut first_line,
                            &active_color,
                        );
                    }
                    let chars: Vec<char> = word.chars().collect();
                    let mut idx = 0;
                    while idx < chars.len() {
                        let space = WRAP_WIDTH - visible;
                        let take = space.min(chars.len() - idx);
                        for _ in 0..take {
                            current.push(chars[idx]);
                            idx += 1;
                        }
                        visible += take;
                        if idx < chars.len() {
                            flush_line(
                                out,
                                &mut current,
                                &mut visible,
                                &mut first_line,
                                &active_color,
                            );
                        }
                    }
                } else if visible + wlen > WRAP_WIDTH {
                    flush_line(
                        out,
                        &mut current,
                        &mut visible,
                        &mut first_line,
                        &active_color,
                    );
                    current.push_str(&word);
                    visible += wlen;
                } else {
                    current.push_str(&word);
                    visible += wlen;
                }
            }
        }
    }

    out.push(current);
}

fn flush_line(
    out: &mut Vec<String>,
    current: &mut String,
    visible: &mut usize,
    first_line: &mut bool,
    active_color: &Option<String>,
) {
    while current.ends_with(|c: char| c.is_whitespace()) {
        current.pop();
    }
    out.push(std::mem::take(current));
    *visible = 0;
    *first_line = false;
    if let Some(c) = active_color {
        current.push_str(c);
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(wrap_chat(&input), vec![w.clone(), w]);
    }

    #[test]
    fn color_preserved_on_continuation() {
        let w = "x".repeat(40);
        let input = format!("&a{w} {w} {w}");
        let lines = wrap_chat(&input);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("&a"));
        assert!(lines[1].starts_with("&a"));
        assert!(lines[2].starts_with("&a"));
    }

    #[test]
    fn most_recent_color_used_for_continuation() {
        let w = "y".repeat(60);
        let input = format!("&a{w} &c{w}");
        let lines = wrap_chat(&input);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("&a"));
        assert!(lines[1].starts_with("&c"));
    }

    #[test]
    fn hard_break_long_word() {
        let w = "z".repeat(WRAP_WIDTH * 2 + 5);
        let lines = wrap_chat(&w);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].chars().count(), WRAP_WIDTH);
        assert_eq!(lines[1].chars().count(), WRAP_WIDTH);
        assert_eq!(lines[2].chars().count(), 5);
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
    fn realistic_error_message_wraps() {
        // anyhow chains can grow long; ensure they wrap and preserve color.
        let long_err = "io error: ".repeat(20);
        let input = format!("&cFailed to load config: &f{long_err}");
        let lines = wrap_chat(&input);
        assert!(lines.len() > 1);
        // First line carries the leading &c; later lines should re-emit &f (most recent).
        assert!(lines[0].starts_with("&c"));
        assert!(lines.iter().skip(1).all(|l| l.starts_with("&f")));
    }
}
