# DeepSearch

**One executable searches inside almost everything.**

DeepSearch is a native Rust CLI for people who want grep-style search across ordinary files, documents, archives, and SQLite without installing Poppler, Pandoc, FFmpeg, or a collection of format-specific command-line tools.

```bash
deepsearch 'error.*timeout' .
deepsearch -F 'invoice 2026' ~/Documents
deepsearch -i -C 2 'needle' archive.zip
```

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

The release binary embeds the format parsers/codecs and bundled SQLite used by DeepSearch. It does **not** shell out to Poppler, Pandoc, SQLite, gzip, xz, zstd, or office tools.

## Scope decisions

V1 intentionally excludes OCR and audio/video transcription. Those are expensive semantic extraction problems and would weaken the core promise of a fast, dependable single-file search tool if added superficially.

## License

Licensed under either Apache-2.0 (`LICENSE-APACHE`) or MIT (`LICENSE-MIT`), at your option.
