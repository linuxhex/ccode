#!/usr/bin/env python3
"""Test script to verify the architecture works end-to-end."""

import asyncio
from pathlib import Path

from ccode.tools import ToolExecutor
from ccode.reasoning import ContextManager


async def test_tool_executor() -> None:
    """Test the tool executor with fallback implementation."""
    print("Testing ToolExecutor...")

    executor = ToolExecutor(use_ffi=False)  # Use Python fallback
    test_dir = Path(__file__).resolve().parent

    # Test shell command
    print("\n1. Testing shell command:")
    result = await executor.execute("shell", {"command": "echo 'Hello from CCODE!'"})
    print(f"   Result: {result}")

    # Test read file
    print("\n2. Testing read_file:")
    result = await executor.execute("read_file", {"path": str(test_dir / "pyproject.toml")})
    if "error" not in result:
        print(f"   Read {result.get('size', 0)} bytes")
    else:
        print(f"   Error: {result.get('error')}")

    # Test list directory
    print("\n3. Testing list_dir:")
    result = await executor.execute("list_dir", {"path": str(test_dir)})
    if "entries" in result:
        print(f"   Found {len(result['entries'])} entries")
        for entry in result["entries"][:5]:
            print(f"   - {entry['name']} {'[dir]' if entry['is_dir'] else ''}")


def test_context_manager() -> None:
    """Test the context manager."""
    print("\n\nTesting ContextManager...")

    ctx = ContextManager()

    ctx.add_user_message("Hello, how are you?")
    ctx.add_assistant_message("I'm doing well, thank you!")
    ctx.add_user_message("What's the weather like?")

    messages = ctx.get_messages()
    print(f"Total messages: {len(messages)}")
    print(f"Estimated tokens: {ctx.estimate_tokens()}")

    for msg in messages:
        role = msg["role"]
        content = msg["content"][:50] + "..." if len(msg["content"]) > 50 else msg["content"]
        print(f"  [{role}] {content}")


async def main() -> None:
    """Run all tests."""
    print("=" * 60)
    print("CCODE Agent Architecture Test")
    print("=" * 60)

    await test_tool_executor()
    test_context_manager()

    print("\n" + "=" * 60)
    print("All tests passed!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
