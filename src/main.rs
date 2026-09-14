use anyhow::{anyhow, Context, Result};
use clap::Parser;
use flate2::read::GzDecoder;
use ignore::WalkBuilder;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use regex::{Regex, RegexBuilder};
use rusqlite::{limits::Limit, types::ValueRef, Connection};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Cursor, Read, Write},
    path::{Path, PathBuf},
};
use tar::Archive;
use xz2::read::XzDecoder;

const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_EXPANDED: usize = 512 * 1024 * 1024;
const MAX_TOPLEVEL_CONTAINER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PLAIN_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Parser, Debug)]
#[command(
    name = "deepsearch",
    version,
    about = "Download one file; grep almost anything"
)]
struct Args {
    pattern: String,
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(short = 'F', long)]
    fixed_strings: bool,
    #[arg(short = 'i', long)]
    ignore_case: bool,
    #[arg(short = 'C', long, default_value_t = 0)]
    context: usize,
    #[arg(long, default_value_t = 4)]
    max_depth: usize,
    #[arg(long)]
    hidden: bool,
}

struct Searcher {
    rx: Regex,
    context: usize,
    max_depth: usize,
    expanded: usize,
    matches: usize,
    incomplete: bool,
    broken_pipe: bool,
    out: BufWriter<io::Stdout>,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("deepsearch: {e:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args = Args::parse();
    if !args.path.exists() {
        return Err(anyhow!("{}: path does not exist", args.path.display()));
    }
    if !args.path.is_file() && !args.path.is_dir() {
        return Err(anyhow!("{}: unsupported input type", args.path.display()));
    }

    let pat = if args.fixed_strings {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    let rx = RegexBuilder::new(&pat)
        .case_insensitive(args.ignore_case)
        .build()
        .context("invalid regex")?;
    let mut s = Searcher {
        rx,
        context: args.context,
        max_depth: args.max_depth,
        expanded: 0,
        matches: 0,
        incomplete: false,
        broken_pipe: false,
        out: BufWriter::new(io::stdout()),
    };

    if args.path.is_file() {
        s.search_path(&args.path)?;
    } else {
        let mut wb = WalkBuilder::new(&args.path);
        wb.hidden(!args.hidden)
            .git_ignore(true)
            .git_exclude(true)
            .ignore(true)
            .parents(true)
            .require_git(false);
        for result in wb.build() {
            match result {
                Ok(dent) if dent.path().is_file() => {
                    if let Err(e) = s.search_path(dent.path()) {
                        s.warn(format!("{}: {e:#}", dent.path().display()));
                    }
                }
                Ok(_) => {}
                Err(e) => s.warn(format!("walk error: {e}")),
            }
            if s.broken_pipe {
                break;
            }
        }
    }

    if let Err(e) = s.out.flush() {
        if e.kind() != io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
        s.broken_pipe = true;
    }
    if s.broken_pipe {
        return Ok(0);
    }
    if s.incomplete {
        Ok(2)
    } else if s.matches > 0 {
        Ok(0)
    } else {
        Ok(1)
    }
}

impl Searcher {
    fn warn(&mut self, message: String) {
        self.incomplete = true;
        eprintln!("deepsearch: warning: {message}");
    }

    fn emit(&mut self, line: &str) {
        if self.broken_pipe {
            return;
        }
        if let Err(e) = writeln!(self.out, "{line}") {
            if e.kind() == io::ErrorKind::BrokenPipe {
                self.broken_pipe = true;
            } else {
                self.warn(format!("stdout: {e}"));
            }
        }
    }

    fn emit_line(&mut self, label: &str, line_no: usize, text: &str) {
        self.emit(&format!("{label}:{line_no}:{text}"));
    }

    fn search_path(&mut self, path: &Path) -> Result<()> {
        // Expansion safety is scoped to one top-level input/tree, not the whole directory walk.
        self.expanded = 0;
        let meta = fs::metadata(path)?;
        let label = path.display().to_string();
        let lower = label.to_ascii_lowercase();
        let magic = read_prefix(path, 8)?;

        if is_sqliteish(&lower, &magic) {
            return self.search_sqlite_path(path, &label);
        }

        if is_containerish(&lower, &magic) {
            if meta.len() > MAX_TOPLEVEL_CONTAINER_BYTES {
                self.warn(format!(
                    "{label}: container is {} bytes; top-level parser safety limit is {MAX_TOPLEVEL_CONTAINER_BYTES}",
                    meta.len()
                ));
                return Ok(());
            }
            let bytes = fs::read(path)?;
            return self.search_blob(label, &bytes, 0);
        }

        self.search_plain_path(path, &label)
    }

    fn search_plain_path(&mut self, path: &Path, label: &str) -> Result<()> {
        let prefix = read_prefix(path, 8192)?;
        if prefix.starts_with(&[0xff, 0xfe]) || prefix.starts_with(&[0xfe, 0xff]) {
            let meta = fs::metadata(path)?;
            if meta.len() > MAX_TOPLEVEL_CONTAINER_BYTES {
                self.warn(format!("{label}: UTF-16 text is too large for V1 decoder"));
                return Ok(());
            }
            let bytes = fs::read(path)?;
            if let Some(text) = decode_unicode_text(&bytes) {
                self.search_text(label, &text);
            }
            return Ok(());
        }

        if !looks_text(&prefix) {
            return Ok(());
        }

        let f = File::open(path)?;
        let mut r = BufReader::new(f);
        let mut line_no = 0usize;
        let mut prev: VecDeque<(usize, String)> = VecDeque::new();
        let mut after = 0usize;
        let mut last_printed = 0usize;
        let mut raw = Vec::new();
        loop {
            raw.clear();
            let n = (&mut r)
                .take((MAX_PLAIN_LINE_BYTES + 1) as u64)
                .read_until(b'\n', &mut raw)?;
            if n == 0 {
                break;
            }
            if n > MAX_PLAIN_LINE_BYTES {
                self.warn(format!(
                    "{label}: logical line exceeds {MAX_PLAIN_LINE_BYTES} byte safety limit"
                ));
                return Ok(());
            }
            line_no += 1;
            if line_no == 1 && raw.starts_with(&[0xef, 0xbb, 0xbf]) {
                raw.drain(..3);
            }
            let text = String::from_utf8_lossy(&raw)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            let hit = self.rx.is_match(&text);
            if hit {
                self.matches += 1;
                let first = prev.front().map(|(n, _)| *n).unwrap_or(line_no);
                if self.context > 0 && last_printed > 0 && first > last_printed + 1 {
                    self.emit("--");
                }
                for (n, s) in &prev {
                    if *n > last_printed {
                        self.emit_line(label, *n, s);
                        last_printed = *n;
                    }
                }
                if line_no > last_printed {
                    self.emit_line(label, line_no, &text);
                    last_printed = line_no;
                }
                after = self.context;
            } else if after > 0 {
                self.emit_line(label, line_no, &text);
                last_printed = line_no;
                after -= 1;
            }
            if self.context > 0 {
                prev.push_back((line_no, text));
                while prev.len() > self.context {
                    prev.pop_front();
                }
            }
            if self.broken_pipe {
                break;
            }
        }
        Ok(())
    }

    fn search_blob(&mut self, label: String, bytes: &[u8], depth: usize) -> Result<()> {
        if depth > self.max_depth {
            self.warn(format!("{label}: maximum archive recursion depth exceeded"));
            return Ok(());
        }
        let lower = label.to_ascii_lowercase();
        if is_zipish(&lower, bytes) {
            return self.search_zip(label, bytes, depth);
        }
        if lower.ends_with(".tar") {
            return self.search_tar(label, bytes, depth);
        }
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            let mut d = GzDecoder::new(bytes);
            return self.search_decoded_tar(label, &mut d, depth);
        }
        if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
            let mut d = XzDecoder::new(bytes);
            return self.search_decoded_tar(label, &mut d, depth);
        }
        if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
            let mut d = zstd::stream::read::Decoder::new(bytes)?;
            return self.search_decoded_tar(label, &mut d, depth);
        }
        if lower.ends_with(".gz") {
            let mut d = GzDecoder::new(bytes);
            if let Some(out) = self.read_expanded(&label, &mut d)? {
                return self.search_blob(trim_suffix(&label, ".gz"), &out, depth + 1);
            }
            return Ok(());
        }
        if lower.ends_with(".xz") {
            let mut d = XzDecoder::new(bytes);
            if let Some(out) = self.read_expanded(&label, &mut d)? {
                return self.search_blob(trim_suffix(&label, ".xz"), &out, depth + 1);
            }
            return Ok(());
        }
        if lower.ends_with(".zst") || lower.ends_with(".zstd") {
            let mut d = zstd::stream::read::Decoder::new(bytes)?;
            if let Some(out) = self.read_expanded(&label, &mut d)? {
                return self.search_blob(
                    trim_suffix_any(&label, &[".zst", ".zstd"]),
                    &out,
                    depth + 1,
                );
            }
            return Ok(());
        }
        if is_sqliteish(&lower, bytes) {
            return self.search_sqlite_bytes(&label, bytes);
        }
        if lower.ends_with(".pdf") || bytes.starts_with(b"%PDF-") {
            return self.search_pdf(&label, bytes);
        }
        if let Some(text) = decode_unicode_text(bytes) {
            self.search_text(&label, &text);
        } else if looks_text(bytes) {
            self.search_text(&label, &String::from_utf8_lossy(bytes));
        }
        Ok(())
    }

    fn read_zip_member<R: Read>(
        &mut self,
        label: &str,
        reader: &mut R,
        declared_size: usize,
    ) -> Result<Option<Vec<u8>>> {
        let remaining = MAX_TOTAL_EXPANDED.saturating_sub(self.expanded);
        let limit = remaining.min(MAX_ENTRY_BYTES);
        if limit == 0 {
            self.warn(format!("{label}: 512 MiB total expansion budget exhausted"));
            return Ok(None);
        }
        if declared_size > limit {
            self.warn(format!(
                "{label}: declared extracted size {declared_size} exceeds {limit} byte safety budget"
            ));
            return Ok(None);
        }
        let mut data = Vec::with_capacity(declared_size.min(1 << 20));
        match reader.take((limit + 1) as u64).read_to_end(&mut data) {
            Ok(_) => {}
            Err(e) => {
                self.warn(format!("{label}: {e}"));
                return Ok(None);
            }
        }
        if data.len() > limit {
            self.warn(format!(
                "{label}: actual extracted data exceeds {limit} byte safety budget"
            ));
            return Ok(None);
        }
        if data.len() != declared_size {
            self.warn(format!(
                "{label}: ZIP size metadata mismatch (declared {declared_size}, actual {})",
                data.len()
            ));
        }
        self.expanded += data.len();
        Ok(Some(data))
    }

    fn reserve_expanded(&mut self, label: &str, size: usize) -> bool {
        if size > MAX_ENTRY_BYTES {
            self.warn(format!("{label}: extracted member exceeds 64 MiB"));
            return false;
        }
        if self.expanded.saturating_add(size) > MAX_TOTAL_EXPANDED {
            self.warn(format!("{label}: 512 MiB total expansion budget exhausted"));
            return false;
        }
        self.expanded += size;
        true
    }

    fn read_expanded<R: Read>(&mut self, label: &str, r: &mut R) -> Result<Option<Vec<u8>>> {
        let remaining = MAX_TOTAL_EXPANDED.saturating_sub(self.expanded);
        let limit = remaining.min(MAX_ENTRY_BYTES);
        if limit == 0 {
            self.warn(format!("{label}: 512 MiB total expansion budget exhausted"));
            return Ok(None);
        }
        let mut v = Vec::new();
        r.take((limit + 1) as u64).read_to_end(&mut v)?;
        if v.len() > limit {
            self.warn(format!(
                "{label}: decompressed stream exceeds {} byte safety budget",
                limit
            ));
            return Ok(None);
        }
        self.expanded += v.len();
        Ok(Some(v))
    }

    fn search_zip(&mut self, label: String, bytes: &[u8], depth: usize) -> Result<()> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("invalid zip/container")?;
        let lower = label.to_ascii_lowercase();
        if lower.ends_with(".xlsx") {
            return self.search_xlsx(&label, &mut zip);
        }
        let docx = lower.ends_with(".docx");
        let pptx = lower.ends_with(".pptx");
        let epub = lower.ends_with(".epub");
        for i in 0..zip.len() {
            let mut f = match zip.by_index(i) {
                Ok(v) => v,
                Err(e) => {
                    self.warn(format!("{label}: cannot open zip member {i}: {e}"));
                    continue;
                }
            };
            if f.is_dir() {
                continue;
            }
            let name = f.name().to_string();
            let member = format!("{label}::{name}");
            let declared = match usize::try_from(f.size()) {
                Ok(v) => v,
                Err(_) => {
                    self.warn(format!("{member}: member size is not representable"));
                    continue;
                }
            };
            let Some(data) = self.read_zip_member(&member, &mut f, declared)? else {
                continue;
            };
            if docx && name.starts_with("word/") && name.ends_with(".xml") {
                if let Some(text) = office_text(&data) {
                    self.search_text(&member, &text);
                } else {
                    self.warn(format!("{member}: malformed XML"));
                }
            } else if pptx && name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                if let Some(text) = office_text(&data) {
                    let slide = digits_after(&name, "slide").unwrap_or(0);
                    self.search_text(&format!("{label}::slide={slide}"), &text);
                } else {
                    self.warn(format!("{member}: malformed XML"));
                }
            } else if epub
                && (name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm"))
            {
                if let Some(text) = generic_xml_text(&data) {
                    self.search_text(&member, &text);
                } else {
                    self.warn(format!("{member}: malformed markup"));
                }
            } else if let Err(e) = self.search_blob(member.clone(), &data, depth + 1) {
                self.warn(format!("{member}: {e:#}"));
            }
            if self.broken_pipe {
                break;
            }
        }
        Ok(())
    }

    fn search_xlsx<R: Read + io::Seek>(
        &mut self,
        label: &str,
        zip: &mut zip::ZipArchive<R>,
    ) -> Result<()> {
        let shared = match zip.by_name("xl/sharedStrings.xml") {
            Ok(mut f) => {
                let member = format!("{label}::xl/sharedStrings.xml");
                let declared = usize::try_from(f.size()).unwrap_or(usize::MAX);
                match self.read_zip_member(&member, &mut f, declared)? {
                    Some(data) => match xlsx_shared_strings(&data) {
                        Some(v) => v,
                        None => {
                            self.warn(format!("{member}: malformed sharedStrings XML"));
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                }
            }
            Err(zip::result::ZipError::FileNotFound) => Vec::new(),
            Err(e) => {
                self.warn(format!("{label}::xl/sharedStrings.xml: {e}"));
                Vec::new()
            }
        };

        let mut rid_to_target = HashMap::new();
        match zip.by_name("xl/_rels/workbook.xml.rels") {
            Ok(mut f) => {
                let member = format!("{label}::xl/_rels/workbook.xml.rels");
                let declared = usize::try_from(f.size()).unwrap_or(usize::MAX);
                if let Some(data) = self.read_zip_member(&member, &mut f, declared)? {
                    match workbook_relationships(&data) {
                        Some(v) => rid_to_target = v,
                        None => self.warn(format!("{member}: malformed relationships XML")),
                    }
                }
            }
            Err(zip::result::ZipError::FileNotFound) => {}
            Err(e) => self.warn(format!("{label}::xl/_rels/workbook.xml.rels: {e}")),
        }
        let mut target_to_sheet = HashMap::new();
        match zip.by_name("xl/workbook.xml") {
            Ok(mut f) => {
                let member = format!("{label}::xl/workbook.xml");
                let declared = usize::try_from(f.size()).unwrap_or(usize::MAX);
                if let Some(data) = self.read_zip_member(&member, &mut f, declared)? {
                    match workbook_sheets(&data) {
                        Some(sheets) => {
                            for (sheet, rid) in sheets {
                                if let Some(target) = rid_to_target.get(&rid) {
                                    let t = target.trim_start_matches('/');
                                    let normalized = if t.starts_with("xl/") {
                                        t.to_string()
                                    } else {
                                        format!("xl/{t}")
                                    };
                                    target_to_sheet.insert(normalized, sheet);
                                }
                            }
                        }
                        None => self.warn(format!("{member}: malformed workbook XML")),
                    }
                }
            }
            Err(zip::result::ZipError::FileNotFound) => {
                self.warn(format!("{label}: missing xl/workbook.xml"))
            }
            Err(e) => self.warn(format!("{label}::xl/workbook.xml: {e}")),
        }

        for i in 0..zip.len() {
            let mut f = match zip.by_index(i) {
                Ok(v) => v,
                Err(e) => {
                    self.warn(format!("{label}: cannot open xlsx member {i}: {e}"));
                    continue;
                }
            };
            let name = f.name().to_string();
            if !name.starts_with("xl/worksheets/") || !name.ends_with(".xml") || f.is_dir() {
                continue;
            }
            let member = format!("{label}::{name}");
            let declared = usize::try_from(f.size()).unwrap_or(usize::MAX);
            let Some(data) = self.read_zip_member(&member, &mut f, declared)? else {
                continue;
            };
            let sheet = target_to_sheet.get(&name).cloned().unwrap_or_else(|| {
                Path::new(&name)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            match xlsx_cells(&data, &shared) {
                Some(cells) => {
                    for (cell, value, is_formula) in cells {
                        let provenance = if is_formula {
                            format!("{label}::sheet={sheet}::cell={cell}::formula")
                        } else {
                            format!("{label}::sheet={sheet}::cell={cell}")
                        };
                        self.search_text(&provenance, &value);
                    }
                }
                None => self.warn(format!("{member}: malformed worksheet XML")),
            }
        }
        Ok(())
    }

    fn search_tar(&mut self, label: String, bytes: &[u8], depth: usize) -> Result<()> {
        let mut ar = Archive::new(Cursor::new(bytes));
        self.search_tar_archive(&label, &mut ar, depth)
    }

    fn search_decoded_tar<R: Read>(
        &mut self,
        label: String,
        r: &mut R,
        depth: usize,
    ) -> Result<()> {
        let mut ar = Archive::new(r);
        self.search_tar_archive(&label, &mut ar, depth)
    }

    fn search_tar_archive<R: Read>(
        &mut self,
        label: &str,
        ar: &mut Archive<R>,
        depth: usize,
    ) -> Result<()> {
        let entries = ar.entries()?;
        for result in entries {
            let mut ent = match result {
                Ok(v) => v,
                Err(e) => {
                    self.warn(format!("{label}: corrupt tar member: {e}"));
                    continue;
                }
            };
            if !ent.header().entry_type().is_file() {
                continue;
            }
            let name = match ent.path() {
                Ok(v) => v.to_string_lossy().into_owned(),
                Err(e) => {
                    self.warn(format!("{label}: invalid tar path: {e}"));
                    continue;
                }
            };
            let member = format!("{label}::{name}");
            let size = usize::try_from(ent.size()).unwrap_or(usize::MAX);
            if !self.reserve_expanded(&member, size) {
                continue;
            }
            let mut data = Vec::with_capacity(size.min(1 << 20));
            if let Err(e) = ent.read_to_end(&mut data) {
                self.warn(format!("{member}: {e}"));
                continue;
            }
            if let Err(e) = self.search_blob(member.clone(), &data, depth + 1) {
                self.warn(format!("{member}: {e:#}"));
            }
        }
        Ok(())
    }

    fn search_sqlite_path(&mut self, path: &Path, label: &str) -> Result<()> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        self.search_sqlite_connection(label, &conn)
    }

    fn search_sqlite_bytes(&mut self, label: &str, bytes: &[u8]) -> Result<()> {
        let tmp = std::env::temp_dir().join(format!(
            "deepsearch-{}-{}-{}.sqlite",
            std::process::id(),
            self.expanded,
            self.matches
        ));
        fs::write(&tmp, bytes)?;
        let result = (|| {
            let conn =
                Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            self.search_sqlite_connection(label, &conn)
        })();
        let _ = fs::remove_file(&tmp);
        result
    }

    fn search_sqlite_connection(&mut self, label: &str, conn: &Connection) -> Result<()> {
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_ENTRY_BYTES as i32);
        let mut q = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let mut tables = Vec::new();
        for result in q.query_map([], |r| r.get::<_, String>(0))? {
            match result {
                Ok(table) => tables.push(table),
                Err(e) => self.warn(format!("{label}: cannot decode SQLite table name: {e}")),
            }
        }
        drop(q);
        for table in tables {
            let qt = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let mut st = match conn.prepare(&qt) {
                Ok(v) => v,
                Err(e) => {
                    self.warn(format!("{label}::table={table}: {e}"));
                    continue;
                }
            };
            let names = st
                .column_names()
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>();
            let mut rows = st.query([])?;
            let mut n = 0usize;
            while let Some(row) = rows.next()? {
                n += 1;
                for (i, col) in names.iter().enumerate() {
                    let txt = match row.get_ref(i)? {
                        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
                        ValueRef::Integer(v) => v.to_string(),
                        ValueRef::Real(v) => v.to_string(),
                        _ => continue,
                    };
                    self.search_text(&format!("{label}::table={table}::row={n}::col={col}"), &txt);
                }
                if self.broken_pipe {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn search_pdf(&mut self, label: &str, bytes: &[u8]) -> Result<()> {
        let doc = lopdf::Document::load_mem(bytes).context("cannot parse PDF")?;
        let pages = doc.get_pages();
        for (page, _) in pages {
            match doc.extract_text(&[page]) {
                Ok(text) => self.search_text(&format!("{label}::page={page}"), &text),
                Err(e) => self.warn(format!(
                    "{label}::page={page}: PDF text extraction failed: {e}"
                )),
            }
        }
        Ok(())
    }

    fn search_text(&mut self, label: &str, text: &str) {
        let lines = text.lines().collect::<Vec<_>>();
        let mut last_end = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if self.rx.is_match(line) {
                self.matches += 1;
                let a = i.saturating_sub(self.context);
                let b = (i + self.context + 1).min(lines.len());
                if a > last_end && self.context > 0 && last_end > 0 {
                    self.emit("--");
                }
                let start = a.max(last_end);
                for (j, value) in lines.iter().enumerate().take(b).skip(start) {
                    self.emit_line(label, j + 1, value);
                }
                last_end = last_end.max(b);
            }
            if self.broken_pipe {
                return;
            }
        }
    }
}

fn read_prefix(path: &Path, n: usize) -> Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut v = vec![0u8; n];
    let got = f.read(&mut v)?;
    v.truncate(got);
    Ok(v)
}

fn is_containerish(label: &str, b: &[u8]) -> bool {
    is_zipish(label, b)
        || label.ends_with(".tar")
        || label.ends_with(".tar.gz")
        || label.ends_with(".tgz")
        || label.ends_with(".tar.xz")
        || label.ends_with(".txz")
        || label.ends_with(".tar.zst")
        || label.ends_with(".tzst")
        || label.ends_with(".gz")
        || label.ends_with(".xz")
        || label.ends_with(".zst")
        || label.ends_with(".zstd")
        || label.ends_with(".pdf")
        || b.starts_with(b"%PDF-")
}

fn trim_suffix(s: &str, suf: &str) -> String {
    if s.len() >= suf.len() && s[s.len() - suf.len()..].eq_ignore_ascii_case(suf) {
        s[..s.len() - suf.len()].to_string()
    } else {
        s.to_string()
    }
}

fn trim_suffix_any(s: &str, sufs: &[&str]) -> String {
    for x in sufs {
        let trimmed = trim_suffix(s, x);
        if trimmed.len() != s.len() {
            return trimmed;
        }
    }
    s.to_string()
}

fn is_zipish(label: &str, b: &[u8]) -> bool {
    label.ends_with(".zip")
        || label.ends_with(".docx")
        || label.ends_with(".xlsx")
        || label.ends_with(".pptx")
        || label.ends_with(".epub")
        || b.starts_with(b"PK\x03\x04")
}

fn is_sqliteish(label: &str, b: &[u8]) -> bool {
    label.ends_with(".sqlite")
        || label.ends_with(".sqlite3")
        || label.ends_with(".db")
        || b.starts_with(b"SQLite format 3\0")
}

fn looks_text(b: &[u8]) -> bool {
    if b.is_empty() {
        return true;
    }
    let sample = &b[..b.len().min(8192)];
    let bad = sample
        .iter()
        .filter(|&&c| c == 0 || c < 9 || (c > 13 && c < 32))
        .count();
    bad * 100 < sample.len().max(1) * 3
}

fn decode_unicode_text(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Some(String::from_utf8_lossy(&data[3..]).into_owned());
    }
    if data.starts_with(&[0xff, 0xfe]) {
        let u = data[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect::<Vec<_>>();
        return Some(String::from_utf16_lossy(&u));
    }
    if data.starts_with(&[0xfe, 0xff]) {
        let u = data[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect::<Vec<_>>();
        return Some(String::from_utf16_lossy(&u));
    }
    None
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

fn attr_value(e: &BytesStart<'_>, wanted: &[u8]) -> Result<Option<String>, ()> {
    let mut found = None;
    for a in e.attributes() {
        let a = a.map_err(|_| ())?;
        let key = a.key.as_ref();
        if key == wanted || (wanted == b"id" && key.ends_with(b":id")) {
            found = Some(String::from_utf8_lossy(a.value.as_ref()).into_owned());
        }
    }
    Ok(found)
}

fn office_text(data: &[u8]) -> Option<String> {
    let mut r = Reader::from_reader(data);
    r.config_mut().trim_text(false);
    let mut out = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Text(t)) => out.push_str(&unescape(&t.decode().ok()?).ok()?),
            Ok(Event::CData(t)) => out.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::Empty(e)) => match local_name(e.name().as_ref()) {
                b"br" => out.push('\n'),
                b"tab" => out.push('\t'),
                _ => {}
            },
            Ok(Event::End(e)) => match local_name(e.name().as_ref()) {
                b"p" | b"tr" => out.push('\n'),
                b"tc" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(out)
}

fn generic_xml_text(data: &[u8]) -> Option<String> {
    let mut r = Reader::from_reader(data);
    r.config_mut().trim_text(false);
    let mut out = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Text(t)) => out.push_str(&unescape(&t.decode().ok()?).ok()?),
            Ok(Event::CData(t)) => out.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::Empty(e)) if local_name(e.name().as_ref()) == b"br" => out.push('\n'),
            Ok(Event::End(e))
                if matches!(
                    local_name(e.name().as_ref()),
                    b"p" | b"div" | b"li" | b"tr" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6"
                ) =>
            {
                out.push('\n')
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(out)
}

fn xlsx_shared_strings(data: &[u8]) -> Option<Vec<String>> {
    let mut r = Reader::from_reader(data);
    r.config_mut().trim_text(false);
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if local_name(e.name().as_ref()) == b"si" => {
                current = Some(String::new())
            }
            Ok(Event::Text(t)) => {
                if let Some(s) = current.as_mut() {
                    s.push_str(&unescape(&t.decode().ok()?).ok()?);
                }
            }
            Ok(Event::End(e)) if local_name(e.name().as_ref()) == b"si" => {
                strings.push(current.take().unwrap_or_default());
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(strings)
}

fn workbook_relationships(data: &[u8]) -> Option<HashMap<String, String>> {
    let mut r = Reader::from_reader(data);
    let mut map = HashMap::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local_name(e.name().as_ref()) == b"Relationship" =>
            {
                let id = attr_value(&e, b"Id").ok()??;
                let target = attr_value(&e, b"Target").ok()??;
                map.insert(id, target);
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(map)
}

fn workbook_sheets(data: &[u8]) -> Option<Vec<(String, String)>> {
    let mut r = Reader::from_reader(data);
    let mut out = Vec::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local_name(e.name().as_ref()) == b"sheet" =>
            {
                let name = attr_value(&e, b"name").ok()??;
                let id = attr_value(&e, b"id").ok()??;
                out.push((name, id));
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(out)
}

fn xlsx_cells(data: &[u8], shared: &[String]) -> Option<Vec<(String, String, bool)>> {
    let mut r = Reader::from_reader(data);
    r.config_mut().trim_text(false);
    let mut out = Vec::new();
    let mut cell_ref = String::new();
    let mut cell_type = String::new();
    let mut value = String::new();
    let mut formula = String::new();
    let mut in_value = false;
    let mut in_formula = false;
    let mut in_cell = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if local_name(e.name().as_ref()) == b"c" => {
                in_cell = true;
                cell_ref = attr_value(&e, b"r")
                    .ok()?
                    .unwrap_or_else(|| "?".to_string());
                cell_type = attr_value(&e, b"t").ok()?.unwrap_or_default();
                value.clear();
                formula.clear();
            }
            Ok(Event::Start(e))
                if in_cell && matches!(local_name(e.name().as_ref()), b"v" | b"t") =>
            {
                in_value = true;
            }
            Ok(Event::Start(e)) if in_cell && local_name(e.name().as_ref()) == b"f" => {
                in_formula = true;
            }
            Ok(Event::Text(t)) if in_cell && in_formula => {
                formula.push_str(&unescape(&t.decode().ok()?).ok()?);
            }
            Ok(Event::Text(t)) if in_cell && in_value => {
                value.push_str(&unescape(&t.decode().ok()?).ok()?);
            }
            Ok(Event::End(e)) if matches!(local_name(e.name().as_ref()), b"v" | b"t") => {
                in_value = false;
            }
            Ok(Event::End(e)) if local_name(e.name().as_ref()) == b"f" => {
                in_formula = false;
            }
            Ok(Event::End(e)) if local_name(e.name().as_ref()) == b"c" => {
                if !formula.is_empty() {
                    out.push((cell_ref.clone(), formula.clone(), true));
                }
                let rendered = if cell_type == "s" {
                    value
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i).cloned())
                        .unwrap_or_else(|| value.clone())
                } else {
                    value.clone()
                };
                if !rendered.is_empty() {
                    out.push((cell_ref.clone(), rendered, false));
                }
                in_cell = false;
                in_value = false;
                in_formula = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    Some(out)
}

fn digits_after(s: &str, marker: &str) -> Option<usize> {
    let rest = s.rsplit(marker).next()?;
    let digits = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}
