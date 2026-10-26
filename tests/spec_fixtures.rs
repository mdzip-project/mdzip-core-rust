//! The MDZip spec's shared conformance fixtures (tests/fixtures, copied from
//! a released spec tag with mdzip-spec's tools/sync-fixtures.mjs).

mod common;

use std::collections::HashMap;

use common::{RawEntry, fixtures, load, raw_zip};
use mdzip_core::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn open_fixture(name: &str) -> MdzArchive {
    MdzArchive::open_path(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn code_of<T: std::fmt::Debug>(result: Result<T>) -> &'static str {
    result.expect_err("expected an error").code().as_str()
}

#[test]
fn fixtures_match_marker() {
    let marker = load("SPEC-FIXTURES.json");
    for (name, expected) in marker["files"].as_object().unwrap() {
        let actual = sha256(&std::fs::read(fixtures().join(name)).unwrap());
        assert_eq!(
            actual,
            expected.as_str().unwrap(),
            "{name} differs from spec {}; re-run sync-fixtures.mjs",
            marker["ref"]
        );
    }
}

// ---- Prebuilt archives (T-001 to T-005, T-007) ----

#[test]
fn t001_t007_minimal_without_manifest() {
    let archive = open_fixture("minimal.mdz");
    assert_eq!(archive.entry_point().unwrap(), "index.md");
    assert!(archive.manifest().is_none());
    assert!(archive.validate().iter().all(|d| d.level != Level::Error));
}

#[test]
fn t002_with_manifest() {
    let archive = open_fixture("with-manifest.mdz");
    assert_eq!(
        archive.entry_point().unwrap(),
        archive.manifest().unwrap()["entryPoint"].as_str().unwrap()
    );
}

#[test]
fn t003_missing_entry_point_target() {
    assert_eq!(
        code_of(open_fixture("invalid-missing-index.mdz").entry_point()),
        "ERR_ENTRYPOINT_MISSING"
    );
}

#[test]
fn t004_bad_entry_point() {
    assert_eq!(
        code_of(open_fixture("invalid-bad-entrypoint.mdz").entry_point()),
        "ERR_ENTRYPOINT_MISSING"
    );
}

#[test]
fn t005_crlf() {
    let archive = open_fixture("line-endings-crlf.mdz");
    assert!(
        archive
            .read_text(&archive.entry_point().unwrap())
            .unwrap()
            .contains("\r\n")
    );
}

// ---- Case files ----

#[test]
fn t006_t008_entry_point_cases() {
    for case in load("entrypoint-cases.json").as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let manifest: Option<Manifest> = match case.get("manifestJson") {
            Some(text) => serde_json::from_str(text.as_str().unwrap()).unwrap(),
            None => case["manifest"].as_object().cloned(),
        };
        let files: Vec<&str> = case["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .filter(|f| *f != "manifest.json")
            .collect();
        let result = resolve_entry_point(files, manifest.as_ref());
        match case["expectedEntryPoint"].as_str() {
            Some(expected) => assert_eq!(result.as_deref(), Ok(expected), "{name}"),
            None => {
                let e = result.expect_err(name);
                if let Some(code) = case.get("expectedError") {
                    assert_eq!(e.code().as_str(), code.as_str().unwrap(), "{name}");
                }
            }
        }
    }
}

#[test]
fn t010_support_files_ignore_locale() {
    // Rust's ASCII case functions never consult the process locale.
    assert_eq!(
        resolve_entry_point(["LICENSE.md", "notes.md"], None).unwrap(),
        "notes.md"
    );
}

#[test]
fn path_validation_cases() {
    for case in load("path-validation-cases.json").as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        let name = case["name"].as_str().unwrap();
        let result = validate_path(path);
        if case["valid"].as_bool().unwrap() {
            assert!(result.is_ok(), "{name}: {result:?}");
        } else {
            assert_eq!(code_of(result), case["errorCode"].as_str().unwrap(), "{name}");
        }
    }
}

