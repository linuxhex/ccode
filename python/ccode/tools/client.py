"""Tool execution client - Calls Rust FFI or kernel."""

import subprocess
from pathlib import Path
from typing import Any, Callable


class ToolExecutor:
    """Executes tools via Rust FFI or fallback to Python."""

    def __init__(self, use_ffi: bool = False) -> None:
        self.use_ffi = use_ffi
        self._ffi_module: Any = None

        if use_ffi:
            try:
                import ccore_engine
                self._ffi_module = ccore_engine
            except ImportError:
                print("Warning: ccore_engine not available, using fallback")
                self.use_ffi = False

    async def execute(
        self,
        tool_id: str,
        params: dict[str, Any],
        progress_callback: Callable[[str], None] | None = None,
    ) -> Any:
        """
        Execute a tool and return the result.

        Args:
            tool_id: Tool identifier (shell, read_file, write_file, etc.)
            params: Tool parameters
            progress_callback: Optional callback for progress updates

        Returns:
            Tool execution result
        """
        if self.use_ffi and self._ffi_module:
            return await self._execute_ffi(tool_id, params)
        else:
            return await self._execute_fallback(tool_id, params)

    async def _execute_ffi(self, tool_id: str, params: dict[str, Any]) -> Any:
        """Execute via Rust FFI."""
        import asyncio
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(
            None,
            lambda: self._ffi_module.execute_tool(tool_id, params),
        )
        return result

    async def _execute_fallback(self, tool_id: str, params: dict[str, Any]) -> Any:
        """Fallback Python implementation for basic tools."""
        if tool_id == "shell":
            return await self._shell_fallback(params)
        elif tool_id == "read_file":
            return await self._read_file_fallback(params)
        elif tool_id == "write_file":
            return await self._write_file_fallback(params)
        elif tool_id == "list_dir":
            return await self._list_dir_fallback(params)
        else:
            raise ValueError(f"Unknown tool: {tool_id}")

    async def _shell_fallback(self, params: dict[str, Any]) -> dict[str, Any]:
        """Fallback shell execution."""
        command = params.get("command", "")
        cwd = params.get("cwd", ".")
        timeout = params.get("timeout", 30)

        try:
            result = subprocess.run(
                command,
                shell=True,
                cwd=cwd,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            return {
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.returncode,
            }
        except subprocess.TimeoutExpired:
            return {"error": "Command timed out", "exit_code": -1}
        except Exception as e:
            return {"error": str(e), "exit_code": -1}

    async def _read_file_fallback(self, params: dict[str, Any]) -> dict[str, Any]:
        """Fallback file reading."""
        path = params.get("path", "")
        try:
            content = Path(path).read_text(encoding="utf-8")
            return {"content": content, "size": len(content)}
        except Exception as e:
            return {"error": str(e)}

    async def _write_file_fallback(self, params: dict[str, Any]) -> dict[str, Any]:
        """Fallback file writing."""
        path = params.get("path", "")
        content = params.get("content", "")
        try:
            Path(path).write_text(content, encoding="utf-8")
            return {"success": True, "size": len(content)}
        except Exception as e:
            return {"error": str(e)}

    async def _list_dir_fallback(self, params: dict[str, Any]) -> dict[str, Any]:
        """Fallback directory listing."""
        path = params.get("path", ".")
        try:
            entries = []
            for entry in Path(path).iterdir():
                entries.append({
                    "name": entry.name,
                    "is_dir": entry.is_dir(),
                    "size": entry.stat().st_size if entry.is_file() else 0,
                })
            return {"entries": entries}
        except Exception as e:
            return {"error": str(e)}
