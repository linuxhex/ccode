"""CCODE Textual TUI Application."""

from pathlib import Path
from typing import Optional

from textual.app import App, ComposeResult
from textual.containers import Container, Vertical
from textual.widgets import Footer, Header, Input, Static
from textual.reactive import reactive

from ccode.reasoning import ReasoningEngine
from ccode.tools import ToolExecutor
from ccode.mode import ModeManager, RuntimeMode
from ccode.kernel import KernelClient


class OutputPanel(Static):
    """Panel for displaying assistant output."""

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self._buffer: str = ""

    def append_text(self, text: str) -> None:
        """Append text to the output."""
        self._buffer += text
        self.update(self._buffer)


class ToolProgress(Static):
    """Panel for displaying tool execution progress."""

    current_tool: reactive[str] = reactive("")

    def set_tool(self, tool_name: str) -> None:
        """Set the current tool being executed."""
        self.current_tool = tool_name
        self.update(f"[yellow]⏳ Running:[/yellow] {tool_name}")

    def clear_tool(self) -> None:
        """Clear the current tool."""
        self.current_tool = ""
        self.update("")


class CCODEApp(App):
    """CCODE Agent TUI Application."""

    CSS = """
    Screen {
        layout: vertical;
    }

    #main-container {
        height: 1fr;
    }

    #output-panel {
        height: 1fr;
        border: solid green;
        padding: 1;
    }

    #tool-progress {
        height: 1;
        padding: 0 1;
    }

    #input-container {
        height: 3;
        dock: bottom;
    }

    #user-input {
        width: 100%;
    }
    """

    BINDINGS = [
        ("ctrl+c", "quit", "Quit"),
        ("ctrl+l", "clear", "Clear"),
    ]

    def __init__(
        self,
        project_dir: str = ".",
        model: str = "claude-sonnet-4-20250514",
        verbose: bool = False,
        mode_manager: Optional[ModeManager] = None,
        kernel_addr: Optional[str] = None,
    ) -> None:
        super().__init__()
        self.project_dir = Path(project_dir).resolve()
        self.model = model
        self.verbose = verbose
        self.mode_manager = mode_manager or ModeManager()
        self.kernel_addr = kernel_addr
        self.reasoning: ReasoningEngine | None = None
        self.tools: ToolExecutor | None = None
        self.kernel: KernelClient | None = None

    def compose(self) -> ComposeResult:
        """Compose the UI layout."""
        yield Header()
        with Container(id="main-container"):
            yield OutputPanel(id="output-panel")
            yield ToolProgress(id="tool-progress")
        with Container(id="input-container"):
            yield Input(placeholder="Ask CCODE anything...", id="user-input")
        yield Footer()

    async def on_mount(self) -> None:
        """Initialize the app after mount."""
        self.reasoning = ReasoningEngine(model=self.model)

        kernel_client = None
        if self.kernel_addr:
            try:
                kernel_client = KernelClient(kernel_addr=self.kernel_addr)
                await kernel_client.connect()
                self.kernel = kernel_client
            except Exception:
                kernel_client = None

        self.tools = ToolExecutor(use_ffi=True, kernel_client=kernel_client)

        output = self.query_one("#output-panel", OutputPanel)
        backend = self.tools.backend
        status = f"[dim]CCODE Agent ready. Project: {self.project_dir} | Backend: {backend}[/dim]"
        output.update(status)

        input_widget = self.query_one("#user-input", Input)
        input_widget.focus()

    async def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle user input submission."""
        if not event.value.strip():
            return

        user_message = event.value
        event.input.clear()

        output = self.query_one("#output-panel", OutputPanel)
        output.append_text(f"\n\n[bold blue]You>[/bold blue] {user_message}")
        output.append_text("\n[bold green]CCODE>[/bold green] ")

        progress = self.query_one("#tool-progress", ToolProgress)

        if self.reasoning and self.tools:
            async for chunk in self.reasoning.chat(
                user_message,
                project_dir=self.project_dir,
                tool_executor=self.tools,
                progress_callback=progress.set_tool,
            ):
                output.append_text(chunk)

            progress.clear_tool()

    def action_clear(self) -> None:
        """Clear the output panel."""
        output = self.query_one("#output-panel", OutputPanel)
        output._buffer = ""
        output.update("")

    async def on_shutdown(self) -> None:
        """Clean up on app shutdown."""
        if self.kernel:
            await self.kernel.disconnect()
