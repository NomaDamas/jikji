"""Jikji configuration."""
from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Config:
    """Runtime options for non-destructive local knowledge-map preparation."""

    # Caller-provided excludes. Hidden files are handled separately so
    # ``--include-hidden`` does not accidentally opt into sensitive files.
    ignore_patterns: list[str] = field(
        default_factory=lambda: ["~$*", "Thumbs.db", ".DS_Store", "desktop.ini"]
    )
    # Safety deny-list that remains active unless include_sensitive is explicit.
    safety_ignore_patterns: list[str] = field(
        default_factory=lambda: [
            ".git",
            ".hg",
            ".svn",
            "node_modules",
            "__pycache__",
            ".venv",
            "venv",
            ".env",
            ".env.*",
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "*.pem",
            "*.key",
            "*.p12",
            "*.pfx",
        ]
    )
    include_hidden: bool = False
    include_sensitive: bool = False
    max_files: int = 0
    max_hash_bytes: int = 512 * 1024 * 1024
    parse_timeout_s: float = 5.0
    agent_doc_text_max_chars: int = 2_000_000
    agent_doc_text_chunk_chars: int = 1_000_000
    enable_media_index: bool = False
    media_index_max_mb: float = 25.0
    # When true, `jikji prepare` writes/refreshes a routing block in project-local
    # agent rule files (AGENTS.md, CLAUDE.md, .cursorrules) pointing agents at
    # `jikji find` before native crawlers. Removed again by `jikji clean`.
    write_agent_rules: bool = True
