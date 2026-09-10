"""Stable prefix management for prompt cache optimization.

Locks the system prompt prefix after first use to maximize API prompt
cache hit rate across turns.
"""

from typing import Optional
import hashlib


class StablePrefix:
    """Manages stable prefix for prompt caching."""

    def __init__(self) -> None:
        self._locked_prefix: Optional[str] = None
        self._prefix_hash: Optional[str] = None

    def lock(self, prefix: str) -> None:
        """Lock the stable prefix after first use."""
        if self._locked_prefix is None:
            self._locked_prefix = prefix
            self._prefix_hash = self._compute_hash(prefix)

    def is_locked(self) -> bool:
        """Check if prefix is locked."""
        return self._locked_prefix is not None

    def get(self) -> Optional[str]:
        """Get the locked prefix."""
        return self._locked_prefix

    def get_hash(self) -> Optional[str]:
        """Get the hash of the locked prefix."""
        return self._prefix_hash

    def verify(self, prefix: str) -> bool:
        """Verify if a prefix matches the locked prefix."""
        if self._locked_prefix is None:
            return True
        return prefix == self._locked_prefix

    def reset(self) -> None:
        """Reset the locked prefix."""
        self._locked_prefix = None
        self._prefix_hash = None

    def _compute_hash(self, text: str) -> str:
        """Compute hash for cache key."""
        return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]
