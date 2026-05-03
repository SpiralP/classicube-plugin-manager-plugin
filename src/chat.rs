#[cfg(test)]
mod tests;

use std::mem;

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
    let s = mem::take(buf);
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
    out.push(mem::take(current));
    *visible = 0;
    *first_line = false;
    if let Some(c) = active_color {
        current.push_str(c);
    }
}
