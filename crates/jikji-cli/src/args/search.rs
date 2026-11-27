use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub(crate) struct FindArgs {
    pub(crate) root: PathBuf,
    pub(crate) query: String,
    #[arg(long, default_value_t = 20)]
    pub(crate) top_k: usize,
    #[arg(long)]
    pub(crate) first: bool,
    /// With --json, drop the internal ranking diagnostics (strategy_results, llm_judge_input,
    /// judge_candidate_slate, ...) and keep what an agent acts on: paths, results (kind, matched
    /// words, snippet), request_plan, notes, index status and the retry contract. About a third
    /// of the default size.
    #[arg(long)]
    pub(crate) brief: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) fresh: bool,
    #[arg(long, default_value_t = false)]
    pub(crate) auto_prepare: bool,
    #[arg(long = "no-auto-prepare", default_value_t = false)]
    pub(crate) no_auto_prepare: bool,
    #[arg(long, default_value_t = 24 * 60 * 60, allow_hyphen_values = true)]
    pub(crate) stale_after_seconds: i64,
    #[arg(long)]
    pub(crate) no_background_refresh: bool,
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
    #[arg(long)]
    pub(crate) after_jikji_retry: bool,
    #[arg(long, default_value = "")]
    pub(crate) retry_proof: String,
}

#[derive(Debug, Parser)]
pub(crate) struct SearchArgs {
    pub(crate) root: PathBuf,
    pub(crate) query: String,
    #[arg(long, default_value_t = 20)]
    pub(crate) top_k: usize,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) fresh: bool,
    #[arg(long, default_value_t = false)]
    pub(crate) auto_prepare: bool,
    #[arg(long = "no-auto-prepare", default_value_t = false)]
    pub(crate) no_auto_prepare: bool,
    #[arg(long, default_value_t = 24 * 60 * 60, allow_hyphen_values = true)]
    pub(crate) stale_after_seconds: i64,
    #[arg(long)]
    pub(crate) no_background_refresh: bool,
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
}

#[derive(Debug, Parser)]
pub(crate) struct BriefArgs {
    pub(crate) root: PathBuf,
    pub(crate) query: String,
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: usize,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) compact: bool,
    #[arg(long)]
    pub(crate) fresh: bool,
    #[arg(long, default_value_t = false)]
    pub(crate) auto_prepare: bool,
    #[arg(long = "no-auto-prepare", default_value_t = false)]
    pub(crate) no_auto_prepare: bool,
    #[arg(long, default_value_t = 24 * 60 * 60, allow_hyphen_values = true)]
    pub(crate) stale_after_seconds: i64,
    #[arg(long)]
    pub(crate) no_background_refresh: bool,
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
}

#[derive(Debug, Parser)]
pub(crate) struct GraphArgs {
    pub(crate) root: PathBuf,
    #[command(subcommand)]
    pub(crate) command: GraphCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GraphCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
    Query {
        query: String,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        #[arg(long)]
        json: bool,
    },
    Explain {
        source_path: String,
        #[arg(long)]
        json: bool,
    },
}
