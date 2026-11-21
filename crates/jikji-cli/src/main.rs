#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use jikji_index::{doctor, read_map};

use crate::args::{Cli, Command};
use crate::bench_commands::{
    run_bench_analyze, run_bench_iterate, run_bench_run, run_benchmark_value_report, run_eval,
    run_eval_generate, run_hermes_bench, run_hermes_compare, run_hippocamp_import,
    run_public_import, run_public_suite,
};
use crate::output::print_json;
use crate::prepare_commands::{run_clean, run_deep_index, run_prepare};
use crate::search_commands::{run_brief, run_discover, run_find, run_search};
use crate::search_graph::run_graph;

mod agent_commands;
mod args;
mod bench_commands;
mod gui_commands;
mod output;
mod post_install_background;
mod post_install_commands;
mod prepare_commands;
mod query_shape;
mod request_plan;
mod search_commands;
mod search_graph;
mod search_prepare_options;
mod search_refresh;

fn main() -> ExitCode {
    let _background_refresh_guard = search_refresh::BackgroundRefreshGuard::from_env();
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> jikji_core::Result<ExitCode> {
    match cli.command {
        Command::Prepare(args) | Command::Refresh(args) => run_prepare(args),
        Command::DeepIndex(args) => run_deep_index(args),
        Command::Clean(args) => run_clean(args),
        Command::Map(args) => {
            print!("{}", truncate_chars(&read_map(&args.root)?, args.max_chars));
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor { root, json } => run_doctor(&root, json),
        Command::Find(args) => run_find(args),
        Command::Discover(args) => run_discover(args),
        Command::Search(args) => run_search(args),
        Command::Brief(args) => run_brief(args),
        Command::Graph(args) => run_graph(args),
        Command::Gui(args) => gui_commands::run_gui(args),
        Command::AgentSkillInstall(args) => agent_commands::run_agent_skill_install(args),
        Command::HermesSkillInstall(args) => agent_commands::run_skill_alias("hermes", args),
        Command::CodexSkillInstall(args) => agent_commands::run_skill_alias("codex", args),
        Command::OmxSkillInstall(args) => agent_commands::run_skill_alias("omx", args),
        Command::ClaudeSkillInstall(args) => agent_commands::run_skill_alias("claude", args),
        Command::OpencodeSkillInstall(args) => agent_commands::run_skill_alias("opencode", args),
        Command::OpencloSkillInstall(args) => agent_commands::run_skill_alias("openclo", args),
        Command::NanocloSkillInstall(args) => agent_commands::run_skill_alias("nanoclo", args),
        Command::SkillExport(args) => agent_commands::run_skill_export(args),
        Command::EvalGenerate(args)
        | Command::EvalGenerateRealistic(args)
        | Command::EvalGenerateHoldout(args) => run_eval_generate(args),
        Command::Eval(args) => run_eval(args),
        Command::BenchAnalyze(args) => run_bench_analyze(args),
        Command::HippocampImport(args) => run_hippocamp_import(args),
        Command::BenchRun(args) => run_bench_run(args),
        Command::BenchIterate(args) => run_bench_iterate(args),
        Command::BeirImport(args) => run_public_import("beir", args),
        Command::EdithSummary(args) | Command::EdithImport(args) => {
            run_public_import("edith", args)
        }
        Command::PublicdataBuild(args) => run_public_import("publicdata", args),
        Command::WorkspacebenchBuild(args) => run_public_import("workspacebench", args),
        Command::HardbenchBuild(args) => run_public_import("hardbench", args),
        Command::HippocampFetch(args) => run_public_import("hippocamp", args),
        Command::BeirSuite(args) => run_public_suite("beir", args),
        Command::EdithSuite(args) => run_public_suite("edith", args),
        Command::PublicdataSuite(args) => run_public_suite("publicdata", args),
        Command::WorkspacebenchSuite(args) => run_public_suite("workspacebench", args),
        Command::HardbenchSuite(args) => run_public_suite("hardbench", args),
        Command::HippocampSuite(args) => run_public_suite("hippocamp", args),
        Command::PostInstallPrepare(args) => post_install_commands::run_post_install_prepare(args),
        Command::HermesBench(args) => run_hermes_bench(args),
        Command::HermesCompare(args) => run_hermes_compare(args),
        Command::BenchmarkValueReport(args) => run_benchmark_value_report(args),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
}

fn run_doctor(root: &std::path::Path, json: bool) -> jikji_core::Result<ExitCode> {
    let report = doctor(root)?;
    if json {
        print_json(&report)?;
    } else if report.ok {
        println!("Jikji doctor OK: {}", report.root.display());
    } else {
        println!("Jikji doctor found errors: {:?}", report.errors);
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
