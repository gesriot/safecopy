# SafeCopy

Reliable file or folder copy to SD cards with end-to-end integrity verification.

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
- **Cache-bypass reads** – `FILE_FLAG_NO_BUFFERING` on Windows, `F_NOCACHE` on macOS, `posix_fadvise(DONTNEED)` on Linux. Verification reads come from the physical card, not from RAM.
- **Write-through writes** – `FILE_FLAG_WRITE_THROUGH` + `FlushFileBuffers` on Windows, `F_FULLFSYNC` on macOS. Data hits the device, not the page cache.
- **Sector-aligned I/O buffer** – a custom aligned allocator on Windows so that `FILE_FLAG_NO_BUFFERING` reads actually succeed.
- **Pre-flight sanity check** – writes and cold-reads a 10 MB pseudorandom probe before the real work starts. Bad reader / dead card fails fast.
- **`manifest.xxh3`** – written to the destination directory in `xxhsum -c` compatible format. The recipient can verify with `xxhsum -c manifest.xxh3` or with `safecopy verify`.
- **Resume-safe** – if a previous run was interrupted, re-running `safecopy copy` reads the existing manifest, skips files whose source **and** cold-read hashes match, and removes stale `.safecopy.tmp` / `.safecopy.tmp.<N>` files.
- **Error classification** – `Transient` (retry with exponential backoff), `PersistentFile` (quarantine and continue), `PersistentDevice` (stop the whole run: disk full, permission denied, or too many consecutive failures).
- **Unlimited retries** – optional mode (`--unlimited-retries` / GUI checkbox) that keeps retrying transient write/verify failures without a cap. Each failed attempt leaves its `.safecopy.tmp.<N>` on the card so the filesystem is forced to allocate different sectors on the next try.
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
3. Optionally expand **Настройки** to change cooldown, retry count, enable unlimited retries, or disable writing the manifest to the card.
4. Click **НАЧАТЬ КОПИРОВАНИЕ**.

Progress bar, current filename, and per-file log (OK / WARN / RETRY / QUARANTINE / ERROR) are shown live.

The **Только проверка** button runs the standalone verification pass against the selected destination.

### CLI

```
safecopy copy <SOURCE> <DEST_DIR> [--cooldown-secs N]
                                  [--max-retries N]
                                  [--unlimited-retries]
                                  [--no-manifest-on-card]

safecopy verify <DIR_OR_MANIFEST>

safecopy gui                # launch the desktop UI explicitly
```

`<SOURCE>` is either a file or a folder. `<DEST_DIR>` is always a folder on the card; a single-file source is placed at `<DEST_DIR>/<filename>`.

Defaults: `--cooldown-secs 45`, `--max-retries 3`. `--unlimited-retries` is off by default.

Examples:

```
safecopy copy "D:\Photos\2025-Trip" "E:\"
safecopy copy "D:\Photos\IMG_0001.RAW" "E:\"          # single file
safecopy copy "D:\Photos" "E:\Archive" --cooldown-secs 60
safecopy verify "E:\"
safecopy verify "E:\manifest.xxh3"
```

If the source contains a file literally named `manifest.xxh3` or `manifest.README.txt` at its root, SafeCopy refuses to start because writing the manifest would clobber the user's file. Re-run with `--no-manifest-on-card` to copy without writing a manifest. If the source root contains `manifest.xxh3`, resume against an existing destination manifest is also disabled, because `<DEST_DIR>/manifest.xxh3` is user data in that scenario.

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

## License

MIT – see [LICENSE](LICENSE).
