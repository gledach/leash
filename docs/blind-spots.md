# What leash v1 does not see

This is the honest list. leash is an observer. It does not prevent anything, and it does not claim to.

- **Changes inside the watched repo are ignored by design**, except for a few high-priority names such as `.env`. If an agent only edits the project you pointed `--path` at, the file section of the report will be quiet on purpose.

- **Reads are invisible.** The USN journal records creates, deletes, renames, and data writes. Opening or reading a file does not show up.

- **No process attribution in v1.** A deleted folder or a new Run key is listed as a fact, not as “Cursor did this.” That needs ETW (planned for v2). Short-lived helper processes may be gone by `leash stop` and will not appear under leftover processes.

- **USN journal reads need Administrator.** Without elevation, leash falls back to a hash/mtime scan of priority folders (your profile dotfiles, `AppData\Roaming`, Startup, nearby projects). It will not see the rest of the disk. `leash doctor` explains the current mode.

- **The journal can wrap or be recreated.** If Windows recycles the journal between start and stop, early events can be missing. leash stores the cursor and warns when that happens.

- **Non-NTFS volumes, network shares, and WSL files** are limited or invisible to the USN path.

- **Registry / tasks / services coverage is the common persistence set**, not a complete catalogue. v1 does not look at WMI event subscriptions, Image File Execution Options, Winlogon notifications, or similar corners.

- **Global package detection is a PATH heuristic.** If `npm`, `pnpm`, `pip`, or `cargo` is missing or renamed, that list is skipped and noted.

- **Filtered noise can hide something real.** Malice dropped inside another project's `node_modules`, a browser cache, or `%TEMP%` is ignored by the default rules. You can tighten or loosen this with `%LOCALAPPDATA%\leash\config.toml`.

- **No network tracing.** A process that only talks to the network, without installing a file or opening a local listen port, is out of scope.

- **leash never deletes, kills, or blocks anything.** Observation only — it is not ransomware protection, not a sandbox, and not an EDR.
