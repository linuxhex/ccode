"""ZMQ client for ROS-style communication with Rust kernel."""

import asyncio
from collections.abc import AsyncIterator, Callable
from typing import Any
from uuid import UUID, uuid4

import zmq
import zmq.asyncio

from ccode.kernel.protocol import (
    MessageFrame,
    NodeId,
    Topic,
    Service,
    Action,
    ToolCallPayload,
    ToolResultPayload,
)


class KernelClient:
    """Client for communicating with the Rust kernel via ZeroMQ."""

    def __init__(self, kernel_addr: str = "tcp://127.0.0.1:5555") -> None:
        self.kernel_addr = kernel_addr
        self.node_id = NodeId("python-agent")
        self._ctx: zmq.asyncio.Context | None = None
        self._sub: zmq.asyncio.Socket | None = None
        self._dealer: zmq.asyncio.Socket | None = None
        self._pending_requests: dict[str, asyncio.Future[MessageFrame]] = {}
        self._subscriptions: dict[str, list[Callable[[MessageFrame], None]]] = {}

    async def connect(self) -> None:
        """Connect to the kernel."""
        self._ctx = zmq.asyncio.Context()

        self._sub = self._ctx.socket(zmq.SUB)
        self._sub.connect(self.kernel_addr.replace("5555", "5556"))
        self._sub.setsockopt_string(zmq.SUBSCRIBE, "")

        self._dealer = self._ctx.socket(zmq.DEALER)
        self._dealer.setsockopt_string(zmq.IDENTITY, str(self.node_id))
        self._dealer.connect(self.kernel_addr)

        asyncio.create_task(self._receive_loop())

    async def disconnect(self) -> None:
        """Disconnect from the kernel."""
        if self._sub:
            self._sub.close()
        if self._dealer:
            self._dealer.close()
        if self._ctx:
            self._ctx.term()

    async def _receive_loop(self) -> None:
        """Background loop to receive messages."""
        while self._sub and not self._sub.closed:
            try:
                data = await self._sub.recv()
                msg = MessageFrame.from_bytes(data)

                if msg.request_id and str(msg.request_id) in self._pending_requests:
                    self._pending_requests[str(msg.request_id)].set_result(msg)

                if msg.topic in self._subscriptions:
                    for callback in self._subscriptions[msg.topic]:
                        callback(msg)
            except zmq.Again:
                continue
            except Exception:
                continue

    async def publish(self, topic: Topic, payload: dict[str, Any]) -> None:
        """Publish a message to a topic."""
        if not self._dealer:
            raise RuntimeError("Not connected")

        msg = MessageFrame(
            topic=topic.value,
            source=self.node_id,
            payload=payload,
        )
        await self._dealer.send(msg.to_bytes())

    async def call_service(
        self,
        service: Service,
        payload: dict[str, Any],
        timeout: float = 30.0,
    ) -> MessageFrame:
        """Call a service and wait for response."""
        if not self._dealer:
            raise RuntimeError("Not connected")

        request_id = uuid4()
        msg = MessageFrame(
            topic=service.value,
            source=self.node_id,
            payload=payload,
            request_id=request_id,
        )

        future: asyncio.Future[MessageFrame] = asyncio.Future()
        self._pending_requests[str(request_id)] = future

        await self._dealer.send(msg.to_bytes())

        try:
            return await asyncio.wait_for(future, timeout=timeout)
        finally:
            self._pending_requests.pop(str(request_id), None)

    async def execute_tool(
        self,
        tool_id: str,
        params: dict[str, Any],
        timeout_ms: int = 30000,
    ) -> ToolResultPayload:
        """Execute a tool via the kernel."""
        payload = ToolCallPayload(
            tool_id=tool_id,
            params=params,
            timeout_ms=timeout_ms,
        )

        response = await self.call_service(
            Service.TOOLS_EXECUTE,
            {
                "tool_id": payload.tool_id,
                "params": payload.params,
                "timeout_ms": payload.timeout_ms,
            },
        )

        return ToolResultPayload(
            tool_id=tool_id,
            result=response.payload.get("result"),
            error=response.payload.get("error"),
            duration_ms=response.payload.get("duration_ms", 0),
        )

    def subscribe(
        self,
        topic: Topic,
        callback: Callable[[MessageFrame], None],
    ) -> None:
        """Subscribe to a topic."""
        if topic.value not in self._subscriptions:
            self._subscriptions[topic.value] = []
        self._subscriptions[topic.value].append(callback)

    def unsubscribe(
        self,
        topic: Topic,
        callback: Callable[[MessageFrame], None],
    ) -> None:
        """Unsubscribe from a topic."""
        if topic.value in self._subscriptions:
            self._subscriptions[topic.value] = [
                cb for cb in self._subscriptions[topic.value] if cb != callback
            ]
