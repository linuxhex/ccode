"""CLI entry point for CCODE Agent."""

import click

from ccode.ui.app import CCODEApp
from ccode.mode import RuntimeMode, ModeManager


@click.command()
@click.option("--project", "-p", default=".", help="Project directory")
@click.option("--model", "-m", default="claude-sonnet-4-20250514", help="Model to use")
@click.option("--mode", type=click.Choice(["python", "rust"]), default="python",
              help="Runtime mode: python (new) or rust (legacy)")
@click.option("--verbose", "-v", is_flag=True, help="Enable verbose logging")
def main(project: str, model: str, mode: str, verbose: bool) -> None:
    """CCODE Agent - AI coding assistant."""
    mode_manager = ModeManager(ModeManager.from_string(mode))

    if mode_manager.is_rust_mode():
        click.echo("Rust mode: launching legacy ccore binary...")
        click.echo("(Not yet implemented - use Python mode)")
        return

    app = CCODEApp(
        project_dir=project,
        model=model,
        verbose=verbose,
        mode_manager=mode_manager,
    )
    app.run()


if __name__ == "__main__":
    main()
