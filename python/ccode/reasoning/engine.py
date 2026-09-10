"""Reasoning engine - orchestrates model calling and context management."""

import os
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

from ccode.reasoning.context import ContextManager
from ccode.reasoning.sampler import (
    ProviderRouter,
    ProviderConfig,
    SampleRequest,
    StreamChunk,
    StreamChannel,
    ThinkingConfig,
    OpenAIProvider,
    ClaudeProvider,
    GeminiProvider,
)


class ReasoningEngine:
    """Multi-provider LLM reasoning engine."""

    def __init__(self, model: str = "claude-sonnet-4-20250514") -> None:
        self.model = model
        self.context = ContextManager()
        self.router = ProviderRouter()
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

    async def chat(
        self,
        user_message: str,
        project_dir: Path,
        tool_executor: Any = None,
        progress_callback: Callable[[str], None] | None = None,
    ) -> AsyncIterator[str]:
        self._ensure_initialized()
        self.context.add_user_message(user_message)

        request = SampleRequest(
            model=self.model,
            messages=self.context.get_messages(),
            system_prompt="You are CCODE, an AI coding assistant. Help the user with their coding tasks.",
        )

        full_response = ""

        try:
            async for chunk in self.router.sample_stream(request):
                if chunk.channel == StreamChannel.TEXT and chunk.content:
                    full_response += chunk.content
                    yield chunk.content
        except Exception as e:
            yield f"\n\n[Error: {e}]"

        if full_response:
            self.context.add_assistant_message(full_response)
