"""Gemini (Google) provider."""

import json
from collections.abc import AsyncIterator
from typing import Any, Optional

import httpx

from ccode.reasoning.sampler.provider import (
    Provider,
    SampleRequest,
    StreamChunk,
    StreamChannel,
)


class GeminiProvider(Provider):
    """Google Gemini provider via REST API."""

    DEFAULT_BASE_URL = "https://generativelanguage.googleapis.com"

    def __init__(
        self,
        name: str = "gemini",
        api_key: str = "",
        base_url: Optional[str] = None,
    ):
        super().__init__(name, api_key, base_url or self.DEFAULT_BASE_URL)
        self._http: Optional[httpx.AsyncClient] = None

    def _get_http(self) -> httpx.AsyncClient:
        if self._http is None:
            self._http = httpx.AsyncClient(timeout=60.0)
        return self._http

    def _build_contents(self, request: SampleRequest) -> list[dict[str, Any]]:
        contents = []
        for msg in request.messages:
            role = "user" if msg["role"] == "user" else "model"
            parts = []
            if isinstance(msg.get("content"), str):
                parts.append({"text": msg["content"]})
            elif isinstance(msg.get("content"), list):
                for item in msg["content"]:
                    if item.get("type") == "text":
                        parts.append({"text": item["text"]})
            if parts:
                contents.append({"role": role, "parts": parts})
        return contents

    def _build_tools(self, request: SampleRequest) -> Optional[list[dict[str, Any]]]:
        if not request.tools:
            return None
        return [{
            "function_declarations": [
                {
                    "name": t.name,
                    "description": t.description,
                    "parameters": {
                        "type": "object",
                        "properties": t.parameters,
                        "required": t.required,
                    } if t.parameters else {},
                }
                for t in request.tools
            ]
        }]

    async def sample_stream(
        self, request: SampleRequest
    ) -> AsyncIterator[StreamChunk]:
        http = self._get_http()
        model = request.model.replace("gemini-", "")
        url = f"{self.base_url}/v1beta/models/{model}:streamGenerateContent"

        body: dict[str, Any] = {
            "contents": self._build_contents(request),
            "generationConfig": {
                "maxOutputTokens": request.max_tokens,
            },
        }

        if request.system_prompt:
            body["system_instruction"] = {
                "parts": [{"text": request.system_prompt}]
            }
        if request.temperature is not None:
            body["generationConfig"]["temperature"] = request.temperature
        if request.top_p is not None:
            body["generationConfig"]["topP"] = request.top_p

        tools = self._build_tools(request)
        if tools:
            body["tools"] = tools

        if request.thinking:
            thinking_fmt = request.thinking.to_gemini_format()
            if thinking_fmt:
                body["generationConfig"].update(thinking_fmt)

        headers = {"x-goog-api-key": self.api_key}
        if request.extra_headers:
            headers.update(request.extra_headers)

        async with http.stream(
            "POST",
            url,
            json=body,
            headers=headers,
            params={"alt": "sse"},
        ) as response:
            response.raise_for_status()
            async for line in response.aiter_lines():
                if not line.startswith("data: "):
                    continue
                data = line[6:]
                if data.strip() == "[DONE]":
                    break
                try:
                    chunk_data = json.loads(data)
                except json.JSONDecodeError:
                    continue

                for candidate in chunk_data.get("candidates", []):
                    content = candidate.get("content", {})
                    for part in content.get("parts", []):
                        if "text" in part:
                            yield StreamChunk(
                                channel=StreamChannel.TEXT,
                                content=part["text"],
                            )
                        if "functionCall" in part:
                            fc = part["functionCall"]
                            yield StreamChunk(
                                channel=StreamChannel.TOOL_USE,
                                content="",
                                tool_calls=[{
                                    "name": fc["name"],
                                    "arguments": json.dumps(fc.get("args", {})),
                                }],
                            )

                    finish_reason = candidate.get("finishReason")
                    if finish_reason:
                        yield StreamChunk(
                            channel=StreamChannel.TEXT,
                            content="",
                            finish_reason=finish_reason.lower(),
                        )

                if "usageMetadata" in chunk_data:
                    u = chunk_data["usageMetadata"]
                    yield StreamChunk(
                        channel=StreamChannel.TEXT,
                        content="",
                        usage={
                            "prompt_tokens": u.get("promptTokenCount", 0),
                            "completion_tokens": u.get("candidatesTokenCount", 0),
                            "total_tokens": u.get("totalTokenCount", 0),
                        },
                    )
