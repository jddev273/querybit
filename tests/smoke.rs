use flate2::{write::GzEncoder, Compression};
use rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{write::SimpleFileOptions, ZipWriter};

fn temp_dir() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("querybit-test-{}-{n}", std::process::id()));
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_querybit"))
        .args(args)
        .output()
        .unwrap()
}

fn zip_write(path: &Path, entries: &[(&str, String)]) {
    let file = fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    for (name, body) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn searches_mixed_inputs_and_honors_ignore() {
    let root = temp_dir();
    let needle = "QUERYBIT_SMOKE_9182";
    fs::write(
        root.join("plain.txt"),
        format!("alpha\n{needle} plain\nomega\n"),
    )
    .unwrap();
    fs::write(root.join("ignored.txt"), format!("{needle} ignored")).unwrap();
    fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();

    let zst = zstd::stream::encode_all(format!("{needle} zstd").as_bytes(), 3).unwrap();
    fs::write(root.join("compressed.txt.zst"), zst).unwrap();
    zip_write(
        &root.join("bundle.zip"),
        &[("inside.txt", format!("{needle} zip"))],
    );

    let db_path = root.join("sample.sqlite");
    let db = Connection::open(&db_path).unwrap();
    db.execute("CREATE TABLE notes(body TEXT)", []).unwrap();
    db.execute(
        "INSERT INTO notes(body) VALUES (?1)",
        [format!("{needle} sqlite")],
    )
    .unwrap();
    drop(db);

    let output = run(&["-F", needle, root.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("plain.txt"));
    assert!(stdout.contains("bundle.zip::inside.txt"));
    assert!(stdout.contains("sample.sqlite::table=notes"));
    assert!(stdout.contains("compressed.txt"));
    assert!(!stdout.contains("ignored.txt"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_path_is_error_not_clean_no_match() {
    let root = temp_dir();
    let missing = root.join("does-not-exist");
    let output = run(&["-F", "needle", missing.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("path does not exist"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn searches_plain_file_larger_than_64_mib() {
    let root = temp_dir();
    let needle = "LARGE_FILE_NEEDLE_271828";
    let path = root.join("large.log");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(file, "{needle}").unwrap();
    let line = vec![b'x'; 1023];
    for _ in 0..66_000 {
        file.write_all(&line).unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    assert!(fs::metadata(&path).unwrap().len() > 64 * 1024 * 1024);
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(needle));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn searches_utf16le_bom_text() {
    let root = temp_dir();
    let needle = "UTF16_NEEDLE_314159";
    let mut bytes = vec![0xff, 0xfe];
    for u in format!("{needle} hello\n").encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let path = root.join("utf16.txt");
    fs::write(&path, bytes).unwrap();
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn top_level_binary_is_not_treated_as_text() {
    let root = temp_dir();
    let needle = "BINARY_NEEDLE_424242";
    let path = root.join("payload.bin");
    let mut bytes = vec![0u8, 1, 2, 0, 255, 254, 0];
    bytes.extend_from_slice(needle.as_bytes());
    bytes.extend_from_slice(&[0, 3, 4, 0]);
    fs::write(&path, bytes).unwrap();
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn docx_adjacent_runs_are_searchable_as_visible_text() {
    let root = temp_dir();
    let needle = "JOINED_WORD_161803";
    let path = root.join("split.docx");
    zip_write(
        &path,
        &[(
            "word/document.xml",
            "<w:document xmlns:w=\"x\"><w:p><w:r><w:t>JOINED_</w:t></w:r><w:r><w:t>WORD_161803</w:t></w:r></w:p></w:document>".to_string(),
        )],
    );
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(needle));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn xlsx_shared_string_reports_sheet_and_cell() {
    let root = temp_dir();
    let needle = "XLSX_NEEDLE_141421";
    let path = root.join("semantic.xlsx");
    zip_write(
        &path,
        &[
            (
                "xl/sharedStrings.xml",
                format!("<sst><si><t>{needle}</t></si></sst>"),
            ),
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Forecast\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row r=\"1\"><c r=\"B17\" t=\"s\"><v>0</v></c></row></sheetData></worksheet>".to_string(),
            ),
        ],
    );
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sheet=Forecast::cell=B17"), "{stdout}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn xlsx_formula_is_searchable_with_formula_provenance() {
    let root = temp_dir();
    let needle = "SUM(B1:B2)";
    let path = root.join("formula.xlsx");
    zip_write(
        &path,
        &[
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Calc\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row r=\"1\"><c r=\"A1\"><f>SUM(B1:B2)</f><v>3</v></c></row></sheetData></worksheet>".to_string(),
            ),
        ],
    );
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sheet=Calc::cell=A1::formula"), "{stdout}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sqlite_wal_committed_row_is_visible() {
    let root = temp_dir();
    let needle = "WAL_NEEDLE_173205";
    let path = root.join("wal.sqlite");
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    db.execute("CREATE TABLE notes(body TEXT)", []).unwrap();
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    db.execute("INSERT INTO notes(body) VALUES (?1)", [needle])
        .unwrap();
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("table=notes"));
    drop(db);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn pdf_text_has_page_provenance() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample.pdf");
    let output = run(&["-F", "NEEDLE_7429", path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("::page=1"), "{stdout}");
    assert!(stdout.contains("NEEDLE_7429"), "{stdout}");
}

#[test]
fn corrupt_pdf_is_incomplete_not_no_match() {
    let root = temp_dir();
    let path = root.join("broken.pdf");
    fs::write(&path, b"%PDF-1.7\nnot a valid PDF body\n").unwrap();
    let output = run(&["-F", "ABSENT_PDF_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot parse PDF"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn pptx_reports_correct_slide_number() {
    let root = temp_dir();
    let needle = "PPTX_NEEDLE_223606";
    let path = root.join("slides.pptx");
    zip_write(
        &path,
        &[(
            "ppt/slides/slide12.xml",
            format!("<p:sld xmlns:p=\"p\" xmlns:a=\"a\"><a:p><a:r><a:t>{needle}</a:t></a:r></a:p></p:sld>"),
        )],
    );
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("slide=12"), "{stdout}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn uppercase_zstd_suffix_is_decoded_once() {
    let root = temp_dir();
    let needle = "UPPERCASE_ZSTD_NEEDLE_161803";
    let bytes = zstd::stream::encode_all(format!("{needle}\n").as_bytes(), 3).unwrap();
    let path = root.join("sample.ZST");
    fs::write(&path, bytes).unwrap();
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(needle));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn epub_inline_markup_preserves_visible_adjacency() {
    let root = temp_dir();
    let needle = "EPUB_JOINED_WORD_271828";
    let path = root.join("inline.epub");
    zip_write(
        &path,
        &[(
            "OEBPS/ch1.xhtml",
            "<html><body><p>EPUB_JOINED_<em>WORD_271828</em></p></body></html>".to_string(),
        )],
    );
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(needle));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn malformed_xlsx_shared_strings_is_incomplete_not_no_match() {
    let root = temp_dir();
    let path = root.join("bad-shared.xlsx");
    zip_write(&path, &[
        ("xl/sharedStrings.xml", "<sst><si><t>MALFORMED_SHARED_NEEDLE</si></sst>".to_string()),
        ("xl/workbook.xml", "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\"/></sheets></workbook>".to_string()),
        ("xl/_rels/workbook.xml.rels", "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string()),
        ("xl/worksheets/sheet1.xml", "<worksheet><sheetData><row><c r=\"A1\" t=\"s\"><v>0</v></c></row></sheetData></worksheet>".to_string()),
    ]);
    let output = run(&["-F", "MALFORMED_SHARED_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed sharedStrings"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn oversized_top_level_container_is_rejected_before_read() {
    let root = temp_dir();
    let path = root.join("oversized.zip");
    let file = fs::File::create(&path).unwrap();
    file.set_len(513 * 1024 * 1024).unwrap();
    drop(file);
    let output = run(&["-F", "ABSENT_CONTAINER_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("top-level parser safety limit"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn duplicate_xlsx_attribute_is_incomplete_not_silent() {
    let root = temp_dir();
    let path = root.join("duplicate-attr.xlsx");
    zip_write(
        &path,
        &[
            (
                "xl/sharedStrings.xml",
                "<sst><si><t>DUP_ATTR_NEEDLE_808080</t></si></sst>".to_string(),
            ),
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row><c r=\"A1\" t=\"s\" t=\"str\"><v>0</v></c></row></sheetData></worksheet>".to_string(),
            ),
        ],
    );
    let output = run(&["-F", "DUP_ATTR_NEEDLE_808080", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("malformed worksheet XML"), "{stderr}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn oversized_logical_line_is_explicitly_incomplete() {
    let root = temp_dir();
    let path = root.join("huge-line.txt");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(&vec![b'a'; 17 * 1024 * 1024]).unwrap();
    drop(file);
    let output = run(&["-F", "ABSENT_LINE_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("logical line exceeds"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn forged_zip_size_metadata_is_not_a_clean_search() {
    let root = temp_dir();
    let path = root.join("forged.zip");
    let needle = "FORGED_SIZE_NEEDLE_789";
    let mut body = "A".repeat(2 * 1024 * 1024);
    body.push_str(needle);
    zip_write(&path, &[("huge.txt", body)]);
    let mut bytes = fs::read(&path).unwrap();
    let fake = 1024u32.to_le_bytes();
    let mut saw_local = false;
    let mut saw_central = false;
    for i in 0..bytes.len().saturating_sub(30) {
        if &bytes[i..i + 4] == b"PK\x03\x04" {
            bytes[i + 22..i + 26].copy_from_slice(&fake);
            saw_local = true;
        } else if &bytes[i..i + 4] == b"PK\x01\x02" {
            bytes[i + 24..i + 28].copy_from_slice(&fake);
            saw_central = true;
        }
    }
    assert!(saw_local && saw_central);
    fs::write(&path, bytes).unwrap();
    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("size metadata mismatch"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn extracted_context_does_not_duplicate_overlaps() {
    let root = temp_dir();
    let path = root.join("ctx.zip");
    zip_write(&path, &[("ctx.txt", "hit\nhit\nthird\n".to_string())]);
    let output = run(&["-F", "hit", "-C", "1", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("::ctx.txt:1:").count(), 1, "{stdout}");
    assert_eq!(stdout.matches("::ctx.txt:2:").count(), 1, "{stdout}");
    assert_eq!(stdout.matches("::ctx.txt:3:").count(), 1, "{stdout}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn non_sqlite_db_file_does_not_make_directory_search_incomplete() {
    let root = temp_dir();
    let needle = "JUNK_DB_NEEDLE_314159";
    fs::write(root.join("chrome.db"), b"junk").unwrap();
    fs::write(root.join("hit.txt"), format!("{needle}\n")).unwrap();

    let output = run(&["-F", needle, root.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hit.txt"));
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("file is not a database"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sqlite_is_detected_by_magic_without_database_extension() {
    let root = temp_dir();
    let needle = "SQLITE_MAGIC_NEEDLE_271828";
    let path = root.join("database.bin");
    let db = Connection::open(&path).unwrap();
    db.execute("CREATE TABLE notes(body TEXT)", []).unwrap();
    db.execute("INSERT INTO notes(body) VALUES (?1)", [needle])
        .unwrap();
    drop(db);

    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("database.bin::table=notes"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn committed_xlsx_fixture_is_searchable() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sample.xlsx");
    let output = run(&["-F", "NEEDLE_7429", path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sheet=Sheet1"), "{stdout}");
    assert!(stdout.contains("cell=A1"), "{stdout}");
}

#[test]
fn nested_sqlite_in_zip_is_searchable() {
    let root = temp_dir();
    let needle = "NESTED_SQLITE_NEEDLE_577215";
    let db_path = root.join("source.sqlite");
    let db = Connection::open(&db_path).unwrap();
    db.execute("CREATE TABLE notes(body TEXT)", []).unwrap();
    db.execute("INSERT INTO notes(body) VALUES (?1)", [needle])
        .unwrap();
    drop(db);
    let db_bytes = fs::read(&db_path).unwrap();
    fs::remove_file(&db_path).unwrap();

    let zip_path = root.join("bundle.zip");
    let file = fs::File::create(&zip_path).unwrap();
    let mut zip = ZipWriter::new(file);
    zip.start_file("inside.sqlite", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&db_bytes).unwrap();
    zip.finish().unwrap();

    let output = run(&["-F", needle, zip_path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("bundle.zip::inside.sqlite::table=notes"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn xml_entities_are_preserved_in_docx_epub_and_xlsx() {
    let root = temp_dir();
    let needle = "AT&T NEEDLE";

    let docx = root.join("entities.docx");
    zip_write(
        &docx,
        &[(
            "word/document.xml",
            "<w:document xmlns:w=\"x\"><w:p><w:r><w:t>AT&amp;T &#78;EEDLE</w:t></w:r></w:p></w:document>".to_string(),
        )],
    );
    let epub = root.join("entities.epub");
    zip_write(
        &epub,
        &[(
            "OEBPS/ch1.xhtml",
            "<html><body><p>AT&amp;T &#78;EEDLE</p></body></html>".to_string(),
        )],
    );
    let xlsx = root.join("entities.xlsx");
    zip_write(
        &xlsx,
        &[
            (
                "xl/sharedStrings.xml",
                "<sst><si><t>AT&amp;T &#78;EEDLE</t></si></sst>".to_string(),
            ),
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row><c r=\"A1\" t=\"s\"><v>0</v></c></row></sheetData></worksheet>".to_string(),
            ),
        ],
    );

    for path in [&docx, &epub, &xlsx] {
        let output = run(&["-F", needle, path.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn concatenated_gzip_searches_later_members() {
    let root = temp_dir();
    let path = root.join("concat.txt.gz");
    let needle = "SECOND_GZIP_MEMBER_NEEDLE_4242";
    let encode = |body: &[u8]| {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).unwrap();
        encoder.finish().unwrap()
    };
    let mut bytes = encode(b"first member without it\n");
    bytes.extend_from_slice(&encode(format!("{needle}\n").as_bytes()));
    fs::write(&path, bytes).unwrap();

    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(needle));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn truncated_docx_xml_is_incomplete() {
    let root = temp_dir();
    let path = root.join("truncated.docx");
    zip_write(
        &path,
        &[(
            "word/document.xml",
            "<w:document xmlns:w=\"x\"><w:p><w:r><w:t>TRUNCATED_XML_NEEDLE".to_string(),
        )],
    );
    let output = run(&["-F", "TRUNCATED_XML_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed XML"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unresolved_xlsx_shared_string_is_incomplete() {
    let root = temp_dir();
    let path = root.join("missing-shared.xlsx");
    zip_write(
        &path,
        &[
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                "<worksheet><sheetData><row><c r=\"A1\" t=\"s\"><v>0</v></c></row></sheetData></worksheet>".to_string(),
            ),
        ],
    );
    let output = run(&["-F", "ABSENT_SHARED_STRING", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed worksheet XML"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn corrupted_tar_gzip_trailer_is_incomplete() {
    let root = temp_dir();
    let path = root.join("corrupt.tar.gz");
    let needle = "TAR_GZIP_CHECKSUM_NEEDLE_31337";

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let body = format!("{needle}\n");
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "inside.txt", body.as_bytes())
            .unwrap();
        builder.finish().unwrap();
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let mut gzip = encoder.finish().unwrap();
    let crc_index = gzip.len() - 8;
    gzip[crc_index] ^= 0x01;
    fs::write(&path, gzip).unwrap();

    let output = run(&["-F", needle, path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checksum") || stderr.contains("integrity"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn truncated_xlsx_metadata_is_incomplete_not_no_match() {
    let root = temp_dir();
    let sheet = "<worksheet><sheetData><row><c r=\"A1\" t=\"inlineStr\"><is><t>harmless</t></is></c></row></sheetData></worksheet>";

    let workbook_path = root.join("truncated-workbook.xlsx");
    zip_write(
        &workbook_path,
        &[
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\">".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            ("xl/worksheets/sheet1.xml", sheet.to_string()),
        ],
    );
    let output = run(&[
        "-F",
        "ABSENT_TRUNCATED_WORKBOOK_NEEDLE",
        workbook_path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("malformed workbook XML"), "{stderr}");

    let rels_path = root.join("truncated-workbook-rels.xlsx");
    zip_write(
        &rels_path,
        &[
            (
                "xl/workbook.xml",
                "<workbook xmlns:r=\"r\"><sheets><sheet name=\"Sheet1\" r:id=\"rId1\"/></sheets></workbook>".to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\">".to_string(),
            ),
            ("xl/worksheets/sheet1.xml", sheet.to_string()),
        ],
    );
    let output = run(&[
        "-F",
        "ABSENT_TRUNCATED_RELS_NEEDLE",
        rels_path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workbook.xml.rels") && stderr.contains("malformed"),
        "{stderr}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn oversized_compressed_tar_integrity_tail_is_incomplete() {
    let root = temp_dir();
    let path = root.join("oversized-tail.tar.gz");

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let body = b"harmless\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "inside.txt", &body[..])
            .unwrap();
        builder.finish().unwrap();
    }

    let file = fs::File::create(&path).unwrap();
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..65 {
        encoder.write_all(&zeros).unwrap();
    }
    encoder.finish().unwrap();

    let output = run(&[
        "-F",
        "ABSENT_OVERSIZED_TAR_TAIL_NEEDLE",
        path.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("integrity tail"), "{stderr}");
    assert!(stderr.contains("safety budget"), "{stderr}");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn small_compressed_tar_integrity_tail_preserves_clean_no_match() {
    let root = temp_dir();
    let path = root.join("small-tail.tar.gz");

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let body = b"harmless\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "inside.txt", &body[..])
            .unwrap();
        builder.finish().unwrap();
    }

    let file = fs::File::create(&path).unwrap();
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.write_all(&vec![0u8; 1024 * 1024]).unwrap();
    encoder.finish().unwrap();

    let output = run(&["-F", "ABSENT_SMALL_TAR_TAIL_NEEDLE", path.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let _ = fs::remove_dir_all(root);
}
