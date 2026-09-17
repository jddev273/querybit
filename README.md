# QueryBit

**One executable searches inside almost everything.**

QueryBit is a native Rust CLI for people who want grep-style search across ordinary files, documents, archives, and SQLite without installing Poppler, Pandoc, FFmpeg, or a collection of format-specific command-line tools.

```bash
querybit 'error.*timeout' .
querybit -F 'invoice 2026' ~/Documents
querybit -i -C 2 'needle' archive.zip
```

## Install

### Homebrew (macOS / Linux)

```bash
brew tap jddev273/tap
brew install querybit
```

### Scoop (Windows)

```powershell
scoop bucket add jddev273 https://github.com/jddev273/scoop-bucket
scoop install jddev273/querybit
```

### Direct download

Download the binary for your platform from the [latest GitHub release](https://github.com/jddev273/querybit/releases/latest), verify its adjacent SHA-256 checksum, and put it somewhere on your `PATH`.

- **Linux x86-64:** `querybit-linux-x86_64` is statically linked (musl), so it does not require a particular glibc version. Run `chmod +x querybit-linux-x86_64` after downloading.
- **macOS Apple Silicon:** `querybit-macos-aarch64` (macOS 11+). Run `chmod +x querybit-macos-aarch64` after downloading.
- **macOS Intel:** `querybit-macos-x86_64` (macOS 10.12+). Run `chmod +x querybit-macos-x86_64` after downloading.
- **Windows x86-64:** `querybit-windows-x86_64.exe`.

The first unsigned V1 binaries may trigger normal macOS Gatekeeper or Windows SmartScreen warnings because they are not code-signed. QueryBit does not require administrator privileges or an installer.

## V1 formats

- Normal text files, streamed without the old 64 MiB cutoff; UTF-8 plus BOM-marked UTF-16LE/UTF-16BE
- ZIP, TAR, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, `.tar.zst`/`.tzst`
- Single gzip, xz, and zstd streams
- DOCX visible text reconstructed across adjacent Office runs
- XLSX values/shared strings and formulas with sheet/cell provenance
- PPTX text with slide provenance, plus EPUB HTML/XHTML
- SQLite databases, including live top-level WAL-mode databases opened read-only with a 64 MiB SQLite value/row safety limit
- PDF text extraction with page provenance
- Nested archives, bounded by recursion depth and extraction budgets

Search is regex by default. Use `-F/--fixed-strings` for literal matching, `-i` for case-insensitive matching, and `-C N` for context. `.gitignore`, `.ignore`, and Git exclude rules are respected during directory walks; `--hidden` includes hidden paths.

Results preserve provenance using `::`, for example:

```text
reports.zip::quarterly.docx::word/document.xml:1:revenue target
book.pdf::page=12:37:revenue target
cache.sqlite::table=events::row=42::col=message:1:revenue target
```

## Search status and safety bounds

Exit codes are `0` for a complete search with matches, `1` for a complete search with no matches, and `2` when the requested search is incomplete or fails. Filesystem walk errors, parser failures, corrupt members, recursion limits, and extraction limits are diagnosed rather than silently becoming "no match". Independent archive members continue where possible.

Top-level ordinary text is streamed. Extracted members are limited to 64 MiB each with a 512 MiB cumulative extracted/search-payload budget per top-level file/container tree and recursion depth 4 by default. Single compressed streams use the same budgets. Hitting a limit returns an incomplete-search diagnostic instead of silently truncating data. Structured top-level inputs that require in-memory parsing have a 512 MiB parser safety ceiling.

## Build and validation

```bash
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

Release CI gates publication on formatting, strict Clippy, tests, and a current RustSec audit, then runs native tests on Linux, Intel macOS, Apple Silicon macOS, and Windows before staging binaries and SHA-256 checksums.

The release binary embeds the format parsers/codecs and bundled SQLite used by QueryBit. It does **not** shell out to Poppler, Pandoc, SQLite, gzip, xz, zstd, or office tools.

## Scope decisions

V1 intentionally excludes OCR and audio/video transcription. Those are expensive semantic extraction problems and would weaken the core promise of a fast, dependable single-file search tool if added superficially.

## License

Licensed under either Apache-2.0 (`LICENSE-APACHE`) or MIT (`LICENSE-MIT`), at your option.