#[test]
fn path_validation_cases_on_open() {
    // The same names stored in real archives: the reader checks the stored name.
    for case in load("path-validation-cases.json").as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        let name = case["name"].as_str().unwrap();
        let mut entries = vec![RawEntry::file(path, "content\n")];
        if path != "index.md" {
            entries.insert(0, RawEntry::file("index.md", "# Index\n"));
        }
        let result = MdzArchive::open(raw_zip(&entries));
        if case["valid"].as_bool().unwrap() {
            assert!(result.is_ok(), "{name}: {:?}", result.err());
        } else {
            assert_eq!(code_of(result), case["errorCode"].as_str().unwrap(), "{name}");
        }
    }
}

#[test]
fn t009_path_matching_cases() {
    for case in load("path-matching-cases.json").as_array().unwrap() {
        let (entry, lookup) = (case["entry"].as_str().unwrap(), case["lookup"].as_str().unwrap());
        assert_eq!(
            same_path(entry, lookup),
            case["matches"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        // And through a real archive's lookup.
        let archive = MdzArchive::open(raw_zip(&[
            RawEntry::file("index.md", "# I\n"),
            RawEntry::file(entry, "x"),
        ]));
        if let Ok(archive) = archive {
            assert_eq!(
                archive.contains(lookup),
                case["matches"].as_bool().unwrap() || lookup == "index.md"
            );
        }
    }
}

#[test]
fn create_filter_cases() {
    for case in load("create-filter-cases.json").as_array().unwrap() {
        let filters: Vec<&str> = case["filters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        let strings = |key: &str| -> Vec<&str> {
            let mut v: Vec<&str> = case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .collect();
            v.sort_unstable();
            v
        };
        let mut included: Vec<&str> = strings("inputPaths")
            .into_iter()
            .filter(|p| matches_filters(p, &filters))
            .collect();
        included.sort_unstable();
        assert_eq!(included, strings("expectedIncluded"), "{}", case["name"]);
    }
}

#[test]
fn t013_entry_set_cases() {
    for case in load("entry-set-cases.json")["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let entries: Vec<&str> = case["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        let valid = case["valid"].as_bool().unwrap();
        let result = validate_entry_set(entries.iter().copied());
        if valid {
            assert!(result.is_ok(), "{name}");
        } else {
            assert_eq!(code_of(result), case["errorCode"].as_str().unwrap(), "{name}");
        }
        // The reader applies the same checks to a real archive.
        let mut raw: Vec<RawEntry> = entries
            .iter()
            .map(|e| RawEntry::file(e, if e.ends_with('/') { "" } else { "# x\n" }))
            .collect();
        raw.push(RawEntry::file(
            if entries.contains(&"index.md") {
                "other.md"
            } else {
                "index.md"
            },
            "# i\n",
        ));
        let opened = MdzArchive::open(raw_zip(&raw));
        if valid {
            assert!(opened.is_ok(), "{name}: {:?}", opened.err());
        } else {
            assert_eq!(code_of(opened), case["errorCode"].as_str().unwrap(), "{name}");
        }
    }
}

#[test]
fn t012_manifest_cases() {
    for case in load("manifest-cases.json")["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let expected = &case["expected"];
        let data = case["manifestJson"].as_str().unwrap().as_bytes();
        let result =
            parse_manifest(data).and_then(|p| validate_manifest(&p.manifest, true).map(|ignored| (p, ignored)));
        if !expected["valid"].as_bool().unwrap() {
            assert_eq!(code_of(result), expected["errorCode"].as_str().unwrap(), "{name}");
            continue;
        }
        let (parsed, mut ignored) = result.unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut want: Vec<&str> = expected
            .get("ignoredFields")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect())
            .unwrap_or_default();
        ignored.sort_unstable();
        want.sort_unstable();
        assert_eq!(ignored, want, "{name}: ignored fields");
        assert_eq!(
            !parsed.duplicate_members.is_empty(),
            expected
                .get("duplicateMembers")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "{name}: duplicate members"
        );
        let manifest = Value::Object(parsed.manifest);
        for (pointer, value) in expected.get("values").and_then(Value::as_object).into_iter().flatten() {
            assert_eq!(manifest.pointer(pointer), Some(value), "{name}: {pointer}");
        }
    }
}

// ---- T-011 link resolution ----

struct FixtureHost<'a> {
    files: Vec<&'a str>,
    directories: Vec<&'a str>,
    archives: HashMap<&'a str, (Vec<&'a str>, Option<&'a str>)>,
    host_location: bool,
    outside_links: bool,
    links_into_archives: bool,
    trust_root: Option<String>,
    allowed: Vec<&'a str>,
    load_outside: bool,
}

