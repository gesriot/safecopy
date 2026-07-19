# SafeCopy

Reliable file or folder copy to SD cards with end-to-end integrity verification.

The native Android port lives in [`android/`](android/README.md). It supports
verified copying both to and from an auto-detected USB drive.

Optimised for Windows, but works on macOS and Linux.

## Why another copy tool?

Regular `robocopy` / drag-and-drop copies assume your destination is reliable.
For SD cards that assumption is wrong:

- Bit-rot and stuck sectors are common, especially on cheap cards.
- USB-bridge hiccups, Windows Indexer and antivirus intercept file I/O through kernel-level filter drivers; a buggy or misbehaving driver can in principle interfere with writes.
- NAND controllers sometimes finish write-leveling *after* a file is "closed" – errors only surface on a genuine cold read minutes later.
- After the copy, the OS page cache may hand you back good data from RAM while the card actually has garbage. A normal re-read never notices.

## Features

- **XXH3-128** content hashing – fast enough that the SD card is the bottleneck, not the CPU.
- **Cache-bypass reads** – `FILE_FLAG_NO_BUFFERING` on Windows, `F_NOCACHE` on macOS, `posix_fadvise(DONTNEED)` on Linux. Verification reads come from the physical card, not from RAM. Note: on Linux `posix_fadvise` is an advisory hint, so the cache bypass there is best-effort, weaker than the hard guarantees on Windows/macOS.
- **Write-through writes** – `FILE_FLAG_WRITE_THROUGH` + `FlushFileBuffers` on Windows, `F_FULLFSYNC` on macOS. Data hits the device, not the page cache.
- **Sector-aligned I/O buffer** – a custom aligned allocator on Windows so that `FILE_FLAG_NO_BUFFERING` reads actually succeed.
- **Pre-flight sanity check** – writes and cold-reads a 10 MB pseudorandom probe before the real work starts. Bad reader / dead card fails fast.
- **`manifest.xxh3`** – written to the destination directory in `xxhsum -c` compatible format. The recipient can verify with `xxhsum -c manifest.xxh3` or with `safecopy verify`.
- **Resume-safe** – if a previous run was interrupted, re-running `safecopy copy` skips files whose source **and** cold-read destination hashes match the previous run, and removes stale `.safecopy.tmp` / `.safecopy.tmp.<N>` files. Resume works even with the manifest on the card disabled: a local checkpoint keyed by (source, destination) is saved after every verified file – next to the exe in `safecopy-state\` on Windows (portable), in `~/Library/Application Support/SafeCopy` on macOS, in `$XDG_DATA_HOME/safecopy` on Linux.
- **Settings remembered** – the GUI persists cooldown, retries, and checkboxes in the same per-platform state directory (`SAFECOPY_STATE_DIR` env var overrides the location).
- **Overlap guard** – copying a folder into itself or into its own subdirectory is rejected up front instead of silently overwriting the source.
- **Error classification** – `Transient` (retry with exponential backoff), `PersistentFile` (quarantine and continue), `PersistentDevice` (stop the whole run: disk full, permission denied, or too many consecutive failures).
- **Unlimited retries** – enabled by default (`--unlimited-retries=false` / GUI checkbox to cap attempts): keeps retrying transient write/verify failures without a cap. Each failed attempt leaves its `.safecopy.tmp.<N>` on the card so the filesystem is forced to allocate different sectors on the next try.
- **.gitignore filter** – optional (`--respect-gitignore` / GUI checkbox, off by default): when copying a code repository, files matched by the repo's own `.gitignore` rules (`target/`, `node_modules/`, build artifacts…) are skipped. Only `.gitignore` files inside the source tree are honoured – no global gitignore, no `.git/info/exclude`.
- **Junk filter** – optional (`--skip-junk` / GUI checkbox, off by default): skips development-tool caches and artifacts regardless of `.gitignore`: `__pycache__`, `node_modules`, `dist`, `venv`/`.venv` and `*-venv`, dot-dirs ending in `cache` (`.mypy_cache`, `.nuitka-cache`…), `.pytest-tmp*`, `*.egg-info`, `.tox`, `.nox`, `.eggs`, `.claude`, `.agents`, `.mplconfig`, plus OS litter files (`.DS_Store`, `Thumbs.db`, `desktop.ini`).
- **Quarantine folder** – files that can't be copied produce a JSON report in `.quarantine/` with timestamp, attempt count and reason. File-level failures continue with the next file; device-level failures stop the run.
- **Timestamps preserved** – `mtime`, `atime`, and (on Windows) the creation time.
- **Path-traversal safe** – the manifest reader rejects absolute paths, `..`, and Windows drive-letter prefixes so a hostile manifest cannot make `verify` read files outside the destination.
- **GUI and CLI** – a minimal desktop UI (egui) when launched with no args, or full CLI for scripting.

## Requirements

- Rust 1.75 or newer (needed for stable `FileTimes::set_created` on Windows).
- Windows 10/11 (primary target), macOS, or Linux.
- Destination formatted as **exFAT** recommended (no 4 GB file limit, no journal – less wear on the card).

## Usage

### GUI (default)

Double-click `safecopy.exe`, or run it with no arguments:

1. Pick a source: click **Файл...** to copy a single file, or **Папка...** to copy a whole tree.
2. Pick the destination folder on the SD card.
3. Optionally expand **Настройки** to change cooldown, retry count, toggle unlimited retries (on by default), toggle the manifest on the card (off by default), or enable the `.gitignore` filter. Settings are remembered between launches.
4. Click **НАЧАТЬ КОПИРОВАНИЕ**.

Progress bar, current filename, and per-file log (OK / WARN / RETRY / QUARANTINE / ERROR) are shown live.

The **Только проверка** button runs the standalone verification pass against the selected destination.

### CLI

```
safecopy copy <SOURCE> <DEST_DIR> [--cooldown-secs N]
                                  [--max-retries N]
                                  [--unlimited-retries[=BOOL]]
                                  [--no-manifest-on-card[=BOOL]]
                                  [--respect-gitignore]
                                  [--skip-junk]

