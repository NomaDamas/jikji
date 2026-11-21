//! What kind of request a `find` was, what was searched for it, and what to use instead when
//! Jikji is the wrong tool.
//!
//! Jikji matches the words of a request against file names, folders and the text of indexed
//! files. The plan states that scope per result, so an agent can tell a content match from a
//! name-only match, and points exact-text requests (regular expressions, code syntax, line
//! positions) at grep or a plain file read. Every signal is the shape of the query or a stored fact
//! about the returned files (their kind, whether their text is indexed), never a list of queries.

use std::path::Path;

use jikji_search::{FileKind, file_kind};
use serde_json::{Value, json};

use crate::query_shape::{ExactKind, exact_kind};

/// A note for `notes[]`: stable `code`, readable `message`.
pub(crate) struct PlanNote {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

pub(crate) struct Plan {
    /// `request_plan` object of the payload.
    pub(crate) json: Value,
    pub(crate) notes: Vec<PlanNote>,
}

/// `results` are `jikji_search::profile_results` entries with paths relative to the asked folder;
/// `coverage` is `jikji_search::coverage` for it, supplied when it can explain an empty result.
pub(crate) fn plan(
    query: &str,
    folder: &Path,
    results: &[Value],
    coverage: Option<&Value>,
    words: &[String],
) -> Plan {
    let trimmed = query.trim();
    let exact = exact_kind(trimmed);
    let mut signals: Vec<String> = Vec::new();
    let mut notes = Vec::new();
    let mut not_searched: Vec<&str> = Vec::new();
    let mut use_instead = None;

    // The request is about non-text files when the best result is one; a media file trailing a
    // document that matched is only noted per result.
    let top_is_name_only_media = results
        .first()
        .is_some_and(|r| result_kind(r).is_media() && !text_indexed(r));
    let media_hits = results
        .iter()
        .filter(|r| result_kind(r).is_media() && !text_indexed(r))
        .count();
    let intent = if trimmed.is_empty() {
        "empty_query"
    } else if let Some(kind) = exact {
        signals.push(kind.signal().to_owned());
        not_searched.extend(["regular_expressions", "exact_punctuation", "line_numbers"]);
        use_instead = Some(exact_alternative(kind, trimmed, folder, results));
        notes.push(PlanNote {
            code: "pattern_syntax_not_supported",
            message: "Jikji ranks files by the words in names and indexed text. It does not evaluate regular expressions or exact code symbols and punctuation, and returns files, not line numbers. Use these results to pick files, then grep inside those files for the exact pattern (see request_plan.use_instead).".to_owned(),
        });
        "exact_text"
    } else if is_identifier(trimmed) && code_file_hit(results) {
        signals.push("identifier_shape".to_owned());
        notes.push(PlanNote {
            code: "identifier_files_only",
            message: "The query looks like a code identifier. Jikji found source files whose name or text contains it; it does not return lines or tell a definition from a use. Run grep -n for the identifier inside these files to get the lines.".to_owned(),
        });
        "code_identifier"
    } else if has_media_extension_token(trimmed) || top_is_name_only_media {
        if has_media_extension_token(trimmed) {
            signals.push("extension_token".to_owned());
        }
        if top_is_name_only_media {
            signals.push("top_result_non_text".to_owned());
        }
        "non_text_files"
    } else {
        "files_by_name_or_text"
    };

    if media_hits > 0 && exact.is_none() {
        not_searched.push("image_audio_video_content");
        notes.push(PlanNote {
            code: "non_text_name_match_only",
            message: format!(
                "{media_hits} of {} results are images, audio or video. They matched on file name, folder or extension only: Jikji does not read what is in them (no OCR or transcription unless `jikji deep-index` was authorised), so a hit does not mean their content is about the query.",
                results.len()
            ),
        });
    }
    if results.is_empty()
        && !trimmed.is_empty()
        && exact.is_none()
        && let Some(count) = name_only_media(coverage)
        && count > 0
    {
        not_searched.push("image_audio_video_content");
        notes.push(PlanNote {
            code: "non_text_content_not_searchable",
            message: format!(
                "This folder holds {count} images, audio or video files whose content is not indexed. If the target is one of them, search by its file name, folder or extension (a date, place or title in the name) instead of by what it shows or says."
            ),
        });
    }
    not_searched.sort_unstable();
    not_searched.dedup();

    let mut json = json!({
        "intent": intent,
        "signals": signals,
        "searched": ["file_name", "path", "indexed_text"],
        "not_searched": not_searched,
        "query_words": words,
    });
    if let Some(alternative) = use_instead {
        json["use_instead"] = alternative;
    }
    Plan { json, notes }
}

fn result_kind(result: &Value) -> FileKind {
    kind_from_name(result["file_kind"].as_str().unwrap_or(""))
}

/// `profile_results` stores the kind as a word; map it back (an unknown word is `Other`).
fn kind_from_name(name: &str) -> FileKind {
    match name {
        "document" => FileKind::Document,
        "text" => FileKind::Text,
        "code" => FileKind::Code,
        "image" => FileKind::Image,
        "audio" => FileKind::Audio,
        "video" => FileKind::Video,
        "archive" => FileKind::Archive,
        _ => FileKind::Other,
    }
}

fn text_indexed(result: &Value) -> bool {
    result["text_indexed"].as_bool().unwrap_or(false)
}

fn name_only_media(coverage: Option<&Value>) -> Option<i64> {
    let by_kind = coverage?.get("by_kind")?.as_object()?;
    Some(
        ["image", "audio", "video"]
            .iter()
            .filter_map(|kind| by_kind.get(*kind))
            .map(|entry| {
                entry["files"].as_i64().unwrap_or(0) - entry["text_indexed"].as_i64().unwrap_or(0)
            })
            .sum(),
    )
}

/// One token made of identifier characters that reads as a name rather than a word:
/// `snake_case`, `camelCase` or `PascalCase` with a lower-to-upper step.
fn is_identifier(query: &str) -> bool {
    if query.len() < 4
        || query.chars().any(char::is_whitespace)
        || !query.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        || !query.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return false;
    }
    let chars: Vec<char> = query.chars().collect();
    chars.contains(&'_')
        || chars
            .windows(2)
            .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
}

