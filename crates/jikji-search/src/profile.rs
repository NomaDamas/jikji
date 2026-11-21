//! What kind of file each result is and what Jikji actually searched in it.
//!
//! The index holds a file's name and path for every file, but its text only when a parser could
//! read it. A photo, a recording or a video is therefore found by name, folder and extension, never
//! by what it shows or says (unless the user ran the opt-in media indexing). Reporting that per
//! result, from what the index really stored, keeps a name match from being mistaken for a content
//! match without guessing from the query.

use std::collections::BTreeMap;
use std::path::Path;

use jikji_core::Result;
use jikji_core::storage::{database_path, open_database, root_id};
use rusqlite::params;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::io::sqlite_error;
use crate::scope::like_pattern;
use crate::tokenizer::{compact_lookup_text, term_variants, tokens};

/// Broad kind of a file, decided from its extension (a file-format fact, not a query guess).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileKind {
    Document,
    Text,
    Code,
    Image,
    Audio,
    Video,
    Archive,
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Text => "text",
            Self::Code => "code",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Archive => "archive",
            Self::Other => "other",
        }
    }

    /// Images, audio and video: their content is not text and is not searched by default.
    pub fn is_media(self) -> bool {
        matches!(self, Self::Image | Self::Audio | Self::Video)
    }
}

const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "ppt", "pptx", "pps", "ppsx", "xls", "xlsx", "hwp", "hwpx", "odt", "ods",
    "odp", "rtf", "epub", "pages", "numbers", "key",
];
const TEXT: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "log", "json", "jsonl", "yaml", "yml", "xml", "ini",
    "cfg", "toml", "html", "htm", "eml", "ics", "srt", "vtt", "tex", "rst",
];
const CODE: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "kt", "swift", "c", "h", "cc", "cpp",
    "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "sql", "lua", "r", "scala", "dart", "vue",
    "svelte", "css", "scss", "gradle", "proto",
];
const IMAGE: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "webp", "bmp", "gif", "heic", "heif", "svg", "raw", "psd",
];
const AUDIO: &[&str] = &[
    "mp3", "wav", "m4a", "flac", "ogg", "aac", "opus", "wma", "aiff",
];
const VIDEO: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "mpg", "mpeg",
];
const ARCHIVE: &[&str] = &[
    "zip", "jar", "war", "tar", "tgz", "tbz", "txz", "7z", "rar", "gz", "bz2", "xz",
];

/// Kind for an extension given with or without the leading dot, in any case.
pub fn file_kind(extension: &str) -> FileKind {
    let ext = extension.trim_start_matches('.').to_lowercase();
    let ext = ext.as_str();
    let table: [(&[&str], FileKind); 7] = [
        (DOCUMENT, FileKind::Document),
        (TEXT, FileKind::Text),
        (CODE, FileKind::Code),
        (IMAGE, FileKind::Image),
        (AUDIO, FileKind::Audio),
        (VIDEO, FileKind::Video),
        (ARCHIVE, FileKind::Archive),
    ];
    table
        .iter()
        .find(|(exts, _)| exts.contains(&ext))
        .map_or(FileKind::Other, |(_, kind)| *kind)
}

