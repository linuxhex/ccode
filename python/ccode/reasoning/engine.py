"""Reasoning engine - orchestrates model calling, tool execution, and context management."""

import json
import os
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

from ccode.reasoning.context import ContextManager
from ccode.reasoning.memory import CompactionPipeline
from ccode.reasoning.prompt import PromptBuilder
from ccode.reasoning.sampler import (
    ProviderRouter,
    ProviderConfig,
    SampleRequest,
    StreamChunk,
    StreamChannel,
    ThinkingConfig,
    ToolDefinition,
)
from ccode.reasoning.sampler.provider import SampleResponse
from ccode.tools import ToolExecutor


# ── Built-in tool definitions ──

def _build_tool_registry() -> tuple[list[ToolDefinition], dict[str, Callable]]:
    """Return (tool_definitions, name_to_executor) for built-in tools."""

    definitions = [
        ToolDefinition(
            name="shell",
            description="Execute a shell command and return stdout, stderr, and exit code. Use for git operations, build commands, file searches, and any CLI tool.",
            parameters={
                "command": {"type": "string", "description": "Shell command to execute"},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default 30)", "default": 30},
            },
            required=["command"],
        ),
        ToolDefinition(
            name="read_file",
            description="Read the contents of a file. Returns the file content and size in bytes.",
            parameters={
                "path": {"type": "string", "description": "Absolute or relative path to the file"},
            },
            required=["path"],
        ),
        ToolDefinition(
            name="write_file",
            description="Write content to a file. Creates parent directories if needed. Overwrites existing files.",
            parameters={
                "path": {"type": "string", "description": "Absolute or relative path to the file"},
                "content": {"type": "string", "description": "Content to write to the file"},
            },
            required=["path", "content"],
        ),
        ToolDefinition(
            name="list_dir",
            description="List entries in a directory. Returns name, is_dir, and size for each entry.",
            parameters={
                "path": {"type": "string", "description": "Absolute or relative path to the directory"},
            },
            required=["path"],
        ),
    ]

    executors: dict[str, Callable] = {}
    return definitions, executors


