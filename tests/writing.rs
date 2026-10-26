//! Producer behavior (Section 14.1) and the fixtures' producer checks.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{RawEntry, central_records, raw_zip, zip_of};
use mdzip_core::*;
use serde_json::json;

/// 2026-10-10T12:00:00Z
fn when() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_791_633_600)
}

fn code<T: std::fmt::Debug>(r: Result<T>) -> ErrorCode {
    r.expect_err("expected an error").code()
}

fn opts() -> CreateOptions {
    CreateOptions::new().when(when())
}

#[test]
fn p001_create_always_writes_a_full_manifest() {
    let archive = MdzArchive::open(create([("index.md", "# Guide\n")], &opts()).unwrap()).unwrap();
    let m = archive.manifest().unwrap();
    assert_eq!(m["spec"], json!({"name": "mdzip-spec", "version": "1.2.0"}));
    assert_eq!(m["entryPoint"], "index.md");
    assert_eq!(m["title"], "Guide");
    assert_eq!(m["mode"], "document");
    assert_eq!(m["producer"]["core"]["name"], "mdzip-core-rust");
    assert_eq!(m["created"], json!({"when": "2026-10-10T12:00:00Z"}));
    assert_eq!(m["modified"], json!({"when": "2026-10-10T12:00:00Z"}));
    assert!(archive.validate().is_empty(), "{:?}", archive.validate());
}

#[test]
fn application_is_recorded() {
    let o = opts().application(Application::new("Marco", "0.26.0").url("https://github.com/Ranrar/Marco"));
    let archive = MdzArchive::open(create([("document.md", "# Doc\n")], &o).unwrap()).unwrap();
    let app = &archive.manifest().unwrap()["producer"]["application"];
    assert_eq!(
        app,
        &json!({"name": "Marco", "version": "0.26.0", "url": "https://github.com/Ranrar/Marco"})
    );
    assert_eq!(archive.entry_point().unwrap(), "document.md");
}

#[test]
fn entry_point_discovery_sets_support_files_aside() {
    let data = create(
        [
            ("notes.md", "# Notes\n"),
            ("AGENTS.md", "# A\n"),
            ("README.md", "# R\n"),
        ],
        &opts(),
    )
    .unwrap();
    assert_eq!(
        MdzArchive::open(data).unwrap().manifest().unwrap()["entryPoint"],
        "notes.md"
    );
}

#[test]
fn ambiguous_entry_point_needs_an_explicit_one() {
    let files = [("a.md", "a"), ("b.md", "b")];
    assert_eq!(code(create(files, &opts())), ErrorCode::EntryPointUnresolved);
    let data = create(files, &opts().entry_point("b.md").mode(Mode::Project)).unwrap();
    let archive = MdzArchive::open(data).unwrap();
    assert_eq!(archive.entry_point().unwrap(), "b.md");
    assert_eq!(archive.mode(), Mode::Project);
}

#[test]
fn invalid_explicit_entry_point() {
    let files: [(&str, &[u8]); 2] = [("index.md", b"x"), ("cover.png", b"x")];
    assert_eq!(
        code(create(files, &opts().entry_point("cover.png"))),
        ErrorCode::EntryPointInvalid
    );
    assert_eq!(
        code(create(files, &opts().entry_point("nope.md"))),
        ErrorCode::EntryPointMissing
    );
}

#[test]
fn title_falls_back_to_the_file_name() {
    let archive = MdzArchive::open(create([("my-notes.md", "no heading")], &opts()).unwrap()).unwrap();
    assert_eq!(archive.title(), Some("my-notes"));
}

#[test]
fn p007_names_flags_and_methods() {
    let big = "repeated text ".repeat(100);
    let data = create(
        [
            ("index.md", "# I\n"),
            ("caf\u{e9}.md", "c"),
            ("images/a.png", "\u{89}PNG"),
            ("long.md", big.as_str()),
        ],
        &opts().entry_point("index.md"),
    )
    .unwrap();
    for (name, flags, method, attrs) in central_records(&data) {
        assert!(method == 0 || method == 8, "{name}: method {method}");
        assert!(name.is_ascii() || flags & 0x800 != 0, "{name}: UTF-8 flag");
        assert_eq!(attrs >> 16 & 0o170000, 0o100000, "{name}: a regular file");
        if name == "long.md" {
            assert_eq!(method, 8, "compressible files are deflated");
        }
    }
}

