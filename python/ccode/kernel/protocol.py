"""Message protocol - ROS-style Topic/Service/Action."""

from dataclasses import dataclass, field
from enum import Enum
from typing import Any
from uuid import UUID, uuid4


class Topic(str, Enum):
    """Pub/Sub topics for async event streaming."""

    MODEL_STREAM = "model/stream"
    TOOL_PROGRESS = "tools/progress"
    AGENT_EVENTS = "agent/events"
    UI_COMMANDS = "ui/commands"
    KERNEL_HEALTH = "kernel/health"


class Service(str, Enum):
    """Request/Reply services for sync operations."""

    TOOLS_EXECUTE = "tools/execute"
    MODEL_CHAT = "model/chat"
    CONTEXT_GET = "context/get"
    SESSION_SAVE = "session/save"
    CONFIG_GET = "config/get"


class Action(str, Enum):
    """Long-running actions with feedback."""

    AGENT_RUN = "agent/run"
    TOOLS_SHELL = "tools/shell"
    TOOLS_COMPILE = "tools/compile"


@dataclass
class NodeId:
    """Unique identifier for a node in the system."""

    name: str
    instance_id: UUID = field(default_factory=uuid4)

    def __str__(self) -> str:
        return f"{self.name}:{self.instance_id.hex[:8]}"


@dataclass
class MessageFrame:
    """Unified message frame for all communication."""

    topic: str
    source: NodeId
    payload: dict[str, Any]
    msg_id: UUID = field(default_factory=uuid4)
    target: NodeId | None = None
    timestamp: float = field(default_factory=lambda: __import__("time").time())
    request_id: UUID | None = None

    def to_bytes(self) -> bytes:
        """Serialize to MessagePack bytes."""
        import msgpack

        data = {
            "msg_id": str(self.msg_id),
            "topic": self.topic,
            "source": str(self.source),
            "target": str(self.target) if self.target else None,
            "payload": self.payload,
            "timestamp": self.timestamp,
            "request_id": str(self.request_id) if self.request_id else None,
        }
        return msgpack.packb(data, use_bin_type=True)

    @classmethod
    def from_bytes(cls, data: bytes) -> "MessageFrame":
        """Deserialize from MessagePack bytes."""
        import msgpack

        raw = msgpack.unpackb(data, raw=False)
        return cls(
            msg_id=UUID(raw["msg_id"]),
            topic=raw["topic"],
            source=NodeId(raw["source"].split(":")[0]),
            target=NodeId(raw["target"].split(":")[0]) if raw.get("target") else None,
            payload=raw["payload"],
            timestamp=raw["timestamp"],
            request_id=UUID(raw["request_id"]) if raw.get("request_id") else None,
        )


@dataclass
class ToolCallPayload:
    """Payload for tool execution requests."""

    tool_id: str
    params: dict[str, Any]
    timeout_ms: int = 30000


@dataclass
class ToolResultPayload:
    """Payload for tool execution results."""

    tool_id: str
    result: Any = None
    error: str | None = None
    duration_ms: float = 0

    @property
    def is_success(self) -> bool:
        return self.error is None


@dataclass
class TextDeltaPayload:
    """Payload for streaming text deltas."""

    content: str
    finish_reason: str | None = None


@dataclass
class ToolProgressPayload:
    """Payload for tool execution progress."""

    tool_id: str
    progress: float
    message: str = ""


@dataclass
class ActionGoalPayload:
    """Payload for action goals."""

    action_id: str
    goal: dict[str, Any]


@dataclass
class ActionFeedbackPayload:
    """Payload for action feedback."""

    action_id: str
    feedback: dict[str, Any]


@dataclass
class ActionResultPayload:
    """Payload for action results."""

    action_id: str
    result: dict[str, Any]
    success: bool = True
