"""4-level compaction pipeline.

Levels:
- L0 Snip: Truncate tool outputs exceeding budget
- L1 MicroCompact: Age-based eviction for cache awareness
- L2 AutoCompact: Hot→Warm→Cold temperature demotion
- L3 ReactiveCompact: LLM-generated structured summary
"""

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Optional
import re


class CompactionLevel(int, Enum):
    """Compaction levels."""
    SNIP = 0
    MICRO = 1
    AUTO = 2
    REACTIVE = 3


# Tool output budget (aligned with Rust implementation)
TOOL_OUTPUT_BUDGET = 24000
TOOL_OUTPUT_HEAD = 10000
TOOL_OUTPUT_TAIL = 10000

# Required sections for structured summary
REQUIRED_SECTIONS = [
    "Primary Request",
    "Key Concepts",
    "Files",
    "Errors",
    "Problem Solving",
    "User Messages",
    "Pending Tasks",
    "Current Work",
    "Next Step",
]

MIN_SECTIONS_REQUIRED = 7  # At least 7/9 sections must exist


class SummaryValidator:
    """Validates structured summaries from LLM compaction."""

    @staticmethod
    def validate(summary: str) -> tuple[bool, list[str], list[str]]:
        """Validate a structured summary.

        Returns:
            Tuple of (is_valid, found_sections, missing_sections)
        """
        found = []
        missing = []

        for section in REQUIRED_SECTIONS:
            # Check for section header (## Section or **Section**)
            patterns = [
                rf"##\s*{re.escape(section)}",
                rf"\*\*{re.escape(section)}\*\*",
                rf"###\s*{re.escape(section)}",
                rf"^{re.escape(section)}:",
            ]
            if any(re.search(p, summary, re.IGNORECASE | re.MULTILINE) for p in patterns):
                found.append(section)
            else:
                missing.append(section)

        is_valid = len(found) >= MIN_SECTIONS_REQUIRED
        return is_valid, found, missing

    @staticmethod
    def is_degenerate(summary: str) -> bool:
        """Check if summary is degenerate (too short or no structure)."""
        if len(summary) < 100:
            return True

        is_valid, found, _ = SummaryValidator.validate(summary)
        return not is_valid


@dataclass
class CompactionResult:
    """Result of a compaction operation."""
    level: CompactionLevel
    original_size: int
    compacted_size: int
    removed_messages: int = 0
    summary: Optional[str] = None

    @property
    def reduction_ratio(self) -> float:
        if self.original_size == 0:
            return 0.0
        return 1.0 - (self.compacted_size / self.original_size)


class CompactionPipeline:
    """4-level compaction pipeline."""

    def __init__(
        self,
        tool_output_budget: int = TOOL_OUTPUT_BUDGET,
        max_tokens: int = 128000,
    ) -> None:
        self.tool_output_budget = tool_output_budget
        self.max_tokens = max_tokens
        self.consecutive_failures = 0
        self.circuit_breaker_threshold = 3

    def snip_tool_output(self, content: str) -> str:
        """L0: Truncate tool output to budget."""
        if len(content) <= self.tool_output_budget:
            return content

        head = content[:TOOL_OUTPUT_HEAD]
        tail = content[-TOOL_OUTPUT_TAIL:]
        omitted = len(content) - TOOL_OUTPUT_HEAD - TOOL_OUTPUT_TAIL

        return f"{head}\n\n... [{omitted} characters omitted] ...\n\n{tail}"

    def micro_compact(
        self,
        messages: list[dict[str, Any]],
        age_threshold: int = 10,
    ) -> tuple[list[dict[str, Any]], int]:
        """L1: Age-based eviction for older messages."""
        if len(messages) <= age_threshold:
            return messages, 0

        # Keep recent messages, evict oldest tool results
        kept = []
        removed = 0

        for i, msg in enumerate(messages):
            is_old = i < len(messages) - age_threshold
            is_tool_result = msg.get("role") == "tool"

            if is_old and is_tool_result:
                removed += 1
                continue
            kept.append(msg)

        return kept, removed

    def estimate_tokens(self, messages: list[dict[str, Any]]) -> int:
        """Estimate token count from messages."""
        total_chars = sum(len(str(m.get("content", ""))) for m in messages)
        return total_chars // 4

    async def reactive_compact(
        self,
        messages: list[dict[str, Any]],
        llm_call: Any,
    ) -> tuple[str, bool]:
        """L3: Generate structured summary using LLM.

        Args:
            messages: Messages to summarize
            llm_call: Async callable that takes prompt and returns response

        Returns:
            Tuple of (summary, success)
        """
        if self.consecutive_failures >= self.circuit_breaker_threshold:
            return "", False

        # Format messages for summarization
        formatted = []
        for msg in messages:
            role = msg.get("role", "unknown")
            content = str(msg.get("content", ""))[:500]
            formatted.append(f"[{role}] {content}")

        context = "\n".join(formatted)

        prompt = f"""Summarize the following conversation into exactly these 9 sections:

## Primary Request
What the user originally asked for

## Key Concepts
Important concepts and technologies mentioned

## Files
Files that were discussed or modified

## Errors
Any errors encountered and their status

## Problem Solving
Approaches tried and their outcomes

## User Messages
Summary of user's messages and preferences

## Pending Tasks
Tasks that are still pending

## Current Work
What is currently being worked on

## Next Step
The immediate next step

---

Conversation:
{context}
"""

        try:
            summary = await llm_call(prompt)
            is_valid, found, missing = SummaryValidator.validate(summary)

            if is_valid:
                self.consecutive_failures = 0
                return summary, True
            else:
                self.consecutive_failures += 1
                return summary, False
        except Exception:
            self.consecutive_failures += 1
            return "", False

    def run_pipeline(
        self,
        messages: list[dict[str, Any]],
    ) -> tuple[list[dict[str, Any]], list[CompactionResult]]:
        """Run the compaction pipeline synchronously (L0-L2).

        L3 (reactive) requires async and should be called separately.
        """
        results = []

        # L0: Snip tool outputs
        snipped = []
        for msg in messages:
            if msg.get("role") == "tool":
                content = str(msg.get("content", ""))
                new_content = self.snip_tool_output(content)
                if new_content != content:
                    msg = {**msg, "content": new_content}
            snipped.append(msg)

        original_size = self.estimate_tokens(messages)
        snipped_size = self.estimate_tokens(snipped)
        if snipped_size < original_size:
            results.append(CompactionResult(
                level=CompactionLevel.SNIP,
                original_size=original_size,
                compacted_size=snipped_size,
            ))

        # L1: Micro compact
        compacted, removed = self.micro_compact(snipped)
        if removed > 0:
            micro_size = self.estimate_tokens(compacted)
            results.append(CompactionResult(
                level=CompactionLevel.MICRO,
                original_size=snipped_size,
                compacted_size=micro_size,
                removed_messages=removed,
            ))

        return compacted, results