/// One of the best results is a source file: the identifier was found in code.
fn code_file_hit(results: &[Value]) -> bool {
    results
        .iter()
        .take(3)
        .any(|result| result_kind(result) == FileKind::Code)
}

/// `.mp4`, `*.jpg`: an extension written the way extensions are written, never a bare word.
fn has_media_extension_token(query: &str) -> bool {
    query.split_whitespace().any(|word| {
        let word = word.trim_start_matches('*');
        word.starts_with('.')
            && word.len() > 1
            && (file_kind(word).is_media() || file_kind(word) == FileKind::Archive)
    })
}

fn exact_alternative(kind: ExactKind, query: &str, folder: &Path, results: &[Value]) -> Value {
    let narrow_to: Vec<&str> = results
        .iter()
        .filter_map(|result| result["path"].as_str())
        .take(5)
        .collect();
    match kind {
        ExactKind::Line => {
            let named = line_reference_file(query);
            let narrow_to: Vec<&str> = match &named {
                Some(name) => results
                    .iter()
                    .filter_map(|result| result["path"].as_str())
                    .filter(|path| path.rsplit('/').next() == Some(name.as_str()))
                    .collect(),
                None => narrow_to,
            };
            let mut alternative = json!({
                "tool": "read",
                "why": "A line number or position is a place inside one file: open that file at that line with a normal file read. Jikji returns files, not lines.",
                "narrow_to": narrow_to,
            });
            if let Some(name) = named {
                alternative["file_name"] = json!(name);
            }
            alternative
        }
        ExactKind::Regex | ExactKind::Code => {
            let mode = if kind == ExactKind::Regex {
                "-rnE"
            } else {
                "-rnF"
            };
            json!({
                "tool": "grep",
                "why": "Exact text, code symbols and regular expressions are matched by grep, not by an index of words.",
                "command": format!("grep {mode} -e {} {}", shell_quote(query), shell_quote(&folder.display().to_string())),
                "narrow_to": narrow_to,
            })
        }
    }
}