safecopy verify <DIR_OR_MANIFEST>

safecopy gui                # launch the desktop UI explicitly
```

`<SOURCE>` is either a file or a folder. `<DEST_DIR>` is always a folder on the card; a single-file source is placed at `<DEST_DIR>/<filename>`, a folder source is copied together with its name into `<DEST_DIR>/<folder-name>/`.

Defaults: `--cooldown-secs 45`, `--max-retries 3`. `--unlimited-retries` and `--no-manifest-on-card` are **on** by default (disable with `=false`), `--respect-gitignore` and `--skip-junk` are off.

Examples:

```
safecopy copy "D:\Photos\2025-Trip" "E:\"
safecopy copy "D:\Photos\IMG_0001.RAW" "E:\"          # single file
safecopy copy "D:\Photos" "E:\Archive" --cooldown-secs 60
safecopy verify "E:\"
safecopy verify "E:\manifest.xxh3"
```

If copying would place a user file or folder literally named `manifest.xxh3` or `manifest.README.txt` at the destination root (a single file with that name, or a source folder itself named like that), SafeCopy refuses to start with manifest writing enabled because the manifest artifact would clobber the user's data. Re-run with `--no-manifest-on-card` to copy without writing a manifest; resume against an existing destination manifest is also disabled in that scenario, because `<DEST_DIR>/manifest.xxh3` is user data.

After a successful `copy`, the destination directory contains:

```
E:\
├── ...your files...
├── manifest.xxh3           # xxhsum -c compatible
├── manifest.README.txt     # one-liner explaining the manifest
└── .quarantine\            # only if any files failed
```

With `--no-manifest-on-card`, `manifest.xxh3` and `manifest.README.txt` are not written; the final in-memory cold-read verification still runs before success is reported.

## Typical workflow

1. Format the card as exFAT (Quick format, default allocation unit).
2. `safecopy copy <source> <card>` – sanity check → copy → cooldown → final cold re-read → manifest.
3. Safely eject the card, hand it over.
4. Recipient (optionally) verifies: `safecopy verify <card>` or `xxhsum -c manifest.xxh3`.
5. Recipient copies the files locally and returns the card.
6. `safecopy verify <card>` one more time to confirm nothing rotted in transit. If it fails, the card is retired.

## How the safety cascade works

For each file:

1. Create `<target>.safecopy.tmp.<attempt>`, opened with `FILE_FLAG_WRITE_THROUGH` on Windows.
2. A reader thread streams 1 MB blocks from the source through a bounded pool, hashing with XXH3-128 as it goes.
3. A writer thread consumes those blocks and writes them to the card.
4. Once the reader signals end-of-file, `sync_all()` + the platform-specific full-sync (`F_FULLFSYNC` on macOS, `FlushFileBuffers` on Windows) push the data to hardware; the handle is closed.
5. A **new** handle is opened with cache bypass (`FILE_FLAG_NO_BUFFERING` / `F_NOCACHE`) and the file is cold-read into a sector-aligned buffer.
6. The cold-read hash is compared against the source hash.
7. On match: timestamps are copied, the temporary file is atomically renamed into place.
8. On mismatch or I/O error: up to 3 retries with exponential backoff (1 s, 2 s, 4 s…). Persistent failure → quarantine entry + continue with the next file.

After the full source is done:

9. Cooldown for 45 s (controller finishes wear leveling).
10. Full cold re-read of every copied file, compared against the in-memory manifest. Mismatch here means the card is losing data after the fact – SafeCopy aborts with a clear message.
11. Unless `--no-manifest-on-card` was used, `manifest.xxh3` (+ `manifest.README.txt`) is written to the destination directory with its own `sync_all` so the manifest itself can't be lost to the page cache.

## Error handling

| Class              | When                                               | Reaction                                  |
| ------------------ | -------------------------------------------------- | ----------------------------------------- |
| `Transient`        | Hash mismatch, transient I/O error                 | Retry with backoff up to `--max-retries` (unlimited if `--unlimited-retries`). |
| `PersistentFile`   | All retries exhausted; source unreadable           | Quarantine the file, continue.            |
| `PersistentDevice` | Disk full, permission denied, sanity-check failed, or 5 files in a row failed | Stop the whole run immediately.           |

## What SafeCopy does *not* do

- No network copy, no deduplication, no compression, no encryption.
- No block-device access – we stay on top of the filesystem.
- No forced unmount / "safely remove" (requires admin on Windows).
- No filesystem repair.

## Development

CI runs the same checks locally:

```
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

### macOS packaging

Build the release binary first, then package it:

```
cargo build --release
./scripts/package-macos.sh
```

The script creates:

- `dist/SaveCopy.app`
- `dist/SaveCopy.app.zip`
- `dist/SaveCopy.dmg`

It uses `macos/icon.PNG` as the source artwork, generates the rounded runtime icon used by egui in the Dock, writes the `.icns` bundle icon, and verifies the DMG with `hdiutil`.

### Windows packaging

Run the PowerShell packaging script from Windows. It requires Python with
Pillow and `rc.exe` from the Windows SDK:

```powershell
.\scripts\package-windows.ps1
```

The script converts `macos/icon-runtime.png` to a multi-size `.ico`, embeds it
into the release executable through a Windows resource, and creates:

- `dist/windows/SaveCopy.exe`
- `dist/SaveCopy-windows.zip`

## License

MIT – see [LICENSE](LICENSE).
