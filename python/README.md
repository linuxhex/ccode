# CCODE Agent - Python Layer

Python UI + Reasoning layer for the CCODE AI coding agent, with Rust tool execution via PyO3 FFI.

## Architecture

```
Python Layer (this package)
├── UI          Textual TUI (terminal interface)
├── Reasoning   Multi-provider LLM (OpenAI / Claude / Gemini)
│   ├── sampler     Provider abstraction + streaming
│   ├── prompt      System prompt builder + cache optimization
│   └── memory      4-level compaction + temperature zones
├── Tools       PyO3 FFI (Rust) with Python fallback
└── Kernel      ZMQ client (Topic/Service/Action)

Rust Layer (ccore)
├── Tool execution engine
├── Message bus (ZMTP)
└── Performance-critical components
```

## Quick Start

```bash
# Install dependencies
uv sync

# Build Rust extension (optional, enables native tool execution)
bash scripts/build_rust.sh

# Run the agent
.venv/bin/python -m ccode.cli

# With options
.venv/bin/python -m ccode.cli --project /path/to/project --model claude-sonnet-4-20250514
```

## Configuration

Set at least one API key:

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # Claude
export OPENAI_API_KEY=sk-...          # OpenAI
export GEMINI_API_KEY=AI...           # Gemini
```

## Module Reference

| Module | Purpose |
|--------|---------|
| `ccode.ui` | Textual TUI application |
| `ccode.reasoning.sampler` | Multi-provider LLM integration |
| `ccode.reasoning.prompt` | Prompt building + stable prefix caching |
| `ccode.reasoning.memory` | Compaction pipeline + temperature memory |
| `ccode.tools` | Tool execution (Rust FFI + Python fallback) |
| `ccode.kernel` | ZMQ kernel client (Topic/Service/Action) |
| `ccode.mode` | Runtime mode switching |

## Development

```bash
# Install dev dependencies
uv sync --dev

# Run tests
.venv/bin/python test_full_integration.py

# Type check
.venv/bin/mypy ccode/

# Lint
.venv/bin/ruff check ccode/
```

## Requirements

- Python >= 3.12
- Rust toolchain (for PyO3 extension)
- uv (package manager)
