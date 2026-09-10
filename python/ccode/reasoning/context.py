"""Context management - Conversation history and window management."""

from typing import Any


class ContextManager:
    """Manages conversation context and message history."""

    def __init__(self, max_tokens: int = 128000) -> None:
        self.max_tokens = max_tokens
        self.messages: list[dict[str, Any]] = []
        self.system_prompt = "You are CCODE, an AI coding assistant."

    def add_user_message(self, content: str) -> None:
        """Add a user message to the context."""
        self.messages.append({"role": "user", "content": content})

    def add_assistant_message(self, content: str) -> None:
        """Add an assistant message to the context."""
        self.messages.append({"role": "assistant", "content": content})

    def add_tool_result(self, tool_id: str, result: Any) -> None:
        """Add a tool result to the context."""
        self.messages.append({
            "role": "tool",
            "tool_call_id": tool_id,
            "content": str(result),
        })

    def get_messages(self) -> list[dict[str, Any]]:
        """Get the current message list for API calls."""
        return self.messages.copy()

    def clear(self) -> None:
        """Clear the conversation history."""
        self.messages.clear()

    def estimate_tokens(self) -> int:
        """Estimate the current token count (rough approximation)."""
        total_chars = sum(len(str(m.get("content", ""))) for m in self.messages)
        return total_chars // 4
