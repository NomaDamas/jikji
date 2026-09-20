use std::collections::BTreeSet;
use std::path::Path;

use crate::stopwords::STOP_TERMS;

const KOREAN_PARTICLE_SUFFIXES: &[&str] = &[
    "이라고",
    "라고",
    "으로",
    "에서",
    "에게",
    "까지",
    "부터",
    "처럼",
    "보다",
    "이나",
    "나",
    "과",
    "와",
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "의",
    "도",
    "만",
    "로",
];

pub(crate) fn tokens(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut start = None;
    for (idx, ch) in chars.iter().enumerate() {
        if is_token_continue(*ch) {
            start.get_or_insert(idx);
        } else if let Some(from) = start.take() {
            push_token(&chars[from..idx], &mut seen, &mut out, limit);
        }
        if out.len() >= limit {
            return out;
        }
    }
    if let Some(from) = start {
        push_token(&chars[from..], &mut seen, &mut out, limit);
    }
    out
}

pub(crate) fn term_variants(term: &str) -> BTreeSet<String> {
    let folded = term.trim().to_lowercase();
    let mut variants = BTreeSet::new();
    if folded.is_empty() {
        return variants;
    }
    variants.insert(folded.clone());
    for suffix in KOREAN_PARTICLE_SUFFIXES {
        if folded.ends_with(suffix) && folded.chars().count() > suffix.chars().count() + 1 {
            let stem_len = folded.len() - suffix.len();
            variants.insert(folded[..stem_len].to_owned());
            break;
        }
    }
    let compact = compact_lookup_text(&folded);
    if compact != folded && compact.chars().count() >= 2 {
        variants.insert(compact);
    }
    variants
}

pub(crate) fn cjk_ngrams(text: &str, limit: usize) -> Vec<String> {
    let compact = compact_lookup_text(text);
    if compact.chars().count() < 2 || !compact.chars().any(is_cjk) {
        return Vec::new();
    }
    let chars = compact.chars().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for n in [4usize, 3, 2] {
        if chars.len() < n {
            continue;
        }
        for idx in 0..=(chars.len() - n) {
            let gram = chars[idx..idx + n].iter().collect::<String>();
            if seen.insert(gram.clone()) {
                out.push(gram);
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

pub(crate) fn filename_lookup_keys(path_or_name: &str) -> Vec<String> {
    let raw = path_or_name.trim();
    let name = Path::new(raw)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(raw);
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let mut keys = BTreeSet::new();
    for value in [raw, name, stem, duplicate_stem(name).as_str()] {
        let compact = compact_lookup_text(value);
        if !compact.is_empty() {
            keys.insert(compact);
        }
    }
    keys.into_iter().collect()
}

pub(crate) fn query_terms(query: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in tokens(query, 64) {
        for variant in term_variants(&token) {
            out.insert(variant.clone());
            for gram in cjk_ngrams(&variant, 128) {
                out.insert(gram);
            }
        }
        for key in filename_lookup_keys(&token) {
            out.insert(key);
        }
    }
    for quoted in quoted_terms(query) {
        out.insert(quoted);
    }
    out
}

pub(crate) fn quoted_terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();
    for ch in query.chars() {
        if ch == '"' {
            if in_quote {
                let compact = compact_lookup_text(&current);
                if compact.chars().count() >= 2 {
                    out.push(compact);
                }
                current.clear();
            }
            in_quote = !in_quote;
        } else if in_quote {
            current.push(ch);
        }
    }
    out
}

pub(crate) fn compact_lookup_text(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || is_cjk(*ch))
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn token_counts(text: &str, limit: usize) -> BTreeSet<(String, usize)> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for token in tokens(text, limit) {
        for variant in term_variants(&token) {
            *counts.entry(variant.clone()).or_insert(0) += 1;
            for gram in cjk_ngrams(&variant, 128) {
                *counts.entry(gram).or_insert(0) += 1;
            }
        }
    }
    counts.into_iter().collect()
}

fn push_token(chars: &[char], seen: &mut BTreeSet<String>, out: &mut Vec<String>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    let token = chars
        .iter()
        .collect::<String>()
        .trim_matches(['.', '_', '+', '-'])
        .to_lowercase();
    if token.chars().count() < 2 || STOP_TERMS.contains(&token.as_str()) {
        return;
    }
    if seen.insert(token.clone()) {
        out.push(token.clone());
    }
    if token.chars().any(|ch| matches!(ch, '.' | '_' | '-' | '+')) {
        for part in token.split(['.', '_', '-', '+']) {
            if part.chars().count() >= 2 && seen.insert(part.to_owned()) {
                out.push(part.to_owned());
                if out.len() >= limit {
                    return;
                }
            }
        }
    }
    if token.chars().any(is_cjk) {
        for gram in cjk_ngrams(&token, limit.saturating_mul(4).max(64)) {
            if seen.insert(gram.clone()) {
                out.push(gram);
                if out.len() >= limit {
                    return;
                }
            }
        }
    }
}

fn duplicate_stem(name: &str) -> String {
    let mut stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name)
        .to_lowercase();
    loop {
        let trimmed = stem
            .trim_end_matches("_copy")
            .trim_end_matches(" copy")
            .trim_end_matches(" - copy")
            .trim()
            .to_owned();
        if trimmed == stem {
            return stem;
        }
        stem = trimmed;
    }
}

pub(crate) fn is_token_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || is_cjk(ch) || matches!(ch, '_' | '.' | '+' | '-')
}

pub(crate) fn is_cjk(ch: char) -> bool {
    ('가'..='힣').contains(&ch)
        || ('ぁ'..='ゟ').contains(&ch)
        || ('゠'..='ヿ').contains(&ch)
        || ('一'..='鿿').contains(&ch)
}

#[cfg(test)]
mod tests {
    use super::query_terms;

    #[test]
    fn query_terms_keeps_document_extensions() {
        let hwp = query_terms("hwp");
        assert!(hwp.contains("hwp"), "{hwp:?}");
        assert!(query_terms("hwpx").contains("hwpx"));
        assert!(query_terms(".HWP").contains("hwp"));
        assert!(query_terms("pdf").contains("pdf"));
    }

    #[test]
    fn query_terms_keeps_extension_in_mixed_korean_query() {
        let terms = query_terms("범정부 hwp");
        assert!(terms.contains("hwp"), "{terms:?}");
        assert!(
            terms.contains("범정부") || terms.iter().any(|term| term.contains("범정")),
            "{terms:?}"
        );
    }

    #[test]
    fn query_terms_still_drops_linguistic_stopwords() {
        assert!(query_terms("파일").is_empty());
        assert!(query_terms("찾아줘").is_empty());
        let terms = query_terms("hwp 파일");
        assert!(terms.contains("hwp"), "{terms:?}");
        assert!(!terms.contains("파일"), "{terms:?}");
    }
}
