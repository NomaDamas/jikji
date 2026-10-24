#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use jikji_core::{Result, io_error};
use serde::Serialize;

mod routing;

pub use routing::{
    AGENT_RULE_FILES, AGENT_RULES_BEGIN, AGENT_RULES_END, remove_routing_block,
    remove_routing_blocks, write_routing_block, write_routing_blocks,
};

pub const SKILL_MARKDOWN: &str = include_str!("../assets/jikji/SKILL.md");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentSkillInstallResult {
    pub agent: String,
    pub path: PathBuf,
    pub installed: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRoute {
    pub command: String,
    pub description: String,
}

pub fn default_agent_routes() -> Vec<AgentRoute> {
    vec![
        AgentRoute {
            command: "jikji prepare".to_owned(),
            description: "Create or refresh non-destructive local knowledge maps.".to_owned(),
        },
        AgentRoute {
            command: "jikji find".to_owned(),
            description: "Query an existing Jikji index from a local agent.".to_owned(),
        },
    ]
}

pub fn normalize_agent_name(agent: &str) -> String {
    let normalized = agent.trim().to_lowercase().replace('_', "-");
    match normalized.as_str() {
        "claudecode" | "claude-code" => "claude".to_owned(),
        "open-code" => "opencode".to_owned(),
        "openclone" | "open-clone" | "open-clo" => "openclo".to_owned(),
        "nano-clone" | "nano-clo" => "nanoclo".to_owned(),
        "any" | "universal" => "custom".to_owned(),
        other => other.to_owned(),
    }
}

pub fn expand_agent_selection(agent: &str) -> Result<Vec<String>> {
    let normalized = normalize_agent_name(agent);
    if normalized == "all" {
        return Ok([
            "hermes", "codex", "omx", "claude", "opencode", "openclo", "nanoclo", "generic",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect());
    }
    let _ = default_skill_dest(&normalized)?;
    Ok(vec![normalized])
}

pub fn default_skill_dest(agent: &str) -> Result<PathBuf> {
    let home = home_dir()?;
    let normalized = normalize_agent_name(agent);
    let path = match normalized.as_str() {
        "hermes" => home.join(".hermes/skills/productivity/jikji/SKILL.md"),
        "codex" | "omx" => home.join(".codex/skills/jikji/SKILL.md"),
        "claude" => home.join(".claude/skills/jikji/SKILL.md"),
        "opencode" => home.join(".config/opencode/skills/jikji/SKILL.md"),
        "openclo" => home.join(".openclo/skills/jikji/SKILL.md"),
        "nanoclo" => home.join(".nanoclo/skills/jikji/SKILL.md"),
        "generic" | "custom" => home.join(".local/share/agent-skills/jikji/SKILL.md"),
        _ => {
            return Err(invalid_input(format!(
                "Unknown agent {agent:?}. Known agents: hermes, codex, omx, claude, opencode, openclo, nanoclo, generic"
            )));
        }
    };
    Ok(path)
}

pub fn install_agent_skill(
    agent: &str,
    dest: Option<&Path>,
    force: bool,
) -> Result<AgentSkillInstallResult> {
    let normalized = normalize_agent_name(agent);
    let target = match dest {
        Some(path) => absolute_path(path)?,
        None => default_skill_dest(&normalized)?,
    };
    if target.exists() && !force {
        let existing = fs::read_to_string(&target).map_err(|source| io_error(&target, source))?;
        if existing == SKILL_MARKDOWN {
            return Ok(AgentSkillInstallResult {
                agent: normalized,
                path: target,
                installed: false,
                message: "already current".to_owned(),
            });
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    fs::write(&target, SKILL_MARKDOWN).map_err(|source| io_error(&target, source))?;
    Ok(AgentSkillInstallResult {
        agent: normalized,
        path: target,
        installed: true,
        message: if force {
            "installed (forced)"
        } else {
            "installed or upgraded"
        }
        .to_owned(),
    })
}

pub fn skill_markdown() -> &'static str {
    SKILL_MARKDOWN
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = env::current_dir().map_err(|source| io_error(".", source))?;
    Ok(cwd.join(path))
}

fn home_dir() -> Result<PathBuf> {
    home_dir_from_vars(
        env::var_os("JIKJI_AGENT_HOME").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
        env::var_os("USERPROFILE").map(PathBuf::from),
    )
    .ok_or_else(|| invalid_input("HOME or USERPROFILE is not set"))
}

fn home_dir_from_vars(
    agent_home: Option<PathBuf>,
    home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
) -> Option<PathBuf> {
    agent_home.or(home).or(user_profile)
}

fn invalid_input(message: impl Into<String>) -> jikji_core::JikjiError {
    io_error(
        "<agent>",
        Error::new(ErrorKind::InvalidInput, message.into()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        AGENT_RULES_BEGIN, AGENT_RULES_END, default_agent_routes, expand_agent_selection,
        home_dir_from_vars, install_agent_skill, remove_routing_block, skill_markdown,
        write_routing_block,
    };

    #[test]
    fn default_agent_routes_name_prepare_and_find_surfaces() {
        let routes = default_agent_routes();

        assert!(routes.iter().any(|route| route.command == "jikji prepare"));
        assert!(routes.iter().any(|route| route.command == "jikji find"));
    }

    #[test]
    fn install_agent_skill_is_idempotent_without_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("SKILL.md");

        let first = install_agent_skill("any", Some(&dest), false).expect("install");
        let second = install_agent_skill("any", Some(&dest), false).expect("skip");

        assert!(first.installed);
        assert!(!second.installed);
        let installed = fs::read_to_string(dest).expect("skill");
        assert!(installed.contains("Scope Gate: Jikji Find First"));
        assert!(installed.contains("known-path reads or edits"));
        assert!(!installed.contains("Absolute Rule: Jikji Find First"));
        assert_eq!(installed, skill_markdown());
    }

    #[test]
    fn install_agent_skill_upgrades_stale_managed_skill_without_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("SKILL.md");
        fs::write(&dest, "---\nname: jikji\n---\nold routing\n").expect("stale skill");

        let result = install_agent_skill("codex", Some(&dest), false).expect("upgrade");

        assert!(result.installed);
        assert_eq!(fs::read_to_string(dest).expect("skill"), skill_markdown());
    }

    #[test]
    fn agent_selection_expands_all_aliases() {
        let agents = expand_agent_selection("all").expect("all");

        assert!(agents.contains(&"codex".to_owned()));
        assert!(agents.contains(&"claude".to_owned()));
        assert_eq!(
            expand_agent_selection("open-clo").expect("alias"),
            ["openclo"]
        );
        assert!(agents.contains(&"hermes".to_owned()));
        assert!(agents.contains(&"opencode".to_owned()));
    }

    #[test]
    fn home_dir_uses_userprofile_when_home_is_absent() {
        let home = home_dir_from_vars(None, None, Some("C:/Users/runneradmin".into()));

        assert_eq!(home, Some("C:/Users/runneradmin".into()));
    }

    #[test]
    fn routing_block_write_and_remove_are_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# Existing\n\nKeep this.\n").expect("seed");

        let first = write_routing_block(&path).expect("write");
        let second = write_routing_block(&path).expect("rewrite");
        let written = fs::read_to_string(&path).expect("read");

        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(written.matches(AGENT_RULES_BEGIN).count(), 1);
        assert!(written.contains(AGENT_RULES_END));
        assert!(written.contains("Keep this."));
        assert!(written.contains("JIKJI SCOPE GATE"));
        assert!(written.contains("local file/folder discovery intent"));
        assert!(written.contains("Negative: read"));
        assert!(
            written
                .contains("Retry proof and stop restrictions apply only to the discovery substep")
        );
        assert!(!written.contains("ABSOLUTE RULE — JIKJI FIRST"));
        assert!(
            !written
                .contains("Do not begin with grep, rg, glob, find, fd, ls, tree, cat, skills_list")
        );

        for required in [
            "only when ALL are true",
            "target path is unknown or a corpus-content search",
            "explicit or reasonably inferred bounded root",
            "general QA, explanations, coding advice, web search",
            "known-path reads or edits, file stats",
            "process/git/build/test/package commands",
            "directory listings, or LSP symbol navigation",
            "A root's existence or the words file/search alone are not sufficient",
            "Installation requires explicit user authorization",
            "Explicit requests for Jikji admin commands are also allowed",
            "exactly one Jikji retry",
            "--after-jikji-retry --retry-proof RETRY_PROOF",
            "raw_fallback_after_retry",
            "not later reads, edits, or tests",
        ] {
            assert!(
                written.contains(required),
                "missing routing policy: {required}"
            );
        }

        let removed = remove_routing_block(&path).expect("remove");
        let final_text = fs::read_to_string(&path).expect("final read");

        assert!(removed.changed);
        assert!(!final_text.contains("JIKJI ROUTING"));
        assert!(final_text.contains("Keep this."));
    }

    #[test]
    fn embedded_skill_preserves_safety_contract() {
        let text = skill_markdown();

        assert!(text.contains("Scope Gate: Jikji Find First"));
        assert!(text.contains("known-path reads or edits"));
        assert!(text.contains("Installation requires explicit user authorization"));
        assert!(text.contains("Never run a second Jikji retry"));
        assert!(text.contains("raw_fallback_allowed=false"));
        assert!(
            text.contains("Retry proof and stop restrictions apply only to the discovery substep")
        );
        for required in [
            "require ALL",
            "local file/folder discovery intent",
            "target path is unknown",
            "corpus-content search",
            "explicit or reasonably inferred bounded root",
            "general QA, explanations, coding advice, web search",
            "process/git/build/test/package commands",
            "LSP symbol navigation",
            "Explicit Jikji admin requests are allowed separately",
            "Known-path reads, edits, tests, and other requested work remain allowed",
        ] {
            assert!(text.contains(required), "missing skill policy: {required}");
        }
        assert!(!text.contains("do not invoke filesystem tools"));
        assert!(!text.contains("Open original files only for final verification"));
        assert!(!text.contains("Install it from GitHub instead of giving up"));
        assert_eq!(text, include_str!("../../../skills/jikji/SKILL.md"));
    }
}
