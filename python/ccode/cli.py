"""CLI entry point for CCODE Agent."""

from typing import Optional

import click

from ccode.ui.app import CCODEApp
from ccode.mode import RuntimeMode, ModeManager


@click.command()
@click.option("--project", "-p", default=".", help="Project directory")
@click.option("--model", "-m", default="claude-sonnet-4-20250514", help="Model to use")
@click.option("--kernel", "-k", default=None,
              help="Rust kernel ZMQ address (e.g. tcp://127.0.0.1:5555)")
@click.option("--verbose", "-v", is_flag=True, help="Enable verbose logging")
def main(project: str, model: str, kernel: Optional[str], verbose: bool) -> None:
    """CCODE Agent - AI coding assistant."""
    app = CCODEApp(
        project_dir=project,
        model=model,
        verbose=verbose,
        kernel_addr=kernel,
    )
    app.run()


if __name__ == "__main__":
    main()