#[test]
fn paths_are_written_in_nfc() {
    let data = create([("cafe\u{301}.md", "# Caf\u{e9}\n")], &opts()).unwrap();
    assert!(central_records(&data).iter().any(|r| r.0 == "caf\u{e9}.md"));
}

#[test]
fn invalid_and_duplicate_paths_are_refused() {
    assert_eq!(
        code(create([("caf\u{e9}.md", "a"), ("cafe\u{301}.md", "b")], &opts())),
        ErrorCode::DuplicatePath
    );
    for bad in ["../x.md", "/x.md", "a\\b.md", "a:b.md"] {
        assert!(create([("index.md", "x"), (bad, "y")], &opts()).is_err(), "{bad}");
    }
    assert_eq!(
        code(create(
            [("index.md", "x"), ("docs", "a"), ("docs/intro.md", "b")],
            &opts()
        )),
        ErrorCode::PathConflict
    );
    assert_eq!(
        code(create([("index.md", "x"), ("docs/", "a")], &opts())),
        ErrorCode::PathInvalid
    );
}

#[test]
fn markdown_gets_lf_and_other_files_keep_their_line_endings() {
    let files = [
        ("index.md", "a\r\nb\r\n"),
        ("notes.markdown", "c\r\n"),
        ("data/table.csv", "a,b\r\n1,2\r\n"),
        ("run.bat", "@echo off\r\n"),
    ];
    let archive = MdzArchive::open(create(files, &opts().entry_point("index.md")).unwrap()).unwrap();
    assert_eq!(archive.read_bytes("index.md").unwrap(), b"a\nb\n");
    assert_eq!(archive.read_bytes("notes.markdown").unwrap(), b"c\n");
    assert_eq!(archive.read_bytes("data/table.csv").unwrap(), b"a,b\r\n1,2\r\n");
    assert_eq!(archive.read_bytes("run.bat").unwrap(), b"@echo off\r\n");
    assert!(!archive.read_text("manifest.json").unwrap().contains('\r'));

    let kept = MdzArchive::open(create([("index.md", "a\r\n")], &opts().keep_line_endings()).unwrap()).unwrap();
    assert_eq!(kept.read_bytes("index.md").unwrap(), b"a\r\n");
}

#[test]
fn extra_manifest_fields_are_kept() {
    let manifest = json!({"x-tool": {"a": [1, null]}, "language": "fr"})
        .as_object()
        .unwrap()
        .clone();
    let archive = MdzArchive::open(create([("index.md", "x")], &opts().manifest(manifest)).unwrap()).unwrap();
    let m = archive.manifest().unwrap();
    assert_eq!(m["x-tool"], json!({"a": [1, null]}));
    assert_eq!(m["language"], "fr");
}

