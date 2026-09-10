"""Temperature-based memory zone management.

Three temperature zones:
- Hot: Active working memory, always in context
- Warm: Recent context, included when space allows
- Cold: Older context, only retrieved on demand
"""

from dataclasses import dataclass, field
from enum import Enum
from typing import Any
import time


class TemperatureZone(str, Enum):
    """Memory temperature zones."""
    HOT = "hot"
    WARM = "warm"
    COLD = "cold"


@dataclass
class MemoryItem:
    """A memory item with temperature tracking."""
    content: str
    zone: TemperatureZone = TemperatureZone.HOT
    created_at: float = field(default_factory=time.time)
    last_accessed: float = field(default_factory=time.time)
    access_count: int = 0
    token_estimate: int = 0

    def touch(self) -> None:
        """Update access time and count."""
        self.last_accessed = time.time()
        self.access_count += 1


@dataclass
class TemperatureManager:
    """Manages temperature zones for memory items."""

    hot_budget: int = 32000  # tokens
    warm_budget: int = 64000  # tokens
    cold_budget: int = 128000  # tokens

    hot_items: list[MemoryItem] = field(default_factory=list)
    warm_items: list[MemoryItem] = field(default_factory=list)
    cold_items: list[MemoryItem] = field(default_factory=list)

    def add(self, item: MemoryItem) -> None:
        """Add item to hot zone."""
        item.zone = TemperatureZone.HOT
        item.token_estimate = len(item.content) // 4
        self.hot_items.append(item)
        self._enforce_budgets()

    def _enforce_budgets(self) -> None:
        """Enforce temperature zone budgets by demoting items."""
        # Demote from hot to warm
        while self._zone_tokens(self.hot_items) > self.hot_budget and self.hot_items:
            item = self._oldest_item(self.hot_items)
            self.hot_items.remove(item)
            item.zone = TemperatureZone.WARM
            self.warm_items.append(item)

        # Demote from warm to cold
        while self._zone_tokens(self.warm_items) > self.warm_budget and self.warm_items:
            item = self._oldest_item(self.warm_items)
            self.warm_items.remove(item)
            item.zone = TemperatureZone.COLD
            self.cold_items.append(item)

        # Evict from cold
        while self._zone_tokens(self.cold_items) > self.cold_budget and self.cold_items:
            self._oldest_item(self.cold_items)
            self.cold_items.pop(0)

    def _zone_tokens(self, items: list[MemoryItem]) -> int:
        return sum(item.token_estimate for item in items)

    def _oldest_item(self, items: list[MemoryItem]) -> MemoryItem:
        return min(items, key=lambda x: x.last_accessed)

    def get_hot_context(self) -> str:
        """Get all hot zone content."""
        return "\n".join(item.content for item in self.hot_items)

    def get_warm_context(self, max_tokens: int = 16000) -> str:
        """Get warm zone content up to budget."""
        parts = []
        total = 0
        for item in sorted(self.warm_items, key=lambda x: x.last_accessed, reverse=True):
            if total + item.token_estimate > max_tokens:
                break
            parts.append(item.content)
            total += item.token_estimate
        return "\n".join(parts)

    def retrieve_cold(self, query: str, max_results: int = 3) -> list[MemoryItem]:
        """Retrieve relevant cold items by simple keyword matching."""
        query_words = set(query.lower().split())
        scored = []

        for item in self.cold_items:
            content_words = set(item.content.lower().split())
            overlap = len(query_words & content_words)
            if overlap > 0:
                scored.append((overlap, item))

        scored.sort(reverse=True, key=lambda x: x[0])
        return [item for _, item in scored[:max_results]]

    def get_stats(self) -> dict[str, Any]:
        """Get temperature zone statistics."""
        return {
            "hot": {
                "items": len(self.hot_items),
                "tokens": self._zone_tokens(self.hot_items),
                "budget": self.hot_budget,
            },
            "warm": {
                "items": len(self.warm_items),
                "tokens": self._zone_tokens(self.warm_items),
                "budget": self.warm_budget,
            },
            "cold": {
                "items": len(self.cold_items),
                "tokens": self._zone_tokens(self.cold_items),
                "budget": self.cold_budget,
            },
        }