class ReasoningEngine:
    """Multi-provider LLM reasoning engine with agentic tool loop."""

    MAX_ITERATIONS = 10
    COMPACTION_THRESHOLD = 0.75

    def __init__(self, model: str = "claude-sonnet-4-20250514") -> None:
        self.model = model
        self.context = ContextManager()
        self.router = ProviderRouter()
        self.prompt_builder = PromptBuilder()
        self.compactor = CompactionPipeline()
        self._tool_definitions: list[ToolDefinition] = []
        self._tool_initialized = False
        self._initialized = False

    def _ensure_initialized(self) -> None:
        if self._initialized:
            return

        anthropic_key = os.environ.get("ANTHROPIC_API_KEY", "")
        if anthropic_key:
            self.router.register_provider(ProviderConfig(
                name="claude",
                api_key=anthropic_key,
                context_window=200000,
                supports_thinking=True,
                models=["claude-sonnet-4-20250514", "claude-3-5-sonnet", "claude-3-opus"],
            ))

        openai_key = os.environ.get("OPENAI_API_KEY", "")
        if openai_key:
            self.router.register_provider(ProviderConfig(
                name="openai",
                api_key=openai_key,
                context_window=128000,
                supports_thinking=True,
                models=["gpt-4o", "gpt-4-turbo", "o1", "o3"],
            ))

        gemini_key = os.environ.get("GEMINI_API_KEY", "")
        if gemini_key:
            self.router.register_provider(ProviderConfig(
                name="gemini",
                api_key=gemini_key,
                context_window=1000000,
                supports_thinking=True,
                models=["gemini-2.0-flash", "gemini-1.5-pro"],
            ))

        self._initialized = True

    def _ensure_tools(self) -> None:
        if self._tool_initialized:
            return
        self._tool_definitions, _ = _build_tool_registry()
        self._tool_initialized = True

    def configure_api_key(self, provider: str, api_key: str) -> bool:
        """Dynamically register a provider with an API key (like Claude Code /login)."""
        configs = {
            "anthropic": ProviderConfig(
                name="claude", api_key=api_key, context_window=200000,
                supports_thinking=True,
                models=["claude-sonnet-4-20250514", "claude-3-5-sonnet", "claude-3-opus"],
            ),
            "openai": ProviderConfig(
                name="openai", api_key=api_key, context_window=128000,
                supports_thinking=True,
                models=["gpt-4o", "gpt-4-turbo", "o1", "o3"],
            ),
            "gemini": ProviderConfig(
                name="gemini", api_key=api_key, context_window=1000000,
                supports_thinking=True,
                models=["gemini-2.0-flash", "gemini-1.5-pro"],
            ),
        }
        cfg = configs.get(provider)
        if not cfg:
            return False
        if cfg.name in self.router._providers:
            del self.router._providers[cfg.name]
        if cfg.name in self.router._configs:
            del self.router._configs[cfg.name]
        self.router.register_provider(cfg)
        return True

    async def chat(
        self,
        user_message: str,
        project_dir: Path,
        tool_executor: ToolExecutor | None = None,
        progress_callback: Callable[[str], None] | None = None,
    ) -> AsyncIterator[str]:
        """Agentic chat: sends message, executes tool calls in a loop until done."""
        self._ensure_initialized()
        self._ensure_tools()

        if not self.prompt_builder.project_context:
            self.prompt_builder.set_project_dir(project_dir)

        if tool_executor is None:
            tool_executor = ToolExecutor()

        self.context.add_user_message(user_message)

        for iteration in range(self.MAX_ITERATIONS):
            self._maybe_compact()

            system_prompt = self.prompt_builder.build(
                tools_description=self._format_tools_description(),
            )

            request = SampleRequest(
                model=self.model,
                messages=self.context.get_messages(),
                system_prompt=system_prompt,
                tools=self._tool_definitions if tool_executor else None,
                tool_choice="auto" if tool_executor else None,
            )

            try:
                full_text = ""
                tool_calls_map: dict[str, dict[str, Any]] = {}

                async for chunk in self.router.sample_stream(request):
                    if chunk.channel == StreamChannel.TEXT and chunk.content:
                        full_text += chunk.content
                        yield chunk.content

                    if chunk.tool_calls:
                        for tc in chunk.tool_calls:
                            tc_id = tc.get("id", "")
                            if tc_id and tc_id not in tool_calls_map:
                                tool_calls_map[tc_id] = {
                                    "id": tc_id,
                                    "name": tc.get("name", ""),
                                    "arguments": "",
                                }
                            if tc.get("arguments"):
                                target_id = tc_id or (list(tool_calls_map.keys())[-1] if tool_calls_map else None)
                                if target_id and target_id in tool_calls_map:
                                    tool_calls_map[target_id]["arguments"] += tc["arguments"]

                tool_calls = list(tool_calls_map.values()) if tool_calls_map else None

            except Exception as e:
                yield f"\n\n[Error: {e}]"
                break

            if not tool_calls:
                if full_text:
                    self.context.add_assistant_message(full_text)
                break

            assistant_text = full_text or f"(executed {len(tool_calls)} tool(s))"
            self.context.add_assistant_message(assistant_text, tool_calls=tool_calls)

            for tc in tool_calls:
                tool_name = tc.get("name", "")
                tool_call_id = tc.get("id", tool_name)
                try:
                    args = json.loads(tc.get("arguments", "{}"))
                except json.JSONDecodeError:
                    args = {}

                yield f"\n\n[Calling {tool_name}({json.dumps(args, ensure_ascii=False)})]\n"

                try:
                    result = await tool_executor.execute(tool_name, args)
                    result_str = json.dumps(result, ensure_ascii=False, default=str)
                except Exception as e:
                    result_str = json.dumps({"error": str(e)}, ensure_ascii=False)

                preview = result_str[:200] + ("..." if len(result_str) > 200 else "")
                yield f"\n\n[ToolResult: {tool_name}] {preview}\n"

                self.context.add_tool_result(tool_call_id, result_str)

            if progress_callback:
                progress_callback(f"iteration {iteration + 1}: {len(tool_calls)} tool(s)")
        else:
            yield "\n\n[Warning: reached max iterations]"

    def _maybe_compact(self) -> None:
        """Run compaction pipeline if context exceeds token threshold."""
        estimated = self.context.estimate_tokens()
        limit = int(self.context.max_tokens * self.COMPACTION_THRESHOLD)
        if estimated <= limit:
            return

        messages = self.context.get_messages()
        compacted, results = self.compactor.run_pipeline(messages)
        if results:
            self.context.set_messages(compacted)

    def _format_tools_description(self) -> str:
        """Format tool definitions as a human-readable description for the system prompt."""
        if not self._tool_definitions:
            return ""
        lines = ["Available tools:"]
        for t in self._tool_definitions:
            lines.append(f"\n### {t.name}")
            lines.append(t.description)
            params = t.parameters
            if params:
                lines.append("Parameters:")
                for pname, pschema in params.items():
                    req = " (required)" if pname in t.required else ""
                    desc = pschema.get("description", "")
                    lines.append(f"  - {pname}{req}: {desc}")
        return "\n".join(lines)
