"""Project context discovery and injection.

Discovers CCODE.md / CLAUDE.md / AGENTS.md files and injects them into
the system prompt. Enforces a 32KB budget (aligned with Codex's
project_doc_max_bytes default).
"""

from pathlib import Path
from dataclasses import dataclass, field

# Aligned with Codex project_doc_max_bytes default
MAX_CONTEXT_BYTES = 32 * 1024

# Files to discover, in priority order
CONTEXT_FILES = ["CCODE.md", "CLAUDE.md", "AGENTS.md"]


@dataclass
class DiscoveredFile:
    """A discovered context file."""
    path: Path
    name: str
    content: str
    size: int


@dataclass
class ProjectContext:
    """Project context manager."""

    project_dir: Path
    discovered: list[DiscoveredFile] = field(default_factory=list)
    total_size: int = 0
    truncated: bool = False

    def discover(self) -> None:
        """Discover context files in project directory."""
        self.discovered.clear()
        self.total_size = 0

        for name in CONTEXT_FILES:
            path = self.project_dir / name
            if path.exists() and path.is_file():
                try:
                    content = path.read_text(encoding="utf-8")
                    size = len(content.encode("utf-8"))
                    self.discovered.append(DiscoveredFile(
                        path=path,
                        name=name,
                        content=content,
                        size=size,
                    ))
                    self.total_size += size
                except Exception:
                    pass

        if self.total_size > MAX_CONTEXT_BYTES:
            self._truncate()

    def _truncate(self) -> None:
        """Truncate content to fit within budget."""
        self.truncated = True
        remaining = MAX_CONTEXT_BYTES

        for file in self.discovered:
            if file.size <= remaining:
                remaining -= file.size
            else:
                # Truncate this file
                encoded = file.content.encode("utf-8")
                if remaining > 100:
                    # Keep first portion with a note
                    truncated_bytes = encoded[:remaining - 100]
                    file.content = truncated_bytes.decode("utf-8", errors="ignore")
                    file.content += f"\n\n... [truncated, {file.size - remaining + 100} bytes omitted. Use read_file to view full content]"
                    file.size = remaining
                    remaining = 0
                else:
                    file.content = ""
                    file.size = 0

    def format_for_prompt(self) -> str:
        """Format discovered context for system prompt injection."""
        if not self.discovered:
            return ""

        parts = ["<project_context>"]
        for file in self.discovered:
            if file.content:
                parts.append(f"\n## {file.name}\n\n{file.content}")
        parts.append("\n</project_context>")

        if self.truncated:
            parts.append("\n<!-- Some context files were truncated due to size. Use read_file to view full content. -->")

        return "\n".join(parts)

    def get_summary(self) -> str:
        """Get a summary of discovered context."""
        if not self.discovered:
            return "No project context files found"

        files = ", ".join(f.name for f in self.discovered)
        status = " (truncated)" if self.truncated else ""
        return f"Found: {files} ({self.total_size} bytes{status})"