/// Per returned path: its kind, whether any of its text is indexed, and where the query terms
/// matched (`name_path` = file name or folder, `text` = indexed text). `paths` are relative to
/// `root` and are scored in the order of `payload["paths"]`.
pub fn profile_results(root: &Path, payload: &Value, query: &str) -> Result<Vec<Value>> {
    let Some(paths) = payload.get("paths").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let terms_by_path = matched_terms(payload);
    let words = query_words(query);
    let index_path = database_path()?;
    let con = open_database()?;
    crate::sqlite_index::initialize(&con, &index_path)?;
    let Some(id) = root_id(&con, root)? else {
        return Ok(Vec::new());
    };
    // The body is read only for the results that get a snippet.
    let mut statement = con
        .prepare(
            "SELECT ext, COALESCE(body_size, 0), CASE WHEN ?3 THEN body_blob END \
             FROM search_docs WHERE root_id=?1 AND path=?2",
        )
        .map_err(|source| sqlite_error(&index_path, source))?;
    let mut out = Vec::with_capacity(paths.len());
    for (rank, path) in paths.iter().filter_map(Value::as_str).enumerate() {
        let with_snippet = rank < SNIPPET_RESULTS;
        let row = statement
            .query_row(params![id, path, with_snippet], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .ok();
        let (ext, body, blob) = row.unwrap_or_default();
        let kind = file_kind(&ext);
        let compact_path = compact_lookup_text(path);
        let mut matched = Vec::new();
        for term in terms_by_path.get(path).into_iter().flatten() {
            let compact = compact_lookup_text(term);
            if compact.is_empty() {
                continue;
            }
            let place = if compact_path.contains(&compact) {
                "name_path"
            } else {
                "text"
            };
            if !matched.contains(&place) {
                matched.push(place);
            }
        }
        let body_text = blob
            .zip(usize::try_from(body).ok())
            .and_then(|(blob, size)| {
                jikji_core::compression::decompress_body_text(&blob, size).ok()
            })
            .map(|text| Folded::new(&text));
        let found = matched_words(&words, path, body_text.as_ref());
        let snippet = body_text.as_ref().and_then(|text| {
            // The search reports only a few matched terms: the request's own words help too.
            let mut terms = terms_by_path.get(path).cloned().unwrap_or_default();
            terms.extend(words.iter().cloned());
            snippet_around(text, &terms)
        });
        out.push(json!({
            "path": path,
            "file_kind": kind.as_str(),
            "text_indexed": body > 0,
            "matched_in": matched,
            "matched_words": found,
            "snippet": snippet,
        }));
    }
    Ok(out)
}

/// The significant words of a request, one per whitespace-separated word, lowercased; words the
/// engine ignores (too short, stop words) are left out.
pub fn query_words(query: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for chunk in query.split_whitespace() {
        if let Some(word) = tokens(chunk, 64).into_iter().next()
            && !words.contains(&word)
        {
            words.push(word);
        }
    }
    words
}

/// Results that carry a snippet; later results are file-level only.
const SNIPPET_RESULTS: usize = 20;
const SNIPPET_CHARS: usize = 200;

/// A text prepared for matching: composed (NFC) characters and their lowercase fold, index for
/// index, so a match position in one is the position in the other.
struct Folded {
    chars: Vec<char>,
    folded: Vec<char>,
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn fold_text(text: &str) -> Vec<char> {
    text.nfc().map(fold).collect()
}

impl Folded {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.nfc().collect();
        let folded = chars.iter().map(|&c| fold(c)).collect();
        Self { chars, folded }
    }

    fn find(&self, needle: &[char]) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        self.folded
            .windows(needle.len())
            .position(|window| window == needle)
    }
}

/// Which of `words` a result contains, in its path or its text. A word counts when it or its
/// stem (a Korean word without its particle) occurs; case and Unicode composition are ignored.
fn matched_words(words: &[String], path: &str, body: Option<&Folded>) -> Vec<String> {
    let compact_path = compact_lookup_text(path);
    words
        .iter()
        .filter(|word| {
            term_variants(word).iter().any(|variant| {
                let compact = compact_lookup_text(variant);
                (!compact.is_empty() && compact_path.contains(&compact))
                    || body.is_some_and(|text| text.find(&fold_text(variant)).is_some())
            })
        })
        .cloned()
        .collect()
}

/// A passage of the text around the first place one of `terms` occurs (longest term first), with
/// whitespace collapsed and `...` marking cut ends. `None` when no term occurs in the text, e.g.
/// the file matched by name only.
fn snippet_around(text: &Folded, terms: &[String]) -> Option<String> {
    let mut ordered: Vec<Vec<char>> = terms
        .iter()
        .map(|term| fold_text(term))
        .filter(|term| term.len() >= 2)
        .collect();
    ordered.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let (start, len) = ordered
        .iter()
        .find_map(|needle| text.find(needle).map(|at| (at, needle.len())))?;
    let hay = &text.chars;
    let half = SNIPPET_CHARS.saturating_sub(len) / 2;
    let mut from = start.saturating_sub(half);
    let mut to = (start + len + half).min(hay.len());
    // Start and end on a word boundary when one is close, so a word is not cut in half.
    while from > 0 && from < start && !hay[from - 1].is_whitespace() && start - from > 1 {
        from += 1;
    }
    while to < hay.len() && to > start + len && !hay[to].is_whitespace() {
        to -= 1;
    }
    let passage: String = hay[from..to].iter().collect();
    let passage = passage.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!(
        "{}{}{}",
        if from > 0 { "... " } else { "" },
        passage,
        if to < hay.len() { " ..." } else { "" }
    ))
}

fn matched_terms(payload: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for candidate in payload
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = candidate.get("p").and_then(Value::as_str) else {
            continue;
        };
        let terms = candidate
            .get("terms")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        out.insert(path.to_owned(), terms);
    }
    out
}

