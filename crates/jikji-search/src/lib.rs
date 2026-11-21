#![forbid(unsafe_code)]

mod answer_pack;
mod artifacts;
mod brief;
mod cache_text;
mod discover;
mod discover_contract;
mod discover_query;
mod graph;
mod graph_artifacts;
mod index_rows;
mod indexer;
mod io;
mod map_query;
mod map_rescore;
mod profile;
mod scope;
mod scoring;
mod searcher;
mod sqlite_index;
mod status;
mod stopwords;
mod tokenizer;

pub use artifacts::{SearchArtifactStats, build_search_artifacts};
pub use brief::{BriefOptions, brief_payload, compact_brief_payload};
pub use discover::{DiscoverOptions, discover};
pub use graph::{explain_source, graph_query, graph_status};
pub use profile::{FileKind, coverage, file_kind, profile_results, query_words};
pub use scope::{count_root_docs, count_scope_docs, rebase_payload_paths};
pub use searcher::{SearchCandidate, SearchOptions, search, search_in_scope};
pub use status::{
    IndexStatus, SearchIndexStatus, TreeCheck, search_index_status, search_index_status_with,
};

pub const SEARCH_INDEX_SCHEMA_VERSION: u8 = 3;
