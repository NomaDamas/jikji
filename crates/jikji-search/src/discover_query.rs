use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::tokenizer::is_token_continue;

pub(crate) fn strip_shell_noise(query: &str) -> String {
    let noise = shell_noise();
    query
        .replace(['$', '`'], " ")
        .split_whitespace()
        .filter_map(|raw| {
            let token = raw.trim_matches(|ch: char| ".,:;!?()[]{}\"'".contains(ch));
            let folded = token.trim_start_matches('-').to_lowercase();
            if token.is_empty()
                || noise.contains(folded.as_str())
                || (token.starts_with('-') && folded.len() <= 2)
                || token.chars().all(|ch| matches!(ch, '/' | '.'))
                || token.chars().any(|ch| "$`;&|<>\\\"".contains(ch))
            {
                None
            } else {
                Some(token.to_owned())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn strategy_variants(query: &str) -> Vec<(String, String)> {
    let mut variants = vec![("lexical".to_owned(), query.to_owned())];
    let anchors = anchor_tokens(query).join(" ");
    if !anchors.is_empty() {
        variants.push(("lexical_anchors".to_owned(), anchors));
    }
    let semantic = semantic_expansion(query);
    if semantic != query && !semantic.is_empty() {
        variants.push(("semantic".to_owned(), semantic));
    }
    let advanced = advanced_query(query);
    if advanced != query && !advanced.is_empty() {
        variants.push(("advanced".to_owned(), advanced));
    }
    let mut seen = BTreeSet::new();
    variants
        .into_iter()
        .filter(|(_, value)| seen.insert(value.to_lowercase()))
        .take(6)
        .collect()
}

fn semantic_expansion(query: &str) -> String {
    let mut terms = query
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let lower = query.to_lowercase();
    let expansions = [
        ("contract", "agreement terms legal"),
        ("agreement", "contract terms legal"),
        ("renewal", "extension continuation"),
        ("invoice", "bill payment receipt"),
        ("meeting", "minutes notes agenda"),
        ("photo", "image picture"),
        ("audio", "recording transcript"),
        ("video", "recording transcript"),
        ("계약", "협약 조항"),
        ("갱신", "연장 재계약"),
        ("회의", "회의록 안건 메모"),
        ("한글", "hwp hwpx"),
        ("발표자료", "pptx ppt 슬라이드"),
        ("워드", "docx"),
        ("제안서", "제안 rfp"),
        ("보고서", "리포트 report"),
    ];
    for (needle, expansion) in expansions {
        if lower.contains(needle) {
            terms.extend(expansion.split_whitespace().map(str::to_owned));
        }
    }
    terms.join(" ")
}

fn advanced_query(query: &str) -> String {
    let noise = shell_noise();
    query
        .split_whitespace()
        .filter(|raw| {
            let token = raw.trim_matches(|ch: char| ".,:;!?()[]{}\"'".contains(ch));
            !token.is_empty() && !noise.contains(token.to_lowercase().as_str()) && token.len() > 2
        })
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn classify_query(query: &str) -> String {
    let folded = query.to_lowercase();
    if [
        "habit",
        "usual",
        "summarize",
        "summary",
        "records",
        "versions",
    ]
    .iter()
    .any(|hint| folded.contains(hint))
    {
        "evidence_set".to_owned()
    } else if [
        "which",
        "what file",
        "find the",
        "locate",
        "contract",
        "agreement",
        "nda",
        "pdf",
        "document",
        "file",
        "한글",
        "계약",
        "제안서",
        "보고서",
        "발표자료",
        "워드",
        "공문",
        "사용매뉴얼",
    ]
    .iter()
    .any(|hint| folded.contains(hint))
    {
        "single_file".to_owned()
    } else {
        "adaptive".to_owned()
    }
}

pub(crate) fn retry_proof_for(root: &Path, query: &str, top_k: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.display().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(query.as_bytes());
    hasher.update(b"\0");
    hasher.update(top_k.to_string().as_bytes());
    hasher.update(b"\0jikji-retry-v1");
    format!("{:x}", hasher.finalize())
        .chars()
        .take(24)
        .collect()
}

pub(crate) fn anchor_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !is_token_continue(ch))
        .filter(|token| token.chars().count() >= 2)
        .flat_map(|token| {
            let mut out = vec![token.to_lowercase()];
            if let Some(year) = token
                .strip_prefix("FY")
                .or_else(|| token.strip_prefix("fy"))
            {
                if year.len() == 2 && year.chars().all(|ch| ch.is_ascii_digit()) {
                    out.push(format!("20{year}"));
                    out.push(year.to_owned());
                }
            }
            out
        })
        .filter(|token| !generic_anchor(token))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn generic_anchor(token: &str) -> bool {
    let lower = token.to_lowercase();
    if matches!(
        lower.as_str(),
        "한글"
            | "문서"
            | "파일"
            | "폴더"
            | "자료"
            | "소스"
            | "관련"
            | "내용"
            | "워드"
            | "텍스트"
            | "발표자료"
            | "markdown"
            | "python"
            | "rust"
    ) {
        return true;
    }
    matches!(
        token.to_ascii_uppercase().as_str(),
        "CEO"
            | "CFO"
            | "COO"
            | "CTO"
            | "DOC"
            | "DOCX"
            | "HWP"
            | "HWPX"
            | "INC"
            | "LLC"
            | "NDA"
            | "PDF"
            | "PPT"
            | "PPTX"
            | "TXT"
            | "XLS"
            | "XLSX"
    )
}

fn shell_noise() -> BTreeSet<&'static str> {
    [
        "bash", "cat", "chmod", "curl", "echo", "find", "grep", "ls", "rm", "rf", "rmdir", "sed",
        "sh", "sudo", "wget",
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::{anchor_tokens, classify_query, strategy_variants};

    #[test]
    fn anchor_tokens_keep_hangul_and_drop_type_words() {
        let anchors = anchor_tokens("정의서 발표자료");
        assert!(
            anchors.iter().any(|token| token == "정의서"),
            "expected hangul filename token, got {anchors:?}"
        );
        assert!(
            !anchors.iter().any(|token| token == "발표자료"),
            "type words should not be anchors: {anchors:?}"
        );
    }

    #[test]
    fn anchor_tokens_keep_snake_case_identifiers() {
        let anchors = anchor_tokens("isolate_data_dir");
        assert_eq!(anchors, vec!["isolate_data_dir".to_owned()]);
    }

    #[test]
    fn korean_document_query_is_single_file() {
        assert_eq!(classify_query("산업지원 제안서"), "single_file");
        assert_eq!(classify_query("한글 문서"), "single_file");
    }

    #[test]
    fn korean_query_gets_lexical_anchors_variant() {
        let variants = strategy_variants("근대역사자료번역요약모델 한글 문서");
        assert!(
            variants.iter().any(|(name, value)| {
                name == "lexical_anchors" && value.contains("근대역사자료번역요약모델")
            }),
            "expected lexical_anchors with hangul stem, got {variants:?}"
        );
    }
}
