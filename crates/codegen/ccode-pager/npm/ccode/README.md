# Ccode

Bring Ccode into your terminal. Fast, flicker-free CLI built for plans, subagents, and parallel work.

**[Homepage](https://ccode.dev/cli)** | **[Documentation](https://docs.ccode.dev/build/overview)**

## Install

```bash
curl -fsSL https://ccode.dev/cli/install.sh | bash
```

Or install with npm:

```bash
npm i -g @ccode-official/ccode
```

## Get Started

```bash
# Launch the interactive TUI
ccode

# Run a single task
ccode -p "Explain this codebase"
```

On first launch, Ccode opens your browser to authenticate. For CI or headless environments, use an API key from [console.ccode.dev](https://console.ccode.dev):

```bash
export CCODE_API_KEY="ccode-..."
```

## Update

```bash
ccode update
```

Or if installed via npm:

```bash
npm i -g @ccode-official/ccode@latest
```

## Supported Platforms

| Platform | Architecture |
|---|---|
| macOS | Apple Silicon (arm64) |
| Linux | x86_64, arm64 |
| Windows | x86_64 |

## Documentation

For full documentation including configuration, MCP servers, custom models, headless mode, agent mode, and more, visit [docs.ccode.dev/build/overview](https://docs.ccode.dev/build/overview).

## Feedback

Run `/feedback` inside Ccode to report issues or send feedback directly.
