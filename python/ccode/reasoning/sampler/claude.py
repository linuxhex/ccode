"""Claude (Anthropic) provider."""

import json
from collections.abc import AsyncIterator
from typing import Any, Optional

from anthropic import AsyncAnthropic

from ccode.reasoning.sampler.provider import (
    Provider,
    SampleRequest,
    StreamChunk,
    StreamChannel,
)


class ClaudeProvider(Provider):
    """Anthropic Claude provider."""

    def __init__(
        self,
        name: str = "claude",
        api_key: str = "",
        base_url: Optional[str] = None,
    ):
        super().__init__(name, api_key, base_url)
        self._client: Optional[AsyncAnthropic] = None

    def _get_client(self) -> AsyncAnthropic:
        if self._client is None:
            kwargs: dict[str, Any] = {"api_key": self.api_key}
            if self.base_url:
                kwargs["base_url"] = self.base_url
            self._client = AsyncAnthropic(**kwargs)
        return self._client

    def _build_tools(self, request: SampleRequest) -> Optional[list[dict[str, Any]]]:
        if not request.tools:
            return None
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
            for t in request.tools
        ]

    async def sample_stream(
        self, request: SampleRequest
    ) -> AsyncIterator[StreamChunk]:
        client = self._get_client()

        kwargs: dict[str, Any] = {
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": request.messages,
            "stream": True,
        }

        if request.system_prompt:
            kwargs["system"] = request.system_prompt
        if request.temperature is not None:
            kwargs["temperature"] = request.temperature
        if request.top_p is not None:
            kwargs["top_p"] = request.top_p

        tools = self._build_tools(request)
        if tools:
            kwargs["tools"] = tools
            if request.tool_choice:
                if request.tool_choice == "auto":
                    kwargs["tool_choice"] = {"type": "auto"}
                elif request.tool_choice == "required":
                    kwargs["tool_choice"] = {"type": "any"}

        if request.thinking:
            thinking_fmt = request.thinking.to_claude_format()
            if thinking_fmt:
                kwargs["thinking"] = thinking_fmt
                if "temperature" in kwargs:
                    del kwargs["temperature"]

        if request.extra_headers:
            kwargs["extra_headers"] = request.extra_headers

        async with client.messages.stream(**kwargs) as stream:
            async for event in stream:
                if event.type == "content_block_start":
                    cb = event.content_block
                    if cb.type == "text":
                        pass
                    elif cb.type == "tool_use":
                        yield StreamChunk(
                            channel=StreamChannel.TOOL_USE,
                            content="",
                            tool_calls=[{
                                "id": cb.id,
                                "name": cb.name,
                                "arguments": "",
                            }],
                        )
                elif event.type == "content_block_delta":
                    delta = event.delta
                    if delta.type == "text_delta":
                        yield StreamChunk(
                            channel=StreamChannel.TEXT,
                            content=delta.text,
                        )
                    elif delta.type == "input_json_delta":
                        yield StreamChunk(
                            channel=StreamChannel.TOOL_USE,
                            content="",
                            tool_calls=[{
                                "arguments": delta.partial_json,
                            }],
                        )
                    elif delta.type == "thinking_delta":
                        yield StreamChunk(
                            channel=StreamChannel.REASONING,
                            content=delta.thinking,
                        )
                elif event.type == "message_stop":
                    yield StreamChunk(
                        channel=StreamChannel.TEXT,
                        content="",
                        finish_reason="stop",
                    )
                elif event.type == "message_delta":
                    if event.usage:
                        yield StreamChunk(
                            channel=StreamChannel.TEXT,
                            content="",
                            usage={
                                "output_tokens": event.usage.output_tokens,
                            },
                        )
