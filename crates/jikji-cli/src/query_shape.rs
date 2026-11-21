//! Tells a request for *files* from a request for *code or text positions*.
//!
//! Jikji ranks files by the words in their names and indexed text. A regular expression, an
//! exact code symbol or a line number is answered by grep, not by an index of words. This
//! module only decides which of the two a query looks like, from its punctuation and shape,
//! never from a list of known queries. Ordinary file names are full of brackets, parentheses,
//! question marks and dots, so a character alone never decides: it must form regex or code
//! syntax (an escape, a quantified group, a call, a generic, a path-like reference...).

/// What a query that Jikji cannot answer by words looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactKind {
    /// Regular expression: escapes, anchors, alternation, quantified groups, character classes.
    Regex,
    /// Code or shell syntax: calls, generics, `::`, `->`, variables, command flags.
    Code,
    /// A position inside a file: `line 42`, `줄 57`, `#L88`, `main.py:120`.
    Line,
}

impl ExactKind {
    pub(crate) fn signal(self) -> &'static str {
        match self {
            Self::Regex => "regex_syntax",
            Self::Code => "code_syntax",
            Self::Line => "line_position",
        }
    }
}

/// The kind of exact-matching request the query is, or `None` for an ordinary file request.
pub(crate) fn exact_kind(query: &str) -> Option<ExactKind> {
    let chars: Vec<char> = query.chars().collect();
    if has_position_reference(query) {
        Some(ExactKind::Line)
    } else if has_regex_syntax(&chars) {
        Some(ExactKind::Regex)
    } else if has_code_syntax(&chars) || has_command_flags(query) {
        Some(ExactKind::Code)
    } else {
        None
    }
}

/// A shell command line: a later word that is an option (`-rn`, `--include`).
fn has_command_flags(query: &str) -> bool {
    query.split_whitespace().skip(1).any(|word| {
        let name = word.trim_start_matches('-');
        let dashes = word.len() - name.len();
        // `--include`, or a short cluster like `-rn`; a longer single-dash word is a name (`-draft`).
        (dashes == 2 || (dashes == 1 && name.len() <= 3))
            && name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn has_regex_syntax(chars: &[char]) -> bool {
    let n = chars.len();
    for (i, &ch) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|j| chars[j]);
        match ch {
            // `\d`, `\b`, `\.`, `\s`: an escape, never part of a file name.
            '\\' if next.is_some_and(|c| !c.is_whitespace()) => return true,
            // Anchors at the ends of the whole pattern.
            '^' if i == 0 && n > 1 => return true,
            '$' if i + 1 == n && n > 1 => return true,
            // A lone `*` between words: a wildcard or `SELECT *`, never part of a name.
            '*' if prev.is_some_and(char::is_whitespace)
                && next.is_some_and(char::is_whitespace) =>
            {
                return true;
            }
            // Alternation or boolean operators.
            '|' => return true,
            // Non-capturing groups and inline flags: `(?:`, `(?i)`.
            '(' if next == Some('?') => return true,
            // A quantifier after something quantifiable: `.*`, `)+`, `]*`, `)?`.
            '*' | '+' if prev.is_some_and(|c| c == '.' || c == ')' || c == ']') => return true,
            '?' if prev.is_some_and(|c| c == ')' || c == ']') => return true,
            // Repetition counts: `{3}`, `{2,5}`.
            '{' if is_repetition(&chars[i..]) => return true,
            // A character class with a range or class escape: `[A-Z]`, `[0-9a-f]`, `[^abc]`.
            '[' if is_char_class(&chars[i..]) => return true,
            _ => {}
        }
    }
    false
}

fn is_repetition(rest: &[char]) -> bool {
    let Some(end) = rest.iter().position(|&c| c == '}') else {
        return false;
    };
    let inner: String = rest[1..end].iter().collect();
    !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit() || c == ',')
}

fn is_char_class(rest: &[char]) -> bool {
    let Some(end) = rest.iter().position(|&c| c == ']') else {
        return false;
    };
    let inner = &rest[1..end];
    inner.first() == Some(&'^')
        || inner
            .windows(3)
            .any(|w| w[1] == '-' && is_word(w[0]) && is_word(w[2]))
        || inner.contains(&'\\')
}

