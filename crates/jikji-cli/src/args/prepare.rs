use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
pub(crate) struct PrepareArgs {
    #[arg(default_value = ".")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) include_hidden: bool,
    #[arg(long)]
    pub(crate) include_sensitive: bool,
    #[arg(long)]
    pub(crate) max_files: Option<usize>,
    #[arg(long = "exclude")]
    pub(crate) exclude: Vec<String>,
    #[arg(long, default_value_t = 512 * 1024 * 1024)]
    pub(crate) max_hash_bytes: u64,
    #[arg(long, default_value_t = 5.0)]
    pub(crate) parse_timeout: f64,
    #[arg(long, default_value_t = 2_000_000)]
    pub(crate) doc_text_max_chars: usize,
    #[arg(long, default_value_t = 1_000_000)]
    pub(crate) doc_text_chunk_chars: usize,
    #[arg(long)]
    pub(crate) enable_media_index: bool,
    #[arg(long, default_value_t = 25.0)]
    pub(crate) media_index_max_mb: f64,
    #[arg(long, hide = true)]
    pub(crate) deep_archive_index: bool,
    #[arg(long, default_value_t = 1_000)]
    pub(crate) archive_max_entries: usize,
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    pub(crate) archive_max_entry_bytes: u64,
    #[arg(long, default_value_t = 128 * 1024 * 1024)]
    pub(crate) archive_max_total_bytes: u64,
    #[arg(long)]
    pub(crate) no_agent_rules: bool,
    /// Do not write `.jikji_agent_map.md` into ROOT (the map stays in the data dir).
    #[arg(long)]
    pub(crate) no_root_map: bool,
    #[arg(long, hide = true)]
    pub(crate) search_only: bool,
}

#[derive(Debug, Parser)]
pub(crate) struct CleanArgs {
    #[arg(default_value = ".")]
    pub(crate) root: PathBuf,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Parser)]
pub(crate) struct MapArgs {
    #[arg(default_value = ".")]
    pub(crate) root: PathBuf,
    #[arg(long, default_value_t = 12_000)]
    pub(crate) max_chars: usize,
}