impl<'a> FixtureHost<'a> {
    fn new(model: &'a Value, context: Option<&'a Value>) -> Self {
        let strs =
            |v: &'a Value| -> Vec<&'a str> { v.as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect() };
        let get = |key: &str| context.and_then(|c| c.get(key));
        let flag = |key: &str, default: bool| get(key).and_then(Value::as_bool).unwrap_or(default);
        // A context trustRoot of null means "no trusted location"; absent means the model's default.
        let trust_root = match get("trustRoot").or_else(|| model.get("trustRoot")) {
            None => Some(String::new()),
            Some(Value::Null) => None,
            Some(v) => Some(v.as_str().unwrap().to_owned()),
        };
        FixtureHost {
            files: strs(&model["files"]),
            directories: strs(&model["directories"]),
            archives: model["archives"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), (strs(&v["entries"]), v["entryPoint"].as_str())))
                .collect(),
            host_location: flag("hostLocation", true),
            outside_links: flag("outsideLinks", true),
            links_into_archives: flag("intoArchiveLinks", true),
            trust_root,
            allowed: context
                .and_then(|c| c.get("allowedPaths"))
                .map(strs)
                .unwrap_or_default(),
            load_outside: flag("loadOutsideResources", false),
        }
    }
}

impl LinkHost for FixtureHost<'_> {
    fn kind(&self, path: &str) -> Option<HostKind> {
        if self.archives.contains_key(path) {
            Some(HostKind::Archive)
        } else if self.files.contains(&path) {
            Some(HostKind::File)
        } else if self.directories.contains(&path) {
            Some(HostKind::Directory)
        } else {
            None
        }
    }
    fn find_entry(&self, archive: &str, entry: &str) -> Option<String> {
        let (entries, _) = self.archives.get(archive)?;
        entries.iter().find(|e| same_path(e, entry)).map(|e| e.to_string())
    }
    fn entry_point(&self, archive: &str) -> Option<String> {
        self.archives.get(archive)?.1.map(str::to_owned)
    }
    fn host_location(&self) -> bool {
        self.host_location
    }
    fn outside_links(&self) -> bool {
        self.outside_links
    }
    fn links_into_archives(&self) -> bool {
        self.links_into_archives
    }
    fn trust_root(&self) -> Option<&str> {
        self.trust_root.as_deref()
    }
    fn is_allowed(&self, path: &str) -> bool {
        self.allowed.contains(&path)
    }
    fn load_outside_resources(&self) -> bool {
        self.load_outside
    }
}

/// A target in the shape the link fixture uses.
fn target_json(target: &LinkTarget) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), target.kind().into());
    let mut put = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            out.insert(k.into(), v.clone().into());
        }
    };
    match target {
        LinkTarget::Internal { entry, fragment } => {
            put("entry", &Some(entry.clone()));
            put("fragment", fragment);
        }
        LinkTarget::Host { path, fragment } => {
            put("path", &Some(path.clone()));
            put("fragment", fragment);
        }
        LinkTarget::Archive {
            archive,
            entry,
            fragment,
        } => {
            put("archive", &Some(archive.clone()));
            put("entry", &Some(entry.clone()));
            put("fragment", fragment);
        }
        LinkTarget::Url => {}
        LinkTarget::Unresolved(reason) => put("reason", &Some(reason.as_str().into())),
    }
    Value::Object(out)
}

