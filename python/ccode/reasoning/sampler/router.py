"""Provider router - routes requests to the appropriate provider."""

import asyncio
import time
from typing import Optional

from ccode.reasoning.sampler.provider import (
    Provider,
    ProviderConfig,
    SampleRequest,
    SampleResponse,
    StreamChunk,
)
from ccode.reasoning.sampler.openai import OpenAIProvider
from ccode.reasoning.sampler.claude import ClaudeProvider
from ccode.reasoning.sampler.gemini import GeminiProvider


class TokenBucket:
    """Simple token bucket rate limiter."""

    def __init__(self, rate: float, capacity: int):
        self.rate = rate
        self.capacity = capacity
        self.tokens = float(capacity)
        self.last_refill = time.monotonic()

    async def acquire(self) -> None:
        while True:
            now = time.monotonic()
            elapsed = now - self.last_refill
            self.tokens = min(self.capacity, self.tokens + elapsed * self.rate)
            self.last_refill = now

            if self.tokens >= 1:
                self.tokens -= 1
                return
            await asyncio.sleep(1.0 / self.rate)


class ProviderRouter:
    """Routes sampling requests to the appropriate provider."""

    def __init__(self) -> None:
        self._providers: dict[str, Provider] = {}
        self._configs: dict[str, ProviderConfig] = {}
        self._fallback_chains: dict[str, list[str]] = {}
        self._rate_limiters: dict[str, TokenBucket] = {}

    def register_provider(
        self,
        config: ProviderConfig,
        rate_limit: Optional[float] = None,
    ) -> None:
        provider = self._create_provider(config)
        self._providers[config.name] = provider
        self._configs[config.name] = config

        if rate_limit:
            self._rate_limiters[config.name] = TokenBucket(rate=rate_limit, capacity=10)

        for model in config.models:
            self._fallback_chains[model] = [config.name]

    def _create_provider(self, config: ProviderConfig) -> Provider:
        name_lower = config.name.lower()
        if "claude" in name_lower or "anthropic" in name_lower:
            return ClaudeProvider(
                name=config.name,
                api_key=config.api_key,
                base_url=config.base_url,
            )
        elif "gemini" in name_lower or "google" in name_lower:
            return GeminiProvider(
                name=config.name,
                api_key=config.api_key,
                base_url=config.base_url,
            )
        else:
            return OpenAIProvider(
                name=config.name,
                api_key=config.api_key,
                base_url=config.base_url,
            )

    def set_fallback_chain(self, model: str, providers: list[str]) -> None:
        self._fallback_chains[model] = providers

    def resolve_provider(self, model: str) -> tuple[str, Provider]:
        if model in self._fallback_chains:
            for provider_name in self._fallback_chains[model]:
                if provider_name in self._providers:
                    return provider_name, self._providers[provider_name]

        for provider_name, config in self._configs.items():
            for m in config.models:
                if model.startswith(m) or m.startswith(model.split("/")[0]):
                    return provider_name, self._providers[provider_name]

        if "default" in self._providers:
            return "default", self._providers["default"]

        if self._providers:
            name = next(iter(self._providers))
            return name, self._providers[name]

        raise ValueError(f"No provider available for model: {model}")

    async def sample(self, request: SampleRequest) -> SampleResponse:
        provider_name, provider = self.resolve_provider(request.model)

        if provider_name in self._rate_limiters:
            await self._rate_limiters[provider_name].acquire()

        fallback_chain = self._fallback_chains.get(request.model, [provider_name])

        last_error: Optional[Exception] = None
        for name in fallback_chain:
            if name not in self._providers:
                continue
            p = self._providers[name]
            try:
                return await p.sample(request)
            except Exception as e:
                last_error = e
                continue

        raise last_error or RuntimeError("All providers failed")

    async def sample_stream(self, request: SampleRequest):
        provider_name, provider = self.resolve_provider(request.model)

        if provider_name in self._rate_limiters:
            await self._rate_limiters[provider_name].acquire()

        async for chunk in provider.sample_stream(request):
            yield chunk

    def get_provider(self, name: str) -> Optional[Provider]:
        return self._providers.get(name)

    def list_providers(self) -> list[str]:
        return list(self._providers.keys())