/// How much of `root` (or of its folder `scope`) can be searched by text: files by kind, and how
/// many of them have indexed text. `by_kind` lists only kinds that occur.
pub fn coverage(root: &Path, scope: Option<&str>) -> Result<Value> {
    let index_path = database_path()?;
    let con = open_database()?;
    crate::sqlite_index::initialize(&con, &index_path)?;
    let Some(id) = root_id(&con, root)? else {
        return Ok(json!({"indexed_files": 0, "by_kind": {}}));
    };
    let like = scope
        .filter(|scope| !scope.is_empty())
        .map_or_else(|| "%".to_owned(), like_pattern);
    let mut statement = con
        .prepare(
            "SELECT ext, COUNT(*), SUM(COALESCE(body_size, 0) > 0) FROM search_docs \
             WHERE root_id=?1 AND path LIKE ?2 ESCAPE '\\' GROUP BY ext",
        )
        .map_err(|source| sqlite_error(&index_path, source))?;
    let rows = statement
        .query_map(params![id, like], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|source| sqlite_error(&index_path, source))?;
    let mut kinds: BTreeMap<FileKind, (i64, i64)> = BTreeMap::new();
    let mut total = 0;
    for row in rows {
        let (ext, files, text) = row.map_err(|source| sqlite_error(&index_path, source))?;
        let entry = kinds.entry(file_kind(&ext)).or_default();
        entry.0 += files;
        entry.1 += text;
        total += files;
    }
    let name_only: i64 = kinds.values().map(|(files, text)| files - text).sum();
    let by_kind: serde_json::Map<String, Value> = kinds
        .iter()
        .map(|(kind, (files, text))| {
            (
                kind.as_str().to_owned(),
                json!({"files": files, "text_indexed": text}),
            )
        })
        .collect();
    Ok(json!({
        "indexed_files": total,
        "name_only_files": name_only,
        "by_kind": by_kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::{FileKind, Folded, file_kind, matched_words, snippet_around};

    fn terms(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_snippet_is_cut_around_the_match_deep_in_a_long_text() {
        let filler = "alpha beta gamma ".repeat(400);
        let text = format!("{filler}the Zyxwvut clause requires notice. {filler}");
        let snippet = snippet_around(&Folded::new(&text), &terms(&["zyxwvut"])).unwrap();
        assert!(
            snippet.contains("Zyxwvut clause requires notice"),
            "{snippet}"
        );
        assert!(snippet.starts_with("... ") && snippet.ends_with(" ..."));
        assert!(snippet.chars().count() < 260, "{}", snippet.chars().count());
    }

    #[test]
    fn the_longest_term_that_occurs_picks_the_passage_and_korean_matches() {
        let text = "서론 내용입니다. 임대차 계약서 갱신 조항은 두 달 전에 통지해야 한다. 끝.";
        let snippet =
            snippet_around(&Folded::new(text), &terms(&["계약", "계약서", "없는말"])).unwrap();
        assert!(snippet.contains("임대차 계약서 갱신"), "{snippet}");
        assert_eq!(
            snippet_around(&Folded::new(text), &terms(&["존재안함"])),
            None
        );
        assert_eq!(snippet_around(&Folded::new(text), &terms(&[])), None);
    }

    #[test]
    fn matched_words_look_in_the_path_and_the_text_and_ignore_particles() {
        let words: Vec<String> = ["계약서를", "kelvorin", "ghostword"]
            .iter()
            .map(|word| (*word).to_owned())
            .collect();
        let body = Folded::new("임대차 계약서 본문 Kelvorin 조항");
        let found = matched_words(&words, "docs/other.txt", Some(&body));
        assert_eq!(found, ["계약서를", "kelvorin"]);
        let by_path = matched_words(&words, "ghostword/계약서.txt", None);
        assert_eq!(by_path, ["계약서를", "ghostword"]);
    }

    #[test]
    fn decomposed_text_and_a_composed_query_still_match() {
        use unicode_normalization::UnicodeNormalization;
        let text: String = "회의 결과 예산 확정".nfd().collect();
        assert!(
            snippet_around(&Folded::new(&text), &terms(&["예산"]))
                .unwrap()
                .contains("예산")
        );
    }

    #[test]
    fn extensions_map_to_kinds_regardless_of_dot_and_case() {
        assert_eq!(file_kind(".JPG"), FileKind::Image);
        assert_eq!(file_kind("mp3"), FileKind::Audio);
        assert_eq!(file_kind(".mkv"), FileKind::Video);
        assert_eq!(file_kind("hwpx"), FileKind::Document);
        assert_eq!(file_kind(".rs"), FileKind::Code);
        assert_eq!(file_kind("md"), FileKind::Text);
        assert_eq!(file_kind(".zip"), FileKind::Archive);
        assert_eq!(file_kind(""), FileKind::Other);
        assert_eq!(file_kind(".weird"), FileKind::Other);
        assert!(FileKind::Video.is_media() && !FileKind::Code.is_media());
    }
}
