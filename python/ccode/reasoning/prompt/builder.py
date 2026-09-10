"""Prompt builder - constructs system prompts with all context."""

from pathlib import Path
from typing import Optional
from dataclasses import dataclass, field

from ccode.reasoning.prompt.project_context import ProjectContext
from ccode.reasoning.prompt.stable_prefix import StablePrefix


DEFAULT_SYSTEM_PROMPT = """You are CCODE, an AI coding assistant. You help users with software engineering tasks.

Core principles:
- Be concise and direct
- Prefer editing existing files over creating new ones
- Write secure code without common vulnerabilities
- Test changes when possible
- Explain your reasoning briefly

When making code changes:
- Match the existing code style
- Don't add unnecessary abstractions
- Only add comments for non-obvious logic
- Handle errors at system boundaries
"""


@dataclass
class PromptConfig:
    """Configuration for prompt building."""
    base_prompt: str = DEFAULT_SYSTEM_PROMPT
    include_project_context: bool = True
    include_tools: bool = True
    max_context_bytes: int = 32 * 1024


class PromptBuilder:
    """Builds system prompts with all context."""

    def __init__(self, config: Optional[PromptConfig] = None) -> None:
        self.config = config or PromptConfig()
        self.stable_prefix = StablePrefix()
        self.project_context: Optional[ProjectContext] = None

    def set_project_dir(self, project_dir: Path) -> None:
        """Set project directory and discover context."""
        self.project_context = ProjectContext(project_dir=project_dir)
        self.project_context.discover()

    def build(
        self,
        tools_description: Optional[str] = None,
        extra_context: Optional[str] = None,
    ) -> str:
        """Build the complete system prompt."""
        parts = [self.config.base_prompt]

        if self.config.include_project_context and self.project_context:
            ctx = self.project_context.format_for_prompt()
            if ctx:
                parts.append("\n" + ctx)

        if extra_context:
            parts.append("\n" + extra_context)

        if self.config.include_tools and tools_description:
            parts.append("\n<tools>\n" + tools_description + "\n</tools>")

        system_prompt = "\n".join(parts)

        # Lock stable prefix on first build
        if not self.stable_prefix.is_locked():
            self.stable_prefix.lock(self.config.base_prompt)

        return system_prompt

    def build_with_cache_markers(
        self,
        provider: str,
        tools_description: Optional[str] = None,
    ) -> tuple[str, list[dict]]:
        """Build prompt with cache control markers.

        Returns:
            Tuple of (system_prompt, cache_markers)
            cache_markers is a list of {"type": "cache_control", ...} for providers
            that support it (e.g., Anthropic).
        """
        system_prompt = self.build(tools_description=tools_description)
        markers: list[dict] = []

        if provider == "claude":
            # Anthropic supports cache_control on system and tools
            markers.append({
                "type": "cache_control",
                "cache_control": {"type": "ephemeral"},
                "position": "system",
            })

        return system_prompt, markers

    def get_stable_prefix_hash(self) -> Optional[str]:
        """Get hash of stable prefix for cache verification."""
        return self.stable_prefix.get_hash()

    def verify_prefix_stability(self, prefix: str) -> bool:
        """Verify prefix matches locked stable prefix."""
        return self.stable_prefix.verify(prefix)