/// The file in a `name.ext:LINE` reference, if the query has one.
fn line_reference_file(query: &str) -> Option<String> {
    query.split_whitespace().find_map(|word| {
        let (file, rest) = word.split_once(':')?;
        let numeric = rest.chars().next()?.is_ascii_digit();
        (numeric
            && file
                .rsplit_once('.')
                .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty()))
        .then(|| file.to_owned())
    })
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::Plan;

    /// A plan for a request whose words are not under test.
    fn plan(query: &str, folder: &Path, results: &[Value], coverage: Option<&Value>) -> Plan {
        super::plan(query, folder, results, coverage, &[])
    }

    fn result(path: &str, kind: &str, indexed: bool, matched: &[&str]) -> Value {
        json!({"path": path, "file_kind": kind, "text_indexed": indexed, "matched_in": matched})
    }

    fn codes(p: &super::Plan) -> Vec<&'static str> {
        p.notes.iter().map(|n| n.code).collect()
    }

    #[test]
    fn exact_requests_point_to_grep_or_a_file_read_instead_of_pretending() {
        let regex = plan("foo\\d+bar", Path::new("/w/it's"), &[], None);
        assert_eq!(regex.json["intent"], "exact_text");
        assert_eq!(regex.json["use_instead"]["tool"], "grep");
        assert_eq!(
            regex.json["use_instead"]["command"],
            "grep -rnE -e 'foo\\d+bar' '/w/it'\\''s'"
        );
        let code = plan("Vec<String>", Path::new("/w"), &[], None);
        assert!(
            code.json["use_instead"]["command"]
                .as_str()
                .unwrap()
                .starts_with("grep -rnF")
        );
        let line = plan(
            "main.py:12",
            Path::new("/w"),
            &[result("a/main.py", "code", true, &[])],
            None,
        );
        assert_eq!(line.json["use_instead"]["tool"], "read");
        assert_eq!(line.json["use_instead"]["narrow_to"][0], "a/main.py");
        assert_eq!(codes(&line), ["pattern_syntax_not_supported"]);
    }

    #[test]
    fn media_name_matches_are_labelled_as_not_content_matches() {
        let results = [
            result("a/trip.jpg", "image", false, &["name_path"]),
            result("b/notes.txt", "text", true, &["text"]),
        ];
        let p = plan("trip", Path::new("/w"), &results, None);
        assert_eq!(codes(&p), ["non_text_name_match_only"]);
        assert_eq!(p.json["intent"], "non_text_files");
        assert!(
            p.json["not_searched"]
                .to_string()
                .contains("image_audio_video_content")
        );
        // Once media text was indexed on request the label disappears.
        let indexed = [result("a/clip.mp4", "video", true, &["text"])];
        assert!(
            plan("clip", Path::new("/w"), &indexed, None)
                .notes
                .is_empty()
        );
    }

    #[test]
    fn an_empty_result_explains_unsearchable_media_from_coverage() {
        let coverage = json!({"by_kind": {"image": {"files": 7, "text_indexed": 0}, "text": {"files": 3, "text_indexed": 3}}});
        let p = plan("beach sunset", Path::new("/w"), &[], Some(&coverage));
        assert_eq!(codes(&p), ["non_text_content_not_searchable"]);
        assert!(p.notes[0].message.contains('7'));
        let none = json!({"by_kind": {"text": {"files": 3, "text_indexed": 3}}});
        assert!(
            plan("beach", Path::new("/w"), &[], Some(&none))
                .notes
                .is_empty()
        );
    }

    #[test]
    fn identifiers_are_files_only_when_a_source_file_is_among_the_top_results() {
        let hit = [result("src/lib.rs", "code", true, &["text"])];
        let p = plan("parse_config", Path::new("/w"), &hit, None);
        assert_eq!(p.json["intent"], "code_identifier");
        assert_eq!(codes(&p), ["identifier_files_only"]);
        // A snake_case name that matched a document is just a file request.
        let by_name = [result("report_final.md", "text", true, &["name_path"])];
        assert_eq!(
            plan("report_final", Path::new("/w"), &by_name, None).json["intent"],
            "files_by_name_or_text"
        );
    }

    #[test]
    fn ordinary_requests_and_extension_tokens() {
        let p = plan(
            "budget renewal",
            Path::new("/w"),
            &[result("a.txt", "text", true, &["text"])],
            None,
        );
        assert_eq!(p.json["intent"], "files_by_name_or_text");
        assert!(p.notes.is_empty());
        assert_eq!(
            plan("*.mp4", Path::new("/w"), &[], None).json["intent"],
            "non_text_files"
        );
        assert_eq!(
            plan("raw data", Path::new("/w"), &[], None).json["intent"],
            "files_by_name_or_text"
        );
        assert_eq!(
            plan("  ", Path::new("/w"), &[], None).json["intent"],
            "empty_query"
        );
    }

    #[test]
    fn a_document_hit_followed_by_a_media_file_is_still_a_text_request() {
        let results = [
            result("a/notes.docx", "document", true, &["name_path"]),
            result("b/clip.mp4", "video", false, &["name_path"]),
        ];
        let p = plan("notes clip", Path::new("/w"), &results, None);
        assert_eq!(p.json["intent"], "files_by_name_or_text");
        assert_eq!(codes(&p), ["non_text_name_match_only"]);
    }
}
