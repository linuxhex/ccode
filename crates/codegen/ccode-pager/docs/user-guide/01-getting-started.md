# Getting Started

Ccode Build is a terminal-based AI coding assistant from SpaceCcode. It runs as a TUI (Terminal User Interface) that understands your codebase, executes shell commands, edits files, searches the web, and manages tasks.

You can use it interactively as a full-screen TUI, run it headlessly for scripting and CI/CD, or integrate it into editors via the Agent Client Protocol (ACP).

---

## Installation

Install the latest stable release (macOS, Linux, or Windows via Git Bash):

```bash
curl -fsSL https://ccode.dev/cli/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://ccode.dev/cli/install.sh | bash -s 0.1.42
```

On **Windows (PowerShell)**, use the native PowerShell installer:

```powershell
irm https://ccode.dev/cli/install.ps1 | iex
```

Install a specific version:

```powershell
$env:CCODE_VERSION="0.1.42"; irm https://ccode.dev/cli/install.ps1 | iex
```

The PowerShell installer automatically adds `%USERPROFILE%\.ccode\bin` to your User PATH. Alternatively, install via [Git for Windows](https://gitforwindows.org/) (Git Bash) or MSYS2 using the bash script above. WSL users get the Linux binary automatically.

Verify the installation:

```bash
ccode --version
```

Update to the latest version at any time:

```bash
ccode update
```

---

## First Launch

Start Ccode by running:

```bash
ccode
```

On first launch, Ccode opens your browser to authenticate with ccode.dev. After you sign in, Ccode stores your credentials in `~/.ccode/auth.json`, where they persist across sessions. Ccode refreshes your credentials automatically and prompts you to sign in again when they can no longer be renewed.

If you prefer API key authentication (e.g., for CI/CD or environments without a browser), set the `CCODE_API_KEY` environment variable instead:

```bash
export CCODE_API_KEY="ccode-..."
ccode
```

See [Authentication](02-authentication.md) for the full set of auth options including OIDC, external auth providers, and device code flow.

---

## Basic Interaction

Once authenticated, Ccode presents a full-screen TUI with two main areas:

- **Scrollback** -- the conversation history showing your prompts, Ccode's responses, tool calls, file edits, and more.
- **Prompt** -- the input area at the bottom where you type messages.

Type a message and press `Enter` to send it. Ccode reads files, runs commands, and edits code as needed. Each tool run streams into the scrollback in real time.

Press `Tab` to move focus between the prompt and the scrollback. While a turn is running, `Esc` cancels it (the exception is fullscreen vim scrollback mode, where mid-turn `Esc` is a no-op; minimal mode cancels even with vim on); `Ctrl+C` cancels once the composer is empty — with a draft, the first press only clears it. Idle, press `Esc` twice within 800ms to clear a non-empty prompt, or (with an empty prompt and conversation messages) to open rewind — see [Keyboard Shortcuts](03-keyboard-shortcuts.md#escape). With the scrollback focused, use the arrow keys to select entries and to collapse or expand them. To navigate with `j`/`k` and fold with `h`/`l` instead, enable Vim mode.

### File References

Use `@` in your prompt to attach files:

```
@src/main.rs              # Attach a file
@src/main.rs:10-50        # Attach lines 10-50
@src/                     # Browse a directory
```

The `@` operator opens a fuzzy file picker. By default it respects `.gitignore` and hides dotfiles. Prefix with `!` to search hidden files:

```
@!.github                 # Search hidden files
@!.env                    # Attach a .env file
```

### Permissions

By default, Ccode asks for permission before executing shell commands or editing files. You can approve individually or toggle always-approve mode:

- Press `Ctrl+O` to toggle always-approve mode
- Use the `--yolo` flag at launch: `ccode --yolo`
- Type `/always-approve` in the prompt to toggle the mode

---

## Key Concepts

### Sessions

Every conversation is a **session**. Sessions are automatically saved to `~/.ccode/sessions/` and can be resumed later. Each session tracks the full conversation history, tool calls, file edits, and task state.

- Start a new session: `Ctrl+N` or `/new`
- Resume a previous session: `/resume` in the TUI, or `--resume <ID>` from the CLI
- Continue the most recent session: `ccode -c`

### Scrollback

The scrollback is the main display area. It shows:

- **User prompts** -- your messages, rendered as sticky headers
- **Agent messages** -- Ccode's responses with full markdown rendering and syntax highlighting
- **Thinking blocks** -- Ccode's reasoning process (collapsible)
- **Tool calls** -- file edits (with inline diffs), command executions, search results, and more
- **Task lists** -- TODO items tracking progress

Collapse or expand the selected entry with the `Left`/`Right` arrow keys (or `h`/`l` and `e` in Vim mode). In Vim mode, press `y` to copy its content and `Y` to copy its metadata (for example, the command that ran). Press `Enter` to open it in the fullscreen viewer (in any mode).

### Tools

Ccode has built-in tools for:

| Tool | Description |
|------|-------------|
| `read_file` / `search_replace` | Read and edit files with line-precise changes |
| `grep` | Regex search across your codebase (powered by ripgrep) |
| `list_dir` | List directory contents |
| `run_terminal_command` | Execute shell commands |
| `web_search` / `web_fetch` | Search the web and fetch URLs |
| `todo_write` | Create and manage task lists |
| `spawn_subagent` | Spawn parallel subagent sessions |
| `memory_search` | Search cross-session memory |

Tools can be extended with [MCP servers](05-configuration.md#mcp-servers) for integrations like GitHub, databases, and more.

### Slash Commands

Type `/` in the prompt to access commands. These provide quick actions without writing a full prompt:

```
/model ccode-build                 # Switch model
/compact                          # Compress conversation history
/always-approve                   # Toggle always-approve mode
/new                              # Start a new session
```

See [Slash Commands](04-slash-commands.md) for the complete reference.

---

## Common Launch Options

```bash
# Launch the interactive TUI and submit an initial prompt as the first turn
ccode "fix the failing auth test and run it"

# Initial prompt in a new git worktree. Use --worktree=<name> (with `=`) so the
# prompt isn't swallowed as the worktree name — `ccode -w "refactor module X"`
# would treat "refactor module X" as the worktree label, not the prompt.
ccode --worktree=feat "refactor module X"

# Base the worktree on a specific branch (e.g. main) instead of the current HEAD:
ccode -w --ref main "implement feature from main"


# Start in a specific project directory
ccode --cwd ~/projects/my-app

# Add project-specific rules
ccode --rules "Always use TypeScript. Prefer functional components."

# Auto-approve all tool executions
ccode --yolo

# Use a specific model
ccode -m ccode-build

# Resume a previous session
ccode --resume <session-id>

# Continue the most recent session
ccode -c

# Experimental scrollback-native render mode. Sticky: plain `ccode` reopens in
# the mode last chosen via --minimal/--fullscreen (or /minimal//fullscreen).
ccode --minimal

# Back to the standard fullscreen TUI (and make it sticky again)
ccode --fullscreen

# Headless mode (for scripts)
ccode -p "Explain this codebase"
```

---

## Headless Mode

Run Ccode non-interactively for scripting, CI/CD, and automation:

```bash
ccode -p "Your prompt here"
```

Output formats:

| Format | Flag | Description |
|--------|------|-------------|
| `plain` | (default) | Human-readable text |
| `json` | `--output-format json` | Single JSON object with `text`, `stopReason`, `sessionId`, and `requestId` |
| `streaming-json` | `--output-format streaming-json` | NDJSON event stream for real-time processing |

Example CI/CD usage:

```bash
ccode -p "Review changes for bugs" --output-format json --yolo | jq -r '.text'
```

---

## Project Rules (AGENTS.md)

Add per-project instructions by creating an `AGENTS.md` file in your repository. Ccode reads these files and injects their contents as a project-instructions message at the start of the conversation:

```
~/.ccode/AGENTS.md           # Global rules (apply to all projects)
<repo-root>/AGENTS.md       # Repository-level rules
<cwd>/AGENTS.md             # Directory-level rules (highest priority)
```

Deeper files take precedence. Ccode also reads `CLAUDE.md` files for compatibility.

---

## Where to Go Next

| Document | What You Will Learn |
|----------|-------------------|
| [Authentication](02-authentication.md) | Browser login, API keys, OIDC, external auth, device code flow |
| [Keyboard Shortcuts](03-keyboard-shortcuts.md) | Complete reference for all key bindings |
| [Slash Commands](04-slash-commands.md) | All available `/` commands |
| [Configuration](05-configuration.md) | config.toml, pager.toml, environment variables |