fn has_code_syntax(chars: &[char]) -> bool {
    for (i, &ch) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let after_word = prev.is_some_and(is_word);
        match ch {
            ':' if next == Some(':') => return true,
            '-' if next == Some('>') => return true,
            '=' if matches!(next, Some('>' | '=')) => return true,
            '&' if next == Some('&') => return true,
            // A shell or environment variable: `$HOME`, `${x}`.
            '$' if next.is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '{') => {
                return true;
            }
            // A call or definition glued to a name: `parse(`, `a.b.c()`, `__init__(self`.
            '(' if after_word => return true,
            // A generic or template: `Vec<String>`, `Option<u8>`.
            '<' if after_word && chars[i + 1..].contains(&'>') => return true,
            // Member access plus index glued to a name: `self.cache[key]`, `a.b[0]`.
            '[' if after_word && chars[i + 1..].contains(&']') && has_member_access(chars, i) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// `a.b[0]` or `self.cache[key]`: a `.` inside the identifier before the bracket.
fn has_member_access(chars: &[char], bracket: usize) -> bool {
    let start = chars[..bracket]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |p| p + 1);
    chars[start..bracket].contains(&'.')
}

/// `line 42`, `lines 10-20`, `줄 57`, `3번째 줄`, `#L88`, `file.py:120`, `file.py:120:5`.
fn has_position_reference(query: &str) -> bool {
    let lower = query.to_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1).copied();
        let next_is_number = next.is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit()));
        if matches!(*tok, "line" | "lines" | "줄" | "행" | "라인") && next_is_number {
            return true;
        }
        if tok.ends_with("번째") && matches!(next, Some("줄" | "행" | "라인")) {
            return true;
        }
        if is_line_anchor(tok) || is_file_line_ref(tok) {
            return true;
        }
    }
    false
}

fn is_line_anchor(token: &str) -> bool {
    token.strip_prefix("#l").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == 'l')
    })
}

/// `name.ext:LINE[:COL]`: a file reference with a position, as compilers and editors print them.
fn is_file_line_ref(token: &str) -> bool {
    let mut parts = token.split(':');
    let Some(file) = parts.next() else {
        return false;
    };
    let numbers: Vec<&str> = parts.collect();
    let looks_like_file = file.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 5
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    looks_like_file
        && (1..=2).contains(&numbers.len())
        && numbers
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{ExactKind, exact_kind};

    #[test]
    fn exact_requests_are_classified_by_what_they_ask_for() {
        let cases: &[(&str, ExactKind)] = &[
            (r"foo\d+bar", ExactKind::Regex),
            ("^import .*requests$", ExactKind::Regex),
            ("[A-Z]{3}-\\d{4}", ExactKind::Regex),
            ("cat|dog", ExactKind::Regex),
            ("(?i)needle", ExactKind::Regex),
            ("SELECT * FROM users WHERE id=", ExactKind::Regex),
            ("fn run_find(", ExactKind::Code),
            ("self.cache[key]", ExactKind::Code),
            ("Vec<String>", ExactKind::Code),
            ("std::collections::HashMap", ExactKind::Code),
            ("x => x + 1", ExactKind::Code),
            ("def __init__(self", ExactKind::Code),
            ("a.b.c()", ExactKind::Code),
            ("$HOME/.config", ExactKind::Code),
            ("grep -rn needle", ExactKind::Code),
            ("cargo build --release", ExactKind::Code),
            ("line 42 of main.py", ExactKind::Line),
            ("lines 10-20 of README", ExactKind::Line),
            ("main.py:120", ExactKind::Line),
            ("main.py:120:5", ExactKind::Line),
            ("#L88", ExactKind::Line),
            ("줄 57", ExactKind::Line),
            ("3번째 줄", ExactKind::Line),
        ];
        for (query, expected) in cases {
            assert_eq!(exact_kind(query), Some(*expected), "{query:?}");
        }
    }

    #[test]
    fn ordinary_file_names_and_questions_are_not_flagged() {
        for q in [
            "budget report (final)",
            "[초안] 계획",
            "구버나모 [사본] 정리",
            "where is the contract file?",
            "what is a lease?",
            "R&D notes",
            "don't panic",
            "100% done",
            "meeting 2024-03-15",
            "report_v2.1 draft",
            "회의록, 최종본: 3월",
            "계약서 (v2) draft",
            "version 3.2.1",
            "notes.txt",
            "invoice 2023:11",
            "design doc - phase 1",
            "Smith - John letter",
            "q3 -draft",
            "release -notes final",
            "",
        ] {
            assert_eq!(exact_kind(q), None, "{q:?} is a plain file request");
        }
    }
}
