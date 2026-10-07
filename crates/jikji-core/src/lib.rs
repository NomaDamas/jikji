#![forbid(unsafe_code)]

use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
pub mod storage;

pub const ROOT_AGENT_MAP: &str = ".jikji_agent_map.md";
pub const LEGACY_ROOT_AGENT_MAP: &str = "000_JIKJI_AGENT_MAP.md";
pub const JIKJI_DIR: &str = ".jikji";

pub const OWNED_GENERATED_PATHS: &[&str] = &[
    ROOT_AGENT_MAP,
    LEGACY_ROOT_AGENT_MAP,
    ".jikji/manifest.json",
    ".jikji/file_index.jsonl",
    ".jikji/folder_index.jsonl",
    ".jikji/document_index.jsonl",
    ".jikji/file_cards.jsonl",
    ".jikji/chunk_map.jsonl",
    ".jikji/search_index.sqlite",
    ".jikji/duplicate_map.jsonl",
    ".jikji/folder_profile.jsonl",
    ".jikji/corpus_profile.json",
    ".jikji/intent_taxonomy.json",
    ".jikji/autorag_manifest.json",
    ".jikji/knowledge_graph.json",
    ".jikji/graph_routes.jsonl",
    ".jikji/llm_wiki_schema.md",
    ".jikji/wiki/",
    ".jikji/wiki/sources/",
    ".jikji/parse_errors.jsonl",
    ".jikji/agent_map.md",
    ".jikji/agent_routes.md",
    ".jikji/agent_skill_context.md",
    ".jikji/human_guide.md",
    ".jikji/.lock",
    ".jikji/doc_text/",
    ".jikji/doc_meta/",
    ".jikji/eval/",
];

pub const RETIRED_GENERATED_PATHS: &[&str] = &[
    ".jikji/search_terms.json",
    ".jikji/search_terms.jsonl",
    ".jikji/folder_cards/",
    ".jikji/file_cards/",
];

pub const GENERATED_ARTIFACTS: &[&str] = OWNED_GENERATED_PATHS;

#[derive(Debug, Error)]
pub enum JikjiError {
    #[error("path is outside the selected Jikji root: {0}")]
    OutsideRoot(PathBuf),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("Jikji index is already being prepared: {0}")]
    Locked(PathBuf),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, JikjiError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    pub fn new(root: PathBuf) -> Self {
        Self(root)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrepareOptions {
    pub include_hidden: bool,
    pub include_sensitive: bool,
    pub max_files: Option<usize>,
    pub exclude_patterns: Vec<String>,
    pub max_hash_bytes: u64,
    pub parse_timeout_seconds: f64,
    pub doc_text_max_chars: usize,
    pub doc_text_chunk_chars: usize,
    pub enable_media_index: bool,
    pub media_index_max_mb: f64,
    #[serde(default)]
    pub deep_archive_index: bool,
    #[serde(default = "default_archive_max_entries")]
    pub archive_max_entries: usize,
    #[serde(default = "default_archive_max_entry_bytes")]
    pub archive_max_entry_bytes: u64,
    #[serde(default = "default_archive_max_total_bytes")]
    pub archive_max_total_bytes: u64,
    /// Do not write `.jikji_agent_map.md` into the prepared root.
    #[serde(default)]
    pub skip_root_map: bool,
}

fn default_archive_max_entries() -> usize {
    1000
}
fn default_archive_max_entry_bytes() -> u64 {
    16 * 1024 * 1024
}
fn default_archive_max_total_bytes() -> u64 {
    128 * 1024 * 1024
}

impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            include_hidden: false,
            include_sensitive: false,
            max_files: None,
            exclude_patterns: Vec::new(),
            max_hash_bytes: 512 * 1024 * 1024,
            parse_timeout_seconds: 5.0,
            doc_text_max_chars: 2_000_000,
            doc_text_chunk_chars: 1_000_000,
            enable_media_index: false,
            media_index_max_mb: 25.0,
            deep_archive_index: false,
            archive_max_entries: default_archive_max_entries(),
            archive_max_entry_bytes: default_archive_max_entry_bytes(),
            archive_max_total_bytes: default_archive_max_total_bytes(),
            skip_root_map: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedArtifact {
    pub relative_path: String,
    pub owned_by_jikji: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestStub {
    pub schema_version: u16,
    pub generated_artifacts: Vec<GeneratedArtifact>,
}

pub fn generated_artifact_manifest() -> ManifestStub {
    let generated_artifacts = GENERATED_ARTIFACTS
        .iter()
        .map(|relative_path| GeneratedArtifact {
            relative_path: (*relative_path).to_owned(),
            owned_by_jikji: true,
        })
        .collect();

    ManifestStub {
        schema_version: 1,
        generated_artifacts,
    }
}

/// NFC-normalize free text such as search queries so they compare equal to
/// indexed text regardless of how the caller's input method composed it.
pub fn nfc_text(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.nfc().collect()
}

/// macOS file systems hand back decomposed (NFD) names, while typed queries and
/// parsed document text are composed (NFC). APFS and HFS+ resolve either form,
/// so index path text in NFC there; other platforms keep names byte-exact.
pub fn path_text(text: &str) -> String {
    if cfg!(target_os = "macos") {
        nfc_text(text)
    } else {
        text.to_owned()
    }
}

pub fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> JikjiError {
    JikjiError::Io {
        path: path.into(),
        source,
    }
}

pub fn json_error(path: impl Into<PathBuf>, source: serde_json::Error) -> JikjiError {
    JikjiError::Json {
        path: path.into(),
        source,
    }
}

pub fn ensure_generated_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|source| io_error(path, source))?;
        }
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io_error(
                path,
                Error::new(
                    ErrorKind::AlreadyExists,
                    format!(
                        "generated artifact path is not a directory: {}",
                        path.display()
                    ),
                ),
            ));
        }
        Err(source) if source.kind() == ErrorKind::NotFound => {}
        Err(source) => return Err(io_error(path, source)),
    }
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

#[cfg(test)]
mod tests {
    use super::{
        GENERATED_ARTIFACTS, RETIRED_GENERATED_PATHS, ROOT_AGENT_MAP, generated_artifact_manifest,
    };

    #[test]
    fn generated_artifacts_include_root_agent_maps_when_building_manifest() {
        let manifest = generated_artifact_manifest();

        assert!(GENERATED_ARTIFACTS.contains(&ROOT_AGENT_MAP));
        assert!(
            manifest
                .generated_artifacts
                .iter()
                .any(|artifact| artifact.relative_path == ROOT_AGENT_MAP)
        );
    }

    #[test]
    fn generated_artifacts_include_python_index_contract_names() {
        assert!(GENERATED_ARTIFACTS.contains(&".jikji/file_index.jsonl"));
        assert!(GENERATED_ARTIFACTS.contains(&".jikji/document_index.jsonl"));
        assert!(RETIRED_GENERATED_PATHS.contains(&".jikji/search_terms.jsonl"));
    }
}
