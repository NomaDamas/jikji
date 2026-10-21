mod agent;
mod bench;
mod gui;
mod prepare;
mod search;

use std::path::PathBuf;

pub(crate) use agent::{AgentSkillArgs, PostInstallPrepareArgs, SkillAliasArgs, SkillExportArgs};
pub(crate) use bench::{
    BenchAnalyzeArgs, BenchIterateArgs, BenchRunArgs, BenchmarkValueReportArgs, EvalGenerateArgs,
    EvalRunArgs, HermesBenchArgs, HermesCompareArgs, ImportArgs, PublicImportArgs, PublicSuiteArgs,
};
use clap::{Parser, Subcommand};
pub(crate) use gui::GuiArgs;
pub(crate) use prepare::{CleanArgs, MapArgs, PrepareArgs};
pub(crate) use search::{BriefArgs, FindArgs, GraphArgs, GraphCommand, SearchArgs};

#[derive(Debug, Parser)]
#[command(
    name = "jikji",
    version,
    about = "Non-destructive local file knowledge maps for AI agents.",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    #[command(about = "Create or update .jikji artifacts for a folder.")]
    Prepare(PrepareArgs),
    #[command(about = "Deeply index archive members and media text for a folder.")]
    DeepIndex(PrepareArgs),
    #[command(about = "Alias for prepare.")]
    Refresh(PrepareArgs),
    #[command(about = "Remove only Jikji-owned generated artifacts.")]
    Clean(CleanArgs),
    #[command(about = "Print the generated agent map for a folder.")]
    Map(MapArgs),
    #[command(about = "Verify expected generated artifacts for a folder.")]
    Doctor {
        root: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "Query an existing Jikji index for bounded local discovery when the target path or corpus content is unknown.",
        long_about = "Query an existing Jikji index for local file, folder, metadata, or document-content discovery only when the target path is unknown and an explicit or inferred bounded root exists. Not for general QA, explanations, coding advice, web search, known-path reads or edits, stats, process/git/build/test/package commands, directory listings, or LSP symbol lookup."
    )]
    Find(FindArgs),
    #[command(about = "Rank files from an existing Jikji index.")]
    Search(SearchArgs),
    #[command(about = "Emit an agent search brief.")]
    Brief(BriefArgs),
    #[command(about = "Emit the discovery answer-pack contract.")]
    Discover(FindArgs),
    #[command(about = "Query graph/wiki route artifacts.")]
    Graph(GraphArgs),
    #[command(about = "Serve a local loopback web UI.")]
    Gui(GuiArgs),
    #[command(about = "Install the Jikji auto-use skill for local agents.")]
    AgentSkillInstall(AgentSkillArgs),
    #[command(about = "Install the Jikji skill into ~/.hermes/skills.")]
    HermesSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for Codex.")]
    CodexSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for OMX.")]
    OmxSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for Claude.")]
    ClaudeSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for OpenCode.")]
    OpencodeSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for OpenClo.")]
    OpencloSkillInstall(SkillAliasArgs),
    #[command(about = "Install the Jikji skill for NanoClo.")]
    NanocloSkillInstall(SkillAliasArgs),
    #[command(about = "Print or write the universal Jikji SKILL.md.")]
    SkillExport(SkillExportArgs),
    #[command(about = "Generate local search evaluation cases.")]
    EvalGenerate(EvalGenerateArgs),
    #[command(about = "Generate curated realistic evaluation cases.")]
    EvalGenerateRealistic(EvalGenerateArgs),
    #[command(about = "Generate locked holdout evaluation cases.")]
    EvalGenerateHoldout(EvalGenerateArgs),
    #[command(about = "Evaluate local search quality.")]
    Eval(EvalRunArgs),
    #[command(about = "Analyze benchmark failures and answerability.")]
    BenchAnalyze(BenchAnalyzeArgs),
    #[command(about = "Import a bounded HippoCamp eval set.")]
    HippocampImport(ImportArgs),
    #[command(about = "Compare raw filesystem search with Jikji-assisted search.")]
    BenchRun(BenchRunArgs),
    #[command(about = "Repeat a deterministic benchmark.")]
    BenchIterate(BenchIterateArgs),
    #[command(about = "Download/materialize one BEIR dataset as local files.")]
    BeirImport(PublicImportArgs),
    #[command(about = "Download/materialize a bounded HippoCamp subset as local files.")]
    HippocampFetch(PublicImportArgs),
    #[command(about = "Run public BEIR local-file retrieval suite.")]
    BeirSuite(PublicSuiteArgs),
    #[command(about = "Inspect public EDiTh benchmark metadata.")]
    EdithSummary(PublicImportArgs),
    #[command(about = "Materialize a bounded EDiTh benchmark.")]
    EdithImport(PublicImportArgs),
    #[command(about = "Run bounded public EDiTh diagnostics.")]
    EdithSuite(PublicSuiteArgs),
    #[command(about = "Build a Korean public-data local-agent corpus.")]
    PublicdataBuild(PublicImportArgs),
    #[command(about = "Build and run public-data diagnostics.")]
    PublicdataSuite(PublicSuiteArgs),
    #[command(about = "Build a bounded Workspace-Bench-Lite corpus.")]
    WorkspacebenchBuild(PublicImportArgs),
    #[command(about = "Build and run Workspace-Bench-Lite diagnostics.")]
    WorkspacebenchSuite(PublicSuiteArgs),
    #[command(about = "Build a hard mixed-document benchmark corpus.")]
    HardbenchBuild(PublicImportArgs),
    #[command(about = "Build and run hard mixed-document diagnostics.")]
    HardbenchSuite(PublicSuiteArgs),
    #[command(about = "Run a bounded multi-profile HippoCamp suite.")]
    HippocampSuite(PublicSuiteArgs),
    #[command(hide = true, about = "Run queued post-install prepares.")]
    PostInstallPrepare(PostInstallPrepareArgs),
    #[command(about = "Run Hermes retrieval benchmarks with the native Rust runner.")]
    HermesBench(HermesBenchArgs),
    #[command(about = "Compare saved Hermes benchmark reports.")]
    HermesCompare(HermesCompareArgs),
    #[command(about = "Build benchmark value and cost reports.")]
    BenchmarkValueReport(BenchmarkValueReportArgs),
}
