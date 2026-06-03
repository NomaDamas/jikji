"""Install the reusable Jikji local-file-discovery skill for local agents."""
from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path


@dataclass
class AgentSkillInstallResult:
    agent: str
    path: Path
    installed: bool
    message: str


AGENT_SKILL_TARGETS: dict[str, Path] = {
    "hermes": Path.home() / ".hermes" / "skills" / "productivity" / "jikji" / "SKILL.md",
    "codex": Path.home() / ".codex" / "skills" / "jikji" / "SKILL.md",
    "omx": Path.home() / ".codex" / "skills" / "jikji" / "SKILL.md",
    "claude": Path.home() / ".claude" / "skills" / "jikji" / "SKILL.md",
    "claude-code": Path.home() / ".claude" / "skills" / "jikji" / "SKILL.md",
    "opencode": Path.home() / ".config" / "opencode" / "skills" / "jikji" / "SKILL.md",
    "openclo": Path.home() / ".openclo" / "skills" / "jikji" / "SKILL.md",
    "open-clo": Path.home() / ".openclo" / "skills" / "jikji" / "SKILL.md",
    "nanoclo": Path.home() / ".nanoclo" / "skills" / "jikji" / "SKILL.md",
    "nano-clo": Path.home() / ".nanoclo" / "skills" / "jikji" / "SKILL.md",
    "generic": Path.home() / ".local" / "share" / "agent-skills" / "jikji" / "SKILL.md",
    "custom": Path.home() / ".local" / "share" / "agent-skills" / "jikji" / "SKILL.md",
    "universal": Path.home() / ".local" / "share" / "agent-skills" / "jikji" / "SKILL.md",
}

INSTALLABLE_AGENTS = (
    "hermes",
    "codex",
    "omx",
    "claude",
    "opencode",
    "openclo",
    "nanoclo",
    "generic",
)
CUSTOM_AGENT_NAMES = ("custom", "universal", "any")


def repo_skill_path() -> Path:
    path = Path(__file__).resolve().parents[2] / "skills" / "jikji" / "SKILL.md"
    if not path.exists():
        raise FileNotFoundError(f"Cannot find repo skill file: {path}")
    return path


def normalize_agent_name(agent: str) -> str:
    normalized = agent.strip().lower().replace("_", "-")
    aliases = {
        "claudecode": "claude",
        "claude-code": "claude",
        "open-code": "opencode",
        "openclone": "openclo",
        "open-clone": "openclo",
        "open-clo": "openclo",
        "nano-clone": "nanoclo",
        "nano-clo": "nanoclo",
        "any": "custom",
    }
    return aliases.get(normalized, normalized)


def default_skill_dest(agent: str) -> Path:
    normalized = normalize_agent_name(agent)
    try:
        return AGENT_SKILL_TARGETS[normalized]
    except KeyError as exc:
        known = ", ".join(INSTALLABLE_AGENTS)
        raise ValueError(f"Unknown agent {agent!r}. Known agents: {known}") from exc


def install_agent_skill(
    agent: str,
    *,
    dest: Path | None = None,
    force: bool = False,
) -> AgentSkillInstallResult:
    normalized = normalize_agent_name(agent)
    target = Path(dest).expanduser().resolve() if dest is not None else default_skill_dest(normalized)
    source = repo_skill_path()
    if target.exists() and not force:
        return AgentSkillInstallResult(
            normalized,
            target,
            False,
            "already exists; pass --force to overwrite",
        )
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    return AgentSkillInstallResult(normalized, target, True, "installed")


def expand_agent_selection(agent: str) -> tuple[str, ...]:
    normalized = normalize_agent_name(agent)
    if normalized == "all":
        return INSTALLABLE_AGENTS
    return (normalized,)
