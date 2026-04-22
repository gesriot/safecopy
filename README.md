# SafeCopy

Reliable folder copy to SD cards with end-to-end integrity verification.

Optimised for Windows, but works on macOS and Linux.

## Why another copy tool?

Regular `robocopy` / drag-and-drop copies assume your destination is reliable.
For SD cards that assumption is wrong:

- Bit-rot and stuck sectors are common, especially on cheap cards.
- USB-bridge hiccups, Windows Indexer and antivirus intercept file I/O through kernel-level filter drivers; a buggy or misbehaving driver can in principle interfere with writes.
- NAND controllers sometimes finish write-leveling *after* a file is "closed" – errors only surface on a genuine cold read minutes later.
- After the copy, the OS page cache may hand you back good data from RAM while
  the card actually has garbage. A normal re-read never notices.

SafeCopy is built around these failure modes. Every file is written, flushed to hardware, re-read through a cache-bypass handle, hashed with XXH3-128, and compared against what was written. After the whole folder is done, the program cools down for 45 seconds (configurable) and does a second full cold re-read to catch delayed controller failures.

## Features

- **XXH3-128** content hashing – fast enough that the SD card is the bottleneck, not the CPU.
- **Cache-bypass reads** – `FILE_FLAG_NO_BUFFERING` on Windows, `F_NOCACHE` on macOS, `posix_fadvise(DONTNEED)` on Linux. Verification reads come from the physical card, not from RAM.
- **Write-through writes** – `FILE_FLAG_WRITE_THROUGH` + `FlushFileBuffers` on Windows, `F_FULLFSYNC` on macOS. Data hits the device, not the page cache.
- **Sector-aligned I/O buffer** – a custom aligned allocator on Windows so that `FILE_FLAG_NO_BUFFERING` reads actually succeed.
- **Pre-flight sanity check** – writes and cold-reads a 10 MB pseudorandom probe before the real work starts. Bad reader / dead card fails fast.
- **Safe-copy pipeline** – `.safecopy.tmp` → write → full_sync → close → cold-read verify → atomic rename → timestamp copy. The final filename never appears before the content is verified.
- **2-stage in-file pipeline** – a reader thread hashes the source while a writer thread streams 1 MB blocks to the card through a bounded buffer pool.
- **Cooldown + final re-read** – 45 s by default, then every file is cold-read again against the in-memory manifest. Delayed controller errors surface here rather than in the user's hands.
- **`manifest.xxh3`** – written at the root of the card in `xxhsum -c` compatible format. The recipient can verify with `xxhsum -c manifest.xxh3` or with `safecopy verify`.
- **Resume-safe** – if a previous run was interrupted, re-running `safecopy copy` reads the existing manifest, skips files whose source **and** cold-read stale `.safecopy.tmp` files.
- **Error classification** – `Transient` (retry with exponential backoff), `PersistentFile` (quarantine and continue), `PersistentDevice` (stop the whole run: disk full, permission denied, or too many consecutive failures).
- **Unlimited retries** – optional mode (`--unlimited-retries` / GUI checkbox) that keeps retrying transient write/verify failures without a cap. Each failed attempt leaves its `.safecopy.tmp.<N>` on the card so the filesystem is forced to allocate different sectors on the next try. Stops only on `PersistentDevice` (disk full) or when all retries for source-unreadable files are exhausted.
- **Quarantine folder** – files that can't be copied produce a JSON report in `.quarantine/` with timestamp, attempt count and reason. Run is not aborted.
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

1. Pick a source folder.
2. Pick the destination folder on the SD card.
3. Optionally expand **Settings** to change cooldown, retry count, enable unlimited retries, or disable writing the manifest to the card.
4. Click **START COPY**.

Progress bar, current filename, and per-file log (OK / WARN / RETRY / QUARANTINE / ERROR) are shown live.

The **Verify only** button runs the standalone verification pass against the selected destination.

### CLI

```
safecopy copy <SOURCE_DIR> <DEST_DIR> [--cooldown-secs N]
                                      [--max-retries N]
                                      [--unlimited-retries]
                                      [--no-manifest-on-card]

safecopy verify <DIR_OR_MANIFEST>

safecopy gui                # launch the desktop UI explicitly
```

Defaults: `--cooldown-secs 45`, `--max-retries 3`. `--unlimited-retries` is off by default.

Examples:

```
safecopy copy "D:\Photos\2025-Trip" "E:\"
safecopy copy "D:\Photos" "E:\Archive" --cooldown-secs 60
safecopy verify "E:\"
safecopy verify "E:\manifest.xxh3"
```

After a successful `copy`, the card contains:

