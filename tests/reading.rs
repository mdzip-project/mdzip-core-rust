//! Reading behavior beyond the shared fixtures (Sections 4, 5, 13).

mod common;

use common::{RawEntry, raw_zip, zip_of};
use mdzip_core::*;

fn code<T: std::fmt::Debug>(r: Result<T>) -> ErrorCode {
    r.expect_err("expected an error").code()
}

#[test]
fn not_a_zip() {
    assert_eq!(code(MdzArchive::open(b"hello".to_vec())), ErrorCode::ZipInvalid);
    assert_eq!(code(MdzArchive::open(Vec::new())), ErrorCode::ZipInvalid);
}

#[test]
fn reads_text_bytes_and_metadata() {
    let archive = MdzArchive::open(zip_of(&[
        ("index.md", "\u{feff}# Hi\r\n"),
        ("images/a.png", "PNG"),
        ("docs/b.markdown", "b"),
    ]))
    .unwrap();
    assert_eq!(
        archive.read_text("index.md").unwrap(),
        "# Hi\r\n",
        "BOM removed, CRLF kept"
    );
    assert_eq!(archive.read_bytes("images/a.png").unwrap(), b"PNG");
    assert_eq!(archive.documents(), ["docs/b.markdown", "index.md"]);
    assert_eq!(archive.mode(), Mode::Document);
    assert_eq!(archive.title(), None);
    assert_eq!(code(archive.read_bytes("missing.md")), ErrorCode::NotFound);
    assert_eq!(archive.entry_info("images/a.png").unwrap().size, 3);
}

#[test]
fn lookups_are_nfc_and_case_sensitive() {
    let archive = MdzArchive::open(zip_of(&[("index.md", "x"), ("cafe\u{301}.md", "c")])).unwrap();
    assert_eq!(archive.find("caf\u{e9}.md"), Some("cafe\u{301}.md"));
    assert!(!archive.contains("Index.md"));
}

#[test]
fn text_that_is_not_utf8() {
    let archive = MdzArchive::open(raw_zip(&[RawEntry::file("index.md", [0xff, 0xfe, 0x00])])).unwrap();
    assert_eq!(code(archive.read_text("index.md")), ErrorCode::TextInvalid);
}

#[test]
fn corrupt_data_fails_the_read() {
    let mut data = zip_of(&[("index.md", "# Hello there\n")]);
    let at = data.windows(5).position(|w| w == b"Hello").unwrap();
    data[at] = b'J';
    let archive = MdzArchive::open(data).unwrap();
    assert_eq!(code(archive.read_bytes("index.md")), ErrorCode::ZipInvalid);
}

#[test]
fn encrypted_entries_are_rejected() {
    let mut data = zip_of(&[("index.md", "x")]);
    // Set general-purpose bit 0 in the central directory record.
    let cd = data
        .windows(4)
        .rposition(|w| w == 0x0201_4b50u32.to_le_bytes())
        .unwrap();
    data[cd + 8] |= 1;
    assert_eq!(code(MdzArchive::open(data)), ErrorCode::ZipEncrypted);
}

