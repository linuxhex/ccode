"""OpenAI-compatible provider."""

import json
from collections.abc import AsyncIterator
from typing import Any, Optional

from openai import AsyncOpenAI

from ccode.reasoning.sampler.provider import (
    Provider,
    SampleRequest,
    StreamChunk,
    StreamChannel,
)


class OpenAIProvider(Provider):
    """OpenAI-compatible provider (GPT, o1, o3, DeepSeek, Qwen, etc.)."""

    def __init__(
        self,
        name: str = "openai",
        api_key: str = "",
        base_url: Optional[str] = None,
    ):
        super().__init__(name, api_key, base_url)
        self._client: Optional[AsyncOpenAI] = None

    def _get_client(self) -> AsyncOpenAI:
        if self._client is None:
            self._client = AsyncOpenAI(
                api_key=self.api_key,
                base_url=self.base_url,
            )
        return self._client

    def _build_messages(self, request: SampleRequest) -> list[dict[str, Any]]:
        messages = []
        if request.system_prompt:
            messages.append({"role": "system", "content": request.system_prompt})
        messages.extend(request.messages)
        return messages

    def _build_tools(self, request: SampleRequest) -> Optional[list[dict[str, Any]]]:
        if not request.tools:
            return None
        return [
            {
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": {
                        "type": "object",
                        "properties": t.parameters,
                        "required": t.required,
                    },
                },
            }
            for t in request.tools
        ]

    async def sample_stream(
        self, request: SampleRequest
    ) -> AsyncIterator[StreamChunk]:
        client = self._get_client()
        messages = self._build_messages(request)

        kwargs: dict[str, Any] = {
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens,
            "stream": True,
        }

        if request.temperature is not None:
            kwargs["temperature"] = request.temperature
        if request.top_p is not None:
            kwargs["top_p"] = request.top_p

        tools = self._build_tools(request)
        if tools:
            kwargs["tools"] = tools
            if request.tool_choice:
                kwargs["tool_choice"] = request.tool_choice

        if request.thinking:
            thinking_fmt = request.thinking.to_openai_format()
            kwargs.update(thinking_fmt)

        if request.extra_headers:
            kwargs["extra_headers"] = request.extra_headers

        stream = await client.chat.completions.create(**kwargs)

        async for chunk in stream:
            if not chunk.choices:
                continue

            delta = chunk.choices[0].delta
            finish_reason = chunk.choices[0].finish_reason

            if delta.content:
                yield StreamChunk(
                    channel=StreamChannel.TEXT,
                    content=delta.content,
                    finish_reason=finish_reason,
                )

            if hasattr(delta, "reasoning_content") and delta.reasoning_content:
                yield StreamChunk(
                    channel=StreamChannel.REASONING,
                    content=delta.reasoning_content,
                )

            if delta.tool_calls:
                tool_calls = []
                for tc in delta.tool_calls:
                    tool_calls.append({
                        "id": tc.id,
                        "name": tc.function.name,
                        "arguments": tc.function.arguments,
                    })
                yield StreamChunk(
                    channel=StreamChannel.TOOL_USE,
                    content="",
                    tool_calls=tool_calls,
                    finish_reason=finish_reason,
                )

            if chunk.usage:
                yield StreamChunk(
                    channel=StreamChannel.TEXT,
                    content="",
                    usage={
                        "prompt_tokens": chunk.usage.prompt_tokens,
                        "completion_tokens": chunk.usage.completion_tokens,
                        "total_tokens": chunk.usage.total_tokens,
                    },
                )