```
E:\
├── ...your files...
├── manifest.xxh3           # xxhsum -c compatible
├── manifest.README.txt     # one-liner explaining the manifest
└── .quarantine\            # only if any files failed
```

## Typical workflow

1. Format the card as exFAT (Quick format, default allocation unit).
2. `safecopy copy <source> <card>` – sanity check → copy → cooldown → final cold re-read → manifest.
3. Safely eject the card, hand it over.
4. Recipient (optionally) verifies: `safecopy verify <card>` or `xxhsum -c manifest.xxh3`.
5. Recipient copies the files locally and returns the card.
6. `safecopy verify <card>` one more time to confirm nothing rotted in transit. If it fails, the card is retired.

## How the safety cascade works

For each file:

1. Create `<target>.safecopy.tmp`, opened with `FILE_FLAG_WRITE_THROUGH` on Windows.
2. A reader thread streams 1 MB blocks from the source through a bounded pool, hashing with XXH3-128 as it goes.
3. A writer thread consumes those blocks and writes them to the card.
4. Once the reader signals end-of-file, `sync_all()` + the platform-specific full-sync (`F_FULLFSYNC` on macOS, `FlushFileBuffers` on Windows) push the data to hardware; the handle is closed.
5. A **new** handle is opened with cache bypass (`FILE_FLAG_NO_BUFFERING` / `F_NOCACHE`) and the file is cold-read into a sector-aligned buffer.
6. The cold-read hash is compared against the source hash.
7. On match: timestamps are copied, the `.tmp` file is atomically renamed into place.
8. On mismatch or I/O error: up to 3 retries with exponential backoff (1 s, 2 s, 4 s…). Persistent failure → quarantine entry + continue with the next file.

After the full folder is done:

9. Cooldown for 45 s (controller finishes wear leveling).
10. Full cold re-read of every copied file, compared against the in-memory manifest. Mismatch here means the card is losing data after the fact – SafeCopy aborts with a clear message.
11. `manifest.xxh3` (+ `manifest.README.txt`) is written to the card root with its own `sync_all` so the manifest itself can't be lost to the page cache.

## Error handling

| Class              | When                                               | Reaction                                  |
| ------------------ | -------------------------------------------------- | ----------------------------------------- |
| `Transient`        | Hash mismatch, transient I/O error                 | Retry with backoff up to `--max-retries` (unlimited if `--unlimited-retries`). |
| `PersistentFile`   | All retries exhausted; source unreadable           | Quarantine the file, continue.            |
| `PersistentDevice` | Disk full, permission denied, sanity-check failed, or 5 files in a row failed | Stop the whole run immediately.           |

Quarantine files live in `<dest>/.quarantine/<mangled_name>.<ts>.failed.json`:

```json
{
  "source_relative": "DCIM/IMG_0123.JPG",
  "attempts": 3,
  "unix_time": 1714000000,
  "reason": "..."
}
```

## Resume / idempotency

Re-running `safecopy copy` on a destination that already contains a `manifest.xxh3` is safe:

- The existing manifest is loaded into memory.
- For each source file, if the manifest has a hash AND the destination file exists AND its cold-read hash matches AND the source hash matches, the file is skipped.
- Anything else (missing, corrupted, changed source) is re-copied from scratch.
- Stray `*.safecopy.tmp` files left over from the previous interrupted run are deleted before the main pass.

The `.quarantine/` folder is left untouched – the user decides what to do with its contents.

## What SafeCopy does *not* do

- No network copy, no deduplication, no compression, no encryption.
- No block-device access – we stay on top of the filesystem.
- No forced unmount / "safely remove" (requires admin on Windows).
- No filesystem repair.

## Project layout

```
copy/
├── Cargo.toml
├── Plan.md
├── README.md
└── src/
    ├── main.rs          — CLI entry, routes to CLI subcommands or GUI
    ├── cli.rs           — clap definitions
    ├── gui.rs           — egui desktop UI
    ├── progress.rs      — ProgressReporter trait + events
    ├── error.rs         — CopyError / ErrorClass
    ├── hash.rs          — XXH3-128 streaming + cold_hash_file helper
    ├── io_flags.rs      — cross-platform cache-bypass / write-through / aligned IoBuf
    ├── sanity.rs        — pre-flight probe
    ├── copy.rs          — safe-copy pipeline, resume, quarantine dispatch
    ├── verify.rs        — standalone verification pass
    ├── manifest.rs      — xxhsum-compatible manifest read/write (+ traversal guard)
    ├── quarantine.rs    — JSON failure reports
    └── timestamps.rs    — mtime / atime / (Windows) creation time copy
```

## License

MIT – see [LICENSE](LICENSE).