#[test]
fn t011_link_resolution_cases() {
    let fixture = load("link-resolution-cases.json");
    for case in fixture["cases"].as_array().unwrap() {
        let from = &case["from"];
        let source = match from.get("archive") {
            Some(archive) => LinkSource::Archive {
                archive: archive.as_str().unwrap(),
                path: from["path"].as_str().unwrap(),
            },
            None => LinkSource::Host {
                host: from["host"].as_str().unwrap(),
            },
        };
        let host = FixtureHost::new(&fixture["host"], case.get("context"));
        let use_ = match case.get("use").and_then(Value::as_str) {
            Some("embed") => LinkUse::Embed,
            _ => LinkUse::Navigate,
        };
        let target = resolve_link(source, case["link"].as_str().unwrap(), &host, use_);
        assert_eq!(target_json(&target), case["expected"], "{}", case["name"]);
    }
}

// ---- P-004 / P-005 round trips ----

fn round_trip_flow(name: &str, data: Vec<u8>, round: u32) -> Vec<u8> {
    let archive = MdzArchive::open(data).unwrap();
    let changes = match name {
        "open and save without an edit" => Update::new(),
        "change a recognized manifest field" => Update::new().set("title", "Edited title"),
        "edit document content" => {
            Update::new().write("index.md", archive.read_text("index.md").unwrap() + "\nEdited.\n")
        }
        "add a resource" => Update::new().write(format!("images/added-{round}.png"), b"\x89PNG"),
        "remove a recognized resource" if round == 1 => {
            // Remove the file and its link, as a tool would (the flow lists index.md in changes).
            let index = archive
                .read_text("index.md")
                .unwrap()
                .replace("See [the guide](docs/guide.md).", "");
            Update::new().write("index.md", index).remove("docs/guide.md")
        }
        "remove a recognized resource" => Update::new().set("title", "Second save"),
        other => panic!("unknown flow {other}"),
    };
    archive.update(&changes).unwrap()
}

#[test]
fn p004_p005_round_trip_flows() {
    let exp = load("round-trip-expectations.json");
    let original = std::fs::read(fixtures().join("round-trip.mdz")).unwrap();
    for flow in exp["flows"].as_array().unwrap() {
        let name = flow["name"].as_str().unwrap();
        let listed = |key: &str| -> Vec<&str> {
            flow.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_str().unwrap()).collect())
                .unwrap_or_default()
        };
        let (changes, removes) = (listed("changes"), listed("removes"));
        let mut data = original.clone();
        for round in 1..=2 {
            data = round_trip_flow(name, data, round);
            let archive = MdzArchive::open(data.clone()).unwrap();
            let manifest = Value::Object(archive.manifest().unwrap().clone());
            for item in exp["preservedManifestValues"].as_array().unwrap() {
                let pointer = item["pointer"].as_str().unwrap();
                assert_eq!(
                    manifest.pointer(pointer),
                    Some(&item["value"]),
                    "{name}, save {round}: {pointer}"
                );
            }
            for path in &removes {
                assert!(!archive.contains(path), "{name}: {path} should be removed");
            }
            for entry in exp["preservedEntries"].as_array().unwrap() {
                let path = entry["path"].as_str().unwrap();
                if changes.contains(&path) || removes.contains(&path) {
                    continue;
                }
                let bytes = archive.read_bytes(path).unwrap();
                assert_eq!(bytes.len() as u64, entry["bytes"].as_u64().unwrap(), "{name}: {path}");
                assert_eq!(sha256(&bytes), entry["sha256"].as_str().unwrap(), "{name}: {path}");
            }
        }
    }
}

// ---- Archive cases (T-014, T-015, T-017, P-008) ----

