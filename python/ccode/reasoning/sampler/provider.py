"""Provider interface and data types."""

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, AsyncIterator, Optional
from collections.abc import AsyncIterator as AsyncIteratorABC


class StreamChannel(str, Enum):
    """Stream channel types."""
    TEXT = "text"
    REASONING = "reasoning"
    TOOL_USE = "tool_use"


@dataclass
class ThinkingConfig:
    """Thinking/reasoning mode configuration."""
    enabled: bool = False
    budget_tokens: Optional[int] = None
    reasoning_effort: Optional[str] = None

    def to_openai_format(self) -> dict[str, Any]:
        if not self.enabled:
            return {}
        result: dict[str, Any] = {}
        if self.reasoning_effort:
            result["reasoning_effort"] = self.reasoning_effort
        return result

    def to_claude_format(self) -> dict[str, Any]:
        if not self.enabled:
            return {}
        result: dict[str, Any] = {"type": "enabled"}
        if self.budget_tokens:
            result["budget_tokens"] = self.budget_tokens
        return result

    def to_gemini_format(self) -> dict[str, Any]:
        if not self.enabled:
            return {}
        return {"thinking_config": {"thinking_budget": self.budget_tokens or -1}}


@dataclass
class ToolDefinition:
    """Tool definition for LLM."""
    name: str
    description: str
    parameters: dict[str, Any]
    required: list[str] = field(default_factory=list)


@dataclass
class SampleRequest:
    """Request to sample from LLM."""
    model: str
    messages: list[dict[str, Any]]
    max_tokens: int = 8192
    temperature: Optional[float] = None
    top_p: Optional[float] = None
    thinking: Optional[ThinkingConfig] = None
    system_prompt: Optional[str] = None
    tools: Optional[list[ToolDefinition]] = None
    tool_choice: Optional[str] = None
    prompt_cache_key: Optional[str] = None
    extra_headers: dict[str, str] = field(default_factory=dict)
    goal_verify: bool = False
    summarize: bool = False
    idle_timeout_secs: Optional[int] = None


@dataclass
class StreamChunk:
    """A chunk from streaming response."""
    channel: StreamChannel
    content: str
    finish_reason: Optional[str] = None
    tool_calls: Optional[list[dict[str, Any]]] = None
    usage: Optional[dict[str, int]] = None


@dataclass
class SampleResponse:
    """Complete response from LLM."""
    content: str
    finish_reason: str
    tool_calls: Optional[list[dict[str, Any]]] = None
    usage: Optional[dict[str, int]] = None
    reasoning_content: Optional[str] = None


class Provider:
    """Base provider interface."""

    def __init__(self, name: str, api_key: str, base_url: Optional[str] = None):
        self.name = name
        self.api_key = api_key
        self.base_url = base_url

    async def sample(self, request: SampleRequest) -> SampleResponse:
        chunks: list[StreamChunk] = []
        async for chunk in self.sample_stream(request):
            chunks.append(chunk)

        content = "".join(c.content for c in chunks if c.channel == StreamChannel.TEXT)
        reasoning = "".join(c.content for c in chunks if c.channel == StreamChannel.REASONING)
        tool_calls = None
        usage = None
        finish_reason = "stop"

        for chunk in chunks:
            if chunk.tool_calls:
                tool_calls = chunk.tool_calls
            if chunk.usage:
                usage = chunk.usage
            if chunk.finish_reason:
                finish_reason = chunk.finish_reason

        return SampleResponse(
            content=content,
            finish_reason=finish_reason,
            tool_calls=tool_calls,
            usage=usage,
            reasoning_content=reasoning if reasoning else None,
        )

    async def sample_stream(self, request: SampleRequest) -> AsyncIteratorABC[StreamChunk]:
        raise NotImplementedError
        yield

    def build_tools_schema(self, tools: list[ToolDefinition]) -> list[dict[str, Any]]:
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": {
                    "type": "object",
                    "properties": t.parameters,
                    "required": t.required,
                },
            }
            for t in tools
        ]


@dataclass
class ProviderConfig:
    """Provider configuration."""
    name: str
    api_key: str
    base_url: Optional[str] = None
    context_window: int = 128000
    supports_thinking: bool = False
    supports_vision: bool = False
    extra_headers: dict[str, str] = field(default_factory=dict)
    models: list[str] = field(default_factory=list)