#[test]
fn create_from_directory_with_filters_and_existing_manifest() {
    let dir = std::env::temp_dir().join(format!("mdzip-core-dir-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("images")).unwrap();
    std::fs::write(dir.join("index.md"), "# Home\n").unwrap();
    std::fs::write(dir.join("images/a.png"), "PNG").unwrap();
    std::fs::write(dir.join("notes.txt"), "skip me").unwrap();
    std::fs::write(dir.join("manifest.json"), r#"{"title": "From folder", "x-keep": true}"#).unwrap();
    let archive = MdzArchive::open(create_from_directory(&dir, None, &opts()).unwrap()).unwrap();
    let mut files: Vec<&str> = archive.files().collect();
    files.sort_unstable();
    assert_eq!(files, ["images/a.png", "index.md", "manifest.json"]);
    let m = archive.manifest().unwrap();
    assert_eq!(m["title"], "From folder");
    assert_eq!(m["x-keep"], true);
    assert_eq!(m["spec"]["version"], "1.2.0");
    let everything = MdzArchive::open(create_from_directory(&dir, Some(&["**"]), &opts()).unwrap()).unwrap();
    assert!(everything.contains("notes.txt"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Update ----

#[test]
fn p002_adds_a_manifest_to_an_archive_without_one() {
    let data = update(
        zip_of(&[("notes.md", "# N\n")]),
        &Update::new().write("AGENTS.md", "# A\n"),
    )
    .unwrap();
    let archive = MdzArchive::open(data).unwrap();
    assert_eq!(archive.manifest().unwrap()["entryPoint"], "notes.md");
    assert_eq!(archive.manifest().unwrap()["spec"]["version"], "1.2.0");
}

#[test]
fn p003_second_support_file_stays_resolvable() {
    let data = update(
        zip_of(&[("README.md", "# R\n")]),
        &Update::new().write("AGENTS.md", "# A\n"),
    )
    .unwrap();
    assert_eq!(MdzArchive::open(data).unwrap().entry_point().unwrap(), "README.md");
}

#[test]
fn p006_saving_a_manifest_less_archive_keeps_its_entry_point() {
    let source = zip_of(&[("notes.md", "# N\n")]);
    let data = update(source, &Update::new().write("notes.md", "# N\n\nEdited.\n")).unwrap();
    assert_eq!(MdzArchive::open(data).unwrap().entry_point().unwrap(), "notes.md");
}

#[test]
fn untouched_entries_keep_bytes_and_metadata() {
    let blob: Vec<u8> = (0..=255).collect();
    let source = create([("index.md", b"# I\n".as_slice()), ("data.bin", &blob)], &opts()).unwrap();
    let before = MdzArchive::open(source.clone())
        .unwrap()
        .entry_info("data.bin")
        .unwrap();
    let later = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let after =
        MdzArchive::open(update(source, &Update::new().write("index.md", "# Changed\n").when(later)).unwrap()).unwrap();
    assert_eq!(after.read_bytes("data.bin").unwrap(), blob);
    assert_eq!(after.entry_info("data.bin").unwrap(), before);
}

#[test]
fn untouched_crlf_markdown_is_not_normalized() {
    let source = zip_of(&[("index.md", "# I\r\n"), ("other.md", "keep\r\n")]);
    let after = MdzArchive::open(update(source, &Update::new().write("index.md", "# Changed\r\n")).unwrap()).unwrap();
    assert_eq!(after.read_bytes("index.md").unwrap(), b"# Changed\n");
    assert_eq!(after.read_bytes("other.md").unwrap(), b"keep\r\n");
}

#[test]
fn existing_spec_version_and_unknown_fields_are_kept() {
    let manifest = r#"{"spec": {"version": "1.1.0", "x": 1}, "entryPoint": "index.md", "x-top": [1], "modified": "2020-01-01T00:00:00Z"}"#;
    let source = zip_of(&[("manifest.json", manifest), ("index.md", "# I\n")]);
    let archive = MdzArchive::open(update(source, &Update::new().write("more.md", "m").when(when())).unwrap()).unwrap();
    let m = archive.manifest().unwrap();
    assert_eq!(m["spec"], json!({"version": "1.1.0", "x": 1}));
    assert_eq!(m["x-top"], json!([1]));
    assert_eq!(m["modified"], "2026-10-10T12:00:00Z", "the legacy string form is kept");
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    assert_eq!(keys, ["spec", "entryPoint", "x-top", "modified"], "field order is kept");
}

#[test]
fn removing_the_entry_point_is_refused_unless_a_new_one_is_named() {
    let source = create([("index.md", "# I\n"), ("other.md", "o")], &opts()).unwrap();
    assert_eq!(
        code(update(source.clone(), &Update::new().remove("index.md"))),
        ErrorCode::EntryPointChanged
    );
    let data = update(source, &Update::new().remove("index.md").entry_point("other.md")).unwrap();
    assert_eq!(MdzArchive::open(data).unwrap().entry_point().unwrap(), "other.md");
}

#[test]
fn manifest_as_a_file_is_refused_and_fields_can_be_unset() {
    let source = create(
        [("index.md", "x")],
        &opts().manifest(json!({"x-gone": 1}).as_object().unwrap().clone()),
    )
    .unwrap();
    assert_eq!(
        code(update(source.clone(), &Update::new().write("manifest.json", "{}"))),
        ErrorCode::ManifestInvalid
    );
    let data = update(source, &Update::new().unset("x-gone")).unwrap();
    assert!(
        !MdzArchive::open(data)
            .unwrap()
            .manifest()
            .unwrap()
            .contains_key("x-gone")
    );
}

#[test]
fn removing_something_missing_is_not_found() {
    let source = create([("index.md", "x")], &opts()).unwrap();
    assert_eq!(
        code(update(source, &Update::new().remove("nope.png"))),
        ErrorCode::NotFound
    );
}

fn with_symlink(name: &str) -> Vec<u8> {
    raw_zip(&[RawEntry::file("index.md", "# I\n"), RawEntry::symlink(name, "index.md")])
}

#[test]
fn symlinks_are_listed_but_never_files() {
    let archive = MdzArchive::open(with_symlink("link.md")).unwrap();
    assert_eq!(archive.special_files().collect::<Vec<_>>(), ["link.md"]);
    assert!(!archive.contains("link.md"));
}

#[test]
fn update_keeps_a_symlink_unchanged() {
    let data = update(with_symlink("link.md"), &Update::new().write("AGENTS.md", "# A\n")).unwrap();
    let record = central_records(&data).into_iter().find(|r| r.0 == "link.md").unwrap();
    assert_eq!(record.3 >> 16, 0o120777);
    assert_eq!(
        MdzArchive::open(data).unwrap().special_files().collect::<Vec<_>>(),
        ["link.md"]
    );
}

#[test]
fn symlinks_are_dropped_only_when_asked() {
    for changes in [
        Update::new().write("AGENTS.md", "# A\n").drop_special_files(),
        Update::new().write("AGENTS.md", "# A\n").remove("link.md"),
    ] {
        let data = update(with_symlink("link.md"), &changes).unwrap();
        assert!(!central_records(&data).iter().any(|r| r.0 == "link.md"));
    }
}

#[test]
fn writing_a_symlink_path_replaces_it_with_a_file() {
    let archive =
        MdzArchive::open(update(with_symlink("link.md"), &Update::new().write("link.md", "# Real\n")).unwrap())
            .unwrap();
    assert_eq!(archive.special_files().count(), 0);
    assert_eq!(archive.read_text("link.md").unwrap(), "# Real\n");
}

#[test]
fn a_kept_symlink_does_not_change_the_entry_point() {
    let data = update(with_symlink("other.md"), &Update::new().write("AGENTS.md", "# A\n")).unwrap();
    assert_eq!(MdzArchive::open(data).unwrap().entry_point().unwrap(), "index.md");
}

#[test]
fn update_keeps_directories_and_binary_entries_over_two_saves() {
    let source = raw_zip(&[
        RawEntry::file("index.md", "# Guide\n"),
        RawEntry::file("empty/", ""),
        RawEntry::file("docs/", ""),
        RawEntry::file("docs/a.md", "# A\n"),
        RawEntry::file("data/blob.bin", "\u{0}\u{1}\u{2}"),
    ]);
    let mut data = source;
    for _ in 0..2 {
        data = update(
            data,
            &Update::new().write("index.md", "# Guide\n\nEdited.\n").when(when()),
        )
        .unwrap();
    }
    let after = MdzArchive::open(data.clone()).unwrap();
    let mut dirs: Vec<&str> = after.directories().collect();
    dirs.sort_unstable();
    assert_eq!(dirs, ["docs/", "empty/"]);
    assert_eq!(after.read_bytes("data/blob.bin").unwrap(), b"\x00\x01\x02");
    let removed = MdzArchive::open(update(data, &Update::new().remove("empty")).unwrap()).unwrap();
    assert_eq!(removed.directories().collect::<Vec<_>>(), ["docs/"]);
}

#[test]
fn created_archives_open_in_the_python_shape() {
    // The manifest is ordered like the other cores write it.
    let archive = MdzArchive::open(create([("index.md", "# T\n")], &opts()).unwrap()).unwrap();
    let keys: Vec<&str> = archive.manifest().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["spec", "producer", "title", "mode", "entryPoint", "created", "modified"]
    );
}

#[test]
fn over_long_paths_are_refused() {
    let long = format!("{}.md", "a".repeat(70_000));
    assert_eq!(
        code(create([("index.md", "x"), (long.as_str(), "y")], &opts())),
        ErrorCode::PathInvalid
    );
}
