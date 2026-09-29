#!/usr/bin/env python3
"""Test the sampler module."""

import asyncio
from ccode.reasoning.sampler import (
    ProviderRouter,
    ProviderConfig,
    SampleRequest,
    ThinkingConfig,
    StreamChannel,
)


def test_thinking_config() -> None:
    """Test ThinkingConfig format conversion."""
    print("Testing ThinkingConfig...")

    # OpenAI format
    tc = ThinkingConfig(enabled=True, reasoning_effort="high")
    fmt = tc.to_openai_format()
    assert fmt == {"reasoning_effort": "high"}, f"Got {fmt}"
    print("  ✓ OpenAI format")

    # Claude format
    tc = ThinkingConfig(enabled=True, budget_tokens=4096)
    fmt = tc.to_claude_format()
    assert fmt == {"type": "enabled", "budget_tokens": 4096}, f"Got {fmt}"
    print("  ✓ Claude format")

    # Gemini format
    tc = ThinkingConfig(enabled=True, budget_tokens=1024)
    fmt = tc.to_gemini_format()
    assert fmt == {"thinking_config": {"thinking_budget": 1024}}, f"Got {fmt}"
    print("  ✓ Gemini format")

    # Disabled
    tc = ThinkingConfig(enabled=False)
    assert tc.to_openai_format() == {}
    assert tc.to_claude_format() == {}
    assert tc.to_gemini_format() == {}
    print("  ✓ Disabled returns empty")


def test_provider_router() -> None:
    """Test ProviderRouter resolution."""
    print("\nTesting ProviderRouter...")

    router = ProviderRouter()

    router.register_provider(ProviderConfig(
        name="claude",
        api_key="test-key",
        models=["claude-sonnet-4-20250514", "claude-3-5-sonnet"],
    ))
    router.register_provider(ProviderConfig(
        name="openai",
        api_key="test-key",
        models=["gpt-4o", "o1"],
    ))

    # Test resolution
    name, provider = router.resolve_provider("claude-sonnet-4-20250514")
    assert name == "claude", f"Expected claude, got {name}"
    print("  ✓ Claude model resolved")

    name, provider = router.resolve_provider("gpt-4o")
    assert name == "openai", f"Expected openai, got {name}"
    print("  ✓ OpenAI model resolved")

    # Test listing
    providers = router.list_providers()
    assert "claude" in providers
    assert "openai" in providers
    print(f"  ✓ Providers: {providers}")


def test_sample_request() -> None:
    """Test SampleRequest construction."""
    print("\nTesting SampleRequest...")

    request = SampleRequest(
        model="claude-sonnet-4-20250514",
        messages=[{"role": "user", "content": "Hello"}],
        max_tokens=4096,
        thinking=ThinkingConfig(enabled=True, budget_tokens=2048),
    )

    assert request.model == "claude-sonnet-4-20250514"
    assert len(request.messages) == 1
    assert request.thinking is not None
    assert request.thinking.enabled
    print("  ✓ Request constructed")


async def main() -> None:
    print("=" * 50)
    print("Sampler Module Test")
    print("=" * 50)

    test_thinking_config()
    test_provider_router()
    test_sample_request()

    print("\n" + "=" * 50)
    print("All tests passed!")
    print("=" * 50)


if __name__ == "__main__":
    asyncio.run(main())
