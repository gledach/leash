# leash examples

The most reliable way to watch an agent is to let leash start it:

```powershell
leash run --path . -- claude
```

leash starts the agent itself, so there is nothing to pick from a process list.
Use `--attach` or the window only as a fallback when the agent is already
running inside an editor. Attribution (`leash run`, `--attach`, `--pid`) needs
an elevated window: right-click PowerShell → Run as administrator.

Agent flags change. These were checked in November 2026 against current CLI
docs; if a flag errors, run the agent's own `--help` and keep the `leash run`
wrapper the same.

leash is read-only. "YOLO" / skip-permissions modes are exactly when you want
it watching — it records what happened, it does not block the agent.

## AI coding agents

### Claude Code (normal)
Recommended. Start Claude in this folder and attribute everything it and its
children touch. Prefer this over attaching to a running editor.

```powershell
leash run --path . -- claude
```

### Claude Code (YOLO)
Skip permission prompts. Official docs say this is for an isolated environment;
leash does not add a sandbox or a block-list.

```powershell
leash run --path . -- claude --dangerously-skip-permissions
```

### OpenAI Codex CLI (normal)
Start Codex in this folder and attribute its process tree.

```powershell
leash run --path . -- codex
```

### OpenAI Codex CLI (unattended / YOLO)
`--full-auto` was removed and now errors. Closest unattended workspace run:
workspace-write sandbox, never ask for approval. Full bypass (no sandbox) is
`--yolo` (same as `--dangerously-bypass-approvals-and-sandbox`). Official docs
say yolo is for a hardened or isolated environment; leash does not provide one.

```powershell
leash run --path . -- codex --sandbox workspace-write --ask-for-approval never
leash run --path . -- codex --yolo
```

### Gemini CLI (normal)
```powershell
leash run --path . -- gemini
```

### Gemini CLI (YOLO)
Prefer `--approval-mode=yolo`. `--yolo` / `-y` still work but are deprecated.
Do not pass both.

```powershell
leash run --path . -- gemini --approval-mode=yolo
```

### Aider
`--yes-always` is the current "don't ask" flag (`AIDER_YES_ALWAYS` also works).
The older `--yes` is still accepted. Aider has no sandbox.

```powershell
leash run --path . -- aider
leash run --path . -- aider --yes-always
```

## Attach to something already open (fallback)

Prefer `leash run -- claude` when you can. Use these only when the agent is
already running, for example inside a VS Code terminal.

### Cursor, VS Code or Windsurf
Attaches to the editor process and every descendant: terminals, extension hosts,
and an agent such as Claude running in the integrated terminal. Use `leash ps`
if you are not sure what is visible (rows include the folder, start time and a
command-line tail, so several `node.exe` or `Code.exe` entries can be told apart).

```powershell
leash start --path . --attach cursor
leash start --path . --attach vscode
leash start --path . --attach windsurf
# …work as usual…
leash stop
```

### A named agent, or a pid
`--attach claude` matches `claude.exe` or a `node.exe` whose command line points
at `claude-code` (npm shims show up as `node.exe`). That attaches only that
subtree, not the whole editor.

```powershell
leash ps
leash start --path . --attach claude
leash start --path . --pid 1234
leash stop
```

## Untrusted commands

### npm / pnpm install or a one-off tool
```powershell
leash run --path . -- pnpm install
leash run --path . -- npm install
leash run --path . -- npx some-tool
```

### Python script or pip
```powershell
leash run --path . -- python script.py
leash run --path . -- pip install some-package
```

## System-wide, no attribution

Same behaviour as the v0.1 preview. Every change on the PC during the session
is listed, including other apps. Works without Administrator (file coverage is
then a folder scan, not the USN journal).

```powershell
leash start --path .
# …run whatever you like…
leash stop
```

## Open the report

```powershell
leash open              # report.html in the browser
leash open --md
leash open --json
leash open --folder     # session folder in Explorer
leash last              # reprint the short summary
```

## Troubleshooting

```powershell
leash doctor
leash --verbose stop
```

The log is `%LOCALAPPDATA%\leash\leash.log`. Command-line reads that fail
(often access denied when leash is not elevated) are written there rather than
dropped silently. `leash ps` shows which editors and agents were detected, and
why.

## PowerShell quoting and npm shims

Put `--` in front of the agent so its flags are not eaten by leash. If PowerShell
itself is swallowing arguments, stop-parsing with `--%`:

```powershell
leash run --path . -- claude --dangerously-skip-permissions
leash run --path . --% -- claude --dangerously-skip-permissions
```

`claude`, `codex`, `gemini` and similar tools installed with npm are often
`claude.cmd` shims, not `.exe` files. `leash run` resolves PATHEXT and launches
`.cmd` / `.bat` through `cmd.exe /c`, and `.ps1` through PowerShell `-File`.