fn archive_case(id: &str) -> Value {
    load("archive-cases.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}

#[test]
fn t017_symlink_entries() {
    let case = archive_case("symlink-entries");
    let archive = open_fixture(case["archive"].as_str().unwrap());
    assert_eq!(
        archive.entry_point().unwrap(),
        case["expectedEntryPoint"].as_str().unwrap()
    );
    let mut special: Vec<&str> = archive.special_files().collect();
    special.sort_unstable();
    let mut expected: Vec<&str> = case["symlinks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    expected.sort_unstable();
    assert_eq!(special, expected);
    let entry = archive.entry_point().unwrap();
    for link in case["links"].as_array().unwrap() {
        let href = link["href"].as_str().unwrap();
        let target = archive.resolve_link(&entry, href, LinkUse::Navigate);
        if link["expected"] == "resolved" {
            assert_eq!(
                target,
                LinkTarget::Internal {
                    entry: link["target"].as_str().unwrap().into(),
                    fragment: None
                },
                "{href}"
            );
        } else {
            // This core skips symbolic links, so "not-followed" is unresolved.
            assert!(matches!(target, LinkTarget::Unresolved(_)), "{href}: {target:?}");
        }
    }
    let reported: String = archive
        .validate()
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" ");
    for name in expected {
        assert!(reported.contains(&format!("'{name}'")), "{name} not reported");
    }
}

#[test]
fn p008_modify_archive_with_symlinks() {
    let source = open_fixture("symlink-entries.mdz");
    let kept = MdzArchive::open(
        source
            .update(&Update::new().write("notes.md", "# Notes\n\nEdited.\n"))
            .unwrap(),
    )
    .unwrap();
    let mut names: Vec<&str> = kept.special_files().collect();
    names.sort_unstable();
    assert_eq!(names, ["escape.md", "linked", "shortcut.md"]);
    assert!(
        !kept.validate().is_empty(),
        "kept symbolic links must still be reported"
    );
    let dropped = MdzArchive::open(
        source
            .update(&Update::new().write("notes.md", "# Notes\n").drop_special_files())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(dropped.special_files().count(), 0);
}

#[test]
fn t014_unsupported_compression() {
    let case = archive_case("unsupported-compression");
    let result = MdzArchive::open_path(fixtures().join(case["archive"].as_str().unwrap()))
        .and_then(|a| a.read_bytes("index.md"));
    assert_eq!(code_of(result), "ERR_ZIP_INVALID");
}

#[test]
fn t015_project_mode() {
    let case = archive_case("project-mode");
    let archive = open_fixture(case["archive"].as_str().unwrap());
    assert_eq!(archive.mode(), Mode::Project);
    assert_eq!(
        archive.entry_point().unwrap(),
        case["expectedEntryPoint"].as_str().unwrap()
    );
    let mut expected: Vec<&str> = case["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    expected.sort_unstable();
    assert_eq!(archive.documents(), expected);
}

// ---- T-018 ZIP-bomb limits ----

const STABLE: [&str; 2] = ["ERR_ZIP_INVALID", "ERR_LIMIT_EXCEEDED"];

fn limit_case(id: &str) -> Value {
    load("resource-limit-cases.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}

#[test]
fn t018_defaults_are_no_looser_than_the_reference() {
    let reference = &load("resource-limit-cases.json")["referenceLimits"];
    let limits = Limits::default();
    assert!(limits.max_entry_uncompressed <= reference["maxEntryUncompressedBytes"].as_u64().unwrap());
    assert!(limits.max_total_uncompressed <= reference["maxTotalUncompressedBytes"].as_u64().unwrap());
    assert!(limits.max_compression_ratio <= reference["maxCompressionRatio"].as_f64().unwrap());
    assert!(limits.ratio_applies_above <= reference["compressionRatioAppliesAboveBytes"].as_u64().unwrap());
    assert!(limits.max_entries <= reference["maxEntries"].as_u64().unwrap());
}

/// Open the archive and read the hostile entry; ordinary entries stay readable.
fn open_and_read(case: &Value) -> MdzError {
    let result = MdzArchive::open_path(fixtures().join(case["archive"].as_str().unwrap())).and_then(|archive| {
        assert!(!archive.read_text("index.md")?.is_empty());
        archive.read_bytes(case["entry"].as_str().unwrap())
    });
    let error = result.expect_err("hostile entry must fail");
    assert!(STABLE.contains(&error.code().as_str()), "{error}");
    error
}

#[test]
fn t018_forged_size() {
    open_and_read(&limit_case("limit-forged-size"));
}

#[test]
fn t018_high_ratio() {
    assert_eq!(
        open_and_read(&limit_case("limit-high-ratio")).code(),
        ErrorCode::LimitExceeded
    );
}

#[test]
fn t018_overlapping_entries() {
    let case = limit_case("limit-overlapping-entries");
    let error = MdzArchive::open_path(fixtures().join(case["archive"].as_str().unwrap())).expect_err("must fail");
    assert!(STABLE.contains(&error.code().as_str()), "{error}");
}