#[test]
fn overlapping_entries_are_rejected() {
    // Two central directory records point at the same local header and data.
    let mut data = zip_of(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let cd = data
        .windows(4)
        .rposition(|w| w == 0x0201_4b50u32.to_le_bytes())
        .unwrap();
    data[cd + 42..cd + 46].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(code(MdzArchive::open(data)), ErrorCode::ZipInvalid);
}

#[test]
fn custom_limits() {
    let data = zip_of(&[("index.md", "x"), ("a.md", "a"), ("b.md", "b")]);
    let few = Limits {
        max_entries: 2,
        ..Limits::default()
    };
    assert_eq!(
        code(MdzArchive::open_with_limits(data.clone(), few)),
        ErrorCode::LimitExceeded
    );
    let small = Limits {
        max_entry_uncompressed: 0,
        ..Limits::default()
    };
    assert_eq!(
        code(MdzArchive::open_with_limits(data, small)),
        ErrorCode::LimitExceeded
    );
}

#[test]
fn manifest_problems_fail_open() {
    let with = |m: &str| zip_of(&[("manifest.json", m), ("index.md", "x")]);
    assert_eq!(code(MdzArchive::open(with("not json"))), ErrorCode::ManifestInvalid);
    assert_eq!(code(MdzArchive::open(with("[1]"))), ErrorCode::ManifestInvalid);
    assert_eq!(
        code(MdzArchive::open(with(r#"{"spec":{"version":"2.0.0"}}"#))),
        ErrorCode::VersionUnsupported
    );
    assert_eq!(
        code(MdzArchive::open(with(r#"{"mode":"book"}"#))),
        ErrorCode::ModeUnsupported
    );
}

#[test]
fn bad_entry_point_keeps_the_rest_readable() {
    let archive = MdzArchive::open(zip_of(&[
        ("manifest.json", r#"{"entryPoint":"../x.md"}"#),
        ("index.md", "x"),
    ]))
    .unwrap();
    assert_eq!(code(archive.entry_point()), ErrorCode::EntryPointInvalid);
    assert_eq!(archive.read_text("index.md").unwrap(), "x");
    let errors: Vec<_> = archive
        .validate()
        .into_iter()
        .filter(|d| d.level == Level::Error)
        .collect();
    assert_eq!(errors[0].code, "ERR_ENTRYPOINT_INVALID");
}

#[test]
fn validate_reports_ignored_fields_duplicates_and_missing_cover() {
    let manifest =
        r#"{"spec":{"version":"1.2.0"},"entryPoint":"index.md","title":7,"cover":"images/c.png","title":""}"#;
    let archive = MdzArchive::open(zip_of(&[("manifest.json", manifest), ("index.md", "x")])).unwrap();
    let codes: Vec<&str> = archive.validate().iter().map(|d| d.code).collect();
    assert!(codes.contains(&"WARN_DUPLICATE_MEMBER"), "{codes:?}");
    assert!(codes.contains(&"WARN_FIELD_IGNORED"), "{codes:?}");
    assert!(codes.contains(&"WARN_COVER_MISSING"), "{codes:?}");
    assert_eq!(archive.ignored_fields(), ["title"]);
}

#[test]
fn no_manifest_is_info_only() {
    let archive = MdzArchive::open(zip_of(&[("notes.md", "x")])).unwrap();
    let found = archive.validate();
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].level, found[0].code), (Level::Info, "INFO_NO_MANIFEST"));
}

#[test]
fn directories_are_listed_separately() {
    let archive = MdzArchive::open(raw_zip(&[
        RawEntry::file("index.md", "# Guide\n"),
        RawEntry::file("empty/", ""),
        RawEntry::file("docs/", ""),
        RawEntry::file("docs/a.md", "# A\n"),
        RawEntry::file("data/blob.bin", "x"),
    ]))
    .unwrap();
    let mut dirs: Vec<&str> = archive.directories().collect();
    dirs.sort_unstable();
    assert_eq!(dirs, ["docs/", "empty/"]);
    assert!(!archive.files().any(|f| f == "empty/"));
    assert_eq!(archive.find_directory("empty"), Some("empty/"));
    assert_eq!(archive.find_directory("docs/"), Some("docs/"));
    assert_eq!(
        archive.find_directory("data"),
        None,
        "implied by a file path, not stored"
    );
    assert_eq!(archive.find_directory("Empty"), None, "case-sensitive");
}

#[test]
fn links_from_an_archive_opened_from_bytes() {
    let archive = MdzArchive::open(zip_of(&[("index.md", "x"), ("docs/a.md", "a")])).unwrap();
    let go = |link| archive.resolve_link("docs/a.md", link, LinkUse::Navigate);
    assert_eq!(
        go("../index.md#top"),
        LinkTarget::Internal {
            entry: "index.md".into(),
            fragment: Some("top".into())
        }
    );
    assert_eq!(go("https://mdzip.org"), LinkTarget::Url);
    assert_eq!(
        go("../../outside.md"),
        LinkTarget::Unresolved(UnresolvedReason::NoHostLocation)
    );
    assert_eq!(go("missing.md"), LinkTarget::Unresolved(UnresolvedReason::NotFound));
}

#[test]
fn links_from_an_archive_opened_from_a_file() {
    let dir = std::env::temp_dir().join(format!("mdzip-core-links-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("beside.md"), "# Beside\n").unwrap();
    std::fs::write(
        dir.join("other.mdz"),
        zip_of(&[("index.md", "# Other\n"), ("a.md", "a")]),
    )
    .unwrap();
    std::fs::write(dir.join("doc.mdz"), zip_of(&[("index.md", "# Doc\n")])).unwrap();
    let archive = MdzArchive::open_path(dir.join("doc.mdz")).unwrap();
    let go = |link| archive.resolve_link("index.md", link, LinkUse::Navigate);
    assert_eq!(
        go("../beside.md"),
        LinkTarget::Host {
            path: "beside.md".into(),
            fragment: None
        }
    );
    assert_eq!(
        go("../other.mdz/a.md"),
        LinkTarget::Archive {
            archive: "other.mdz".into(),
            entry: "a.md".into(),
            fragment: None
        }
    );
    assert_eq!(
        go("../other.mdz"),
        LinkTarget::Archive {
            archive: "other.mdz".into(),
            entry: "index.md".into(),
            fragment: None
        }
    );
    assert_eq!(
        archive.resolve_link("index.md", "../beside.md", LinkUse::Embed),
        LinkTarget::Unresolved(UnresolvedReason::Denied),
        "embeds don't load from outside the archive by default"
    );
    std::fs::remove_dir_all(&dir).ok();
}
