#!/usr/bin/env python3
"""Full integration test for CCODE Agent three-layer architecture."""

import asyncio
from pathlib import Path

# Test all module imports
def test_imports() -> None:
    """Test all module imports work."""
    print("Testing imports...")

    # Sampler
    from ccode.reasoning.sampler import (
        Provider, ProviderConfig, SampleRequest, SampleResponse,
        StreamChunk, StreamChannel, ThinkingConfig,
        ProviderRouter, OpenAIProvider, ClaudeProvider, GeminiProvider,
    )
    print("  ✓ sampler")

    # Prompt
    from ccode.reasoning.prompt import PromptBuilder, StablePrefix, ProjectContext
    print("  ✓ prompt")

    # Memory
    from ccode.reasoning.memory import (
        CompactionPipeline, CompactionLevel, SummaryValidator,
        TemperatureZone, TemperatureManager,
    )
    print("  ✓ memory")

    # Engine
    from ccode.reasoning import ReasoningEngine, ContextManager
    print("  ✓ engine")

    # Tools
    from ccode.tools import ToolExecutor
    print("  ✓ tools")

    # Kernel
    from ccode.kernel import KernelClient, Topic, Service, Action
    print("  ✓ kernel")

    # Mode
    from ccode.mode import RuntimeMode, ModeManager
    print("  ✓ mode")

    # UI
    from ccode.ui import CCODEApp
    print("  ✓ ui")


def test_prompt_builder() -> None:
    """Test prompt builder."""
    print("\nTesting PromptBuilder...")
    from ccode.reasoning.prompt import PromptBuilder, ProjectContext

    builder = PromptBuilder()
    builder.set_project_dir(Path("."))

    prompt = builder.build()
    assert "CCODE" in prompt
    print(f"  ✓ Built prompt ({len(prompt)} chars)")

    # Test stable prefix
    assert builder.stable_prefix.is_locked()
    h = builder.get_stable_prefix_hash()
    assert h is not None
    print(f"  ✓ Stable prefix hash: {h}")


def test_compaction() -> None:
    """Test compaction pipeline."""
    print("\nTesting CompactionPipeline...")
    from ccode.reasoning.memory import CompactionPipeline, SummaryValidator

    pipeline = CompactionPipeline()

    # Test snip
    long_output = "x" * 30000
    snipped = pipeline.snip_tool_output(long_output)
    assert len(snipped) < len(long_output)
    assert "omitted" in snipped
    print(f"  ✓ Snip: {len(long_output)} → {len(snipped)} chars")

    # Test micro compact
    messages = [{"role": "tool", "content": f"result {i}"} for i in range(20)]
    messages.append({"role": "user", "content": "latest"})
    compacted, removed = pipeline.micro_compact(messages, age_threshold=5)
    assert removed > 0
    print(f"  ✓ Micro compact: removed {removed} messages")

    # Test summary validator
    good_summary = """
## Primary Request
User asked for help

## Key Concepts
Python, Rust

## Files
test.py

## Errors
None

## Problem Solving
Solved

## User Messages
User is happy

## Pending Tasks
None

## Current Work
Testing

## Next Step
Deploy
"""
    is_valid, found, missing = SummaryValidator.validate(good_summary)
    assert is_valid
    assert len(found) >= 7
    print(f"  ✓ Summary validation: {len(found)}/9 sections found")

    bad_summary = "This is a short summary without structure."
    assert SummaryValidator.is_degenerate(bad_summary)
    print("  ✓ Degenerate detection works")


def test_temperature() -> None:
    """Test temperature manager."""
    print("\nTesting TemperatureManager...")
    from ccode.reasoning.memory import TemperatureManager
    from ccode.reasoning.memory.temperature import MemoryItem

    manager = TemperatureManager(hot_budget=100, warm_budget=200)

    # Add items
    for i in range(10):
        item = MemoryItem(content=f"Item {i} " * 20)
        manager.add(item)

    stats = manager.get_stats()
    print(f"  ✓ Hot: {stats['hot']['items']} items, {stats['hot']['tokens']} tokens")
    print(f"  ✓ Warm: {stats['warm']['items']} items, {stats['warm']['tokens']} tokens")
    print(f"  ✓ Cold: {stats['cold']['items']} items, {stats['cold']['tokens']} tokens")


def test_mode_switching() -> None:
    """Test mode switching."""
    print("\nTesting ModeManager...")
    from ccode.mode import RuntimeMode, ModeManager

    manager = ModeManager()
    assert manager.is_python_mode()
    print("  ✓ Default: Python mode")

    manager.set_mode(RuntimeMode.RUST)
    assert manager.is_rust_mode()
    print("  ✓ Switched to Rust mode")

    parsed = ModeManager.from_string("py")
    assert parsed == RuntimeMode.PYTHON
    print("  ✓ Parse 'py' → Python")


async def test_tool_executor() -> None:
    """Test tool executor."""
    print("\nTesting ToolExecutor...")
    from ccode.tools import ToolExecutor

    executor = ToolExecutor(use_ffi=False)

    result = await executor.execute("shell", {"command": "echo 'integration test'"})
    assert result["exit_code"] == 0
    assert "integration test" in result["stdout"]
    print("  ✓ Shell execution works")


async def main() -> None:
    print("=" * 60)
    print("CCODE Agent Full Integration Test")
    print("=" * 60)

    test_imports()
    test_prompt_builder()
    test_compaction()
    test_temperature()
    test_mode_switching()
    await test_tool_executor()

    print("\n" + "=" * 60)
    print("All integration tests passed!")
    print("=" * 60)
    print("\nArchitecture summary:")
    print("  Python: UI (Textual) + Reasoning (sampler/prompt/memory)")
    print("  Rust:   Tool execution + ZMQ kernel (via PyO3 FFI)")
    print("  Node:   npm distribution shell")


if __name__ == "__main__":
    asyncio.run(main())
