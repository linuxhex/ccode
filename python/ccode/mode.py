"""Runtime mode switching - supports both Python and Rust modes."""

from enum import Enum
from typing import Optional
from pathlib import Path


class RuntimeMode(str, Enum):
    """Runtime mode for the agent."""
    PYTHON = "python"  # Python UI + Python reasoning + Rust tools
    RUST = "rust"      # Full Rust (existing ccore)


class ModeManager:
    """Manages runtime mode switching."""

    def __init__(self, default_mode: RuntimeMode = RuntimeMode.PYTHON) -> None:
        self.current_mode = default_mode
        self._rust_binary_path: Optional[Path] = None

    def set_mode(self, mode: RuntimeMode) -> None:
        """Switch runtime mode."""
        self.current_mode = mode

    def get_mode(self) -> RuntimeMode:
        """Get current runtime mode."""
        return self.current_mode

    def is_python_mode(self) -> bool:
        """Check if running in Python mode."""
        return self.current_mode == RuntimeMode.PYTHON

    def is_rust_mode(self) -> bool:
        """Check if running in Rust mode."""
        return self.current_mode == RuntimeMode.RUST

    def set_rust_binary_path(self, path: Path) -> None:
        """Set path to Rust binary for Rust mode."""
        self._rust_binary_path = path

    def get_rust_binary_path(self) -> Optional[Path]:
        """Get path to Rust binary."""
        return self._rust_binary_path

    @staticmethod
    def from_string(mode_str: str) -> RuntimeMode:
        """Parse mode from string."""
        mode_str = mode_str.lower().strip()
        if mode_str in ("python", "py"):
            return RuntimeMode.PYTHON
        elif mode_str in ("rust", "rs"):
            return RuntimeMode.RUST
        else:
            raise ValueError(f"Unknown mode: {mode_str}")
