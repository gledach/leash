# leash

A small, free, local-first Windows tool for the [gledach.de](https://gledach.de) shelf.

When an AI coding agent (Cursor, Claude Code, Codex, and friends) runs on your PC, it can change more than the repo you opened. leash takes a snapshot of the machine before the session, another one after, and prints a filtered report of what changed *outside* that repo.

It is read-only. It never deletes, kills, or blocks anything. It does not open a network connection and does not ask for an account.

## What it looks at

- File changes on NTFS/ReFS volumes, via the USN change journal when you run it elevated, grouped as create / delete / rename / data overwrite or extend
- A fallback hash/mtime scan of priority folders if you are not elevated: profile dotfiles, `AppData\Roaming`, Startup, nearby projects
- Registry `Run` and `RunOnce` (HKCU and HKLM)
- Scheduled tasks, Windows services, user and system environment variables (PATH is split into entries)
- Global npm, pnpm, pip, and cargo packages (if those tools are on PATH)
- Shell profiles, `.gitconfig`, and `~/.ssh` (hashes and file names only — never key material)
- Listening TCP/UDP ports, with the owning process when it can be seen
- Processes that appeared during the session and are still running (pid, parent, command line when available)

The report is ranked **high / medium / info**, each finding with a short reason. Default filters drop the watched repo, `node_modules`, `.git`, `%TEMP%`, browser and editor caches, and Windows prefetch/log noise. `.env` files, Startup folder items, `AppData\Roaming`, and other project roots are kept and ranked high.

Rules live in a small, tested module. Copy `config/default-filters.toml` to `%LOCALAPPDATA%\leash\config.toml` and edit it; `extra_ignore_globs` and `extra_high_globs` append to the defaults.

## What it does not see

Please read [docs/blind-spots.md](docs/blind-spots.md). The short version: repo-internal edits are ignored on purpose, file reads are invisible, v1 cannot name the process that made a change, USN needs Administrator, the journal can wrap, and filters can hide something planted in a cache or `node_modules`.

## Usage (Windows 11)

The binary is a single `leash.exe`. Double-click it in Explorer (or pin it with **Add to Start menu** in the window). A calm window opens: pick the project folder, press **Start**, use the agent as usual, then press **Stop**. Start writes a snapshot to disk, so you can close the window and still press Stop later. **Open report** opens `report.html` in your browser.

If you are not administrator, the window says so and offers **Restart as admin for full coverage**. Without elevation, file coverage is a fallback scan of key folders; startup entries, packages and ports still appear.

The same exe is the CLI when you pass any argument (`leash start`, `leash --help`, …). Double-click does not show a console. From PowerShell the CLI still prints to that terminal (it attaches to the parent console, and it leaves pipes alone so `leash report --json > file` works).

For a full file diff, start an **elevated** window (Restart as admin) or PowerShell (right-click → Run as administrator):


```powershell
cd C:\path\to\your\repo
leash doctor
leash start --path .
# …run Cursor / Claude / Codex as you normally would…
leash stop
# console: a short summary + paths, not the full report
leash open              # opens report.html in your browser
leash last              # reprint the short summary
leash report --verbose  # full text in the terminal
leash report --json     # JSON on stdout
```

`leash stop` writes `report.html`, `report.json`, and `report.md` under `%LOCALAPPDATA%\leash\sessions\<id>\`. The console shows counts, up to three High findings, and those file paths, then a hint: `Open it: leash open`. The HTML report is self-contained (no network, light and dark). It exits with code 1 if there is any high finding (set `exit_on_high = false` in the config to change that).

```powershell
leash open              # report.html (default)
leash open --md
leash open --json
leash open --folder     # session folder in Explorer
leash stop --open       # also open the session folder
leash stop --verbose    # full text report in the terminal
```

Aliases, if you prefer the names:

```powershell
leash snap before
leash snap after
leash diff
leash diff --json
leash diff --markdown .\leash-report.md
```

Without Administrator, `start` / `stop` still run. File coverage shrinks to the fallback folder scan, and the report will say so. Persistence collectors (Run keys, tasks, services, env, ports, processes, packages) work unelevated, with the usual access limits.

Put `leash.exe` anywhere you like. There is no installer in the portable zip. In the window, **Add to Start menu** creates a normal `.lnk` under your Start menu. An Inno Setup script lives at `installer/leash.iss` for a later Windows build; it is not compiled in this tree.

## Build

Developed as a Rust workspace. Unit tests for filters, diffs, reports, and parsers run on Linux. The Windows collectors compile behind `cfg(windows)`.

```bash
# on a Linux box, with a MinGW (or llvm-mingw) linker on PATH
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --target x86_64-pc-windows-gnu
```

The window uses [GPUI Kit](https://gpui-kit.com/) 0.9.1. `vendor/gpui-pre-windows` is a small patch so release builds can cross-compile from Linux without `fxc.exe`; shaders are compiled at first launch with `D3DCompile`.

The release binary lands at `target/x86_64-pc-windows-gnu/release/leash.exe`. Building natively on Windows with the MSVC toolchain (`x86_64-pc-windows-msvc`) is also fine, and is the simpler path on a Windows 11 machine.

If you cross-compile from Linux with [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) rather than a GCC-based MinGW, rustc still asks for `-lgcc` / `-lgcc_eh`. Run `scripts/setup-cross.sh` after setting `LLVM_MINGW` to the toolchain prefix; `.cargo/config.toml` already adds that compat library path. A classic `mingw-w64` GCC install does not need the script.

## Layout

```
crates/
  leash-cli          command line (start, stop, report, doctor)
  leash-snapshot     snapshot model, session store, diff, report
  leash-filter       ignore / priority rules
  leash-usn          USN journal via usn-journal-rs, plus fallback scans
  leash-persist      registry, tasks, services, env, ports, processes
  leash-packages     global npm / pnpm / pip / cargo
  leash-alert        empty hook for v1.5 toasts
config/default-filters.toml
docs/blind-spots.md
```

## License

MIT. See [LICENSE](LICENSE).
