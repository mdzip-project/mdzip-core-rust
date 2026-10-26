//! `manifest.json` handling (Section 6).
//!
//! A manifest is kept as a [`serde_json::Map`] in its original field order,
//! so every field, including ones this library doesn't know, survives a
//! read, modify and write cycle (Section 6.3).

use std::cell::RefCell;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::error::{ErrorCode, MdzError, Result};
use crate::paths::{cover_problem, entry_point_problem};

/// A parsed `manifest.json`: the top-level JSON object, with field order kept.
pub type Manifest = Map<String, Value>;

/// The spec name written in `spec.name`.
pub const SPEC_NAME: &str = "mdzip-spec";
/// The MDZip specification version this library implements and writes.
pub const SPEC_VERSION: &str = "1.2.0";
/// The newest spec major version this library reads (Section 11.2).
pub const SUPPORTED_MAJOR: u64 = 1;

/// How an archive is interpreted (Section 5.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// One logical document, possibly split across several Markdown files.
    #[default]
    Document,
    /// Several related documents (Section 5.7.3).
    Project,
}

impl Mode {
    /// `"document"` or `"project"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Document => "document",
            Mode::Project => "project",
        }
    }

    /// Parse a manifest `mode` value.
    pub fn parse(value: &str) -> Option<Mode> {
        match value {
            "document" => Some(Mode::Document),
            "project" => Some(Mode::Project),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed manifest and the names of members it repeats (Section 6.4).
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedManifest {
    /// The manifest. For a repeated member, the last occurrence wins.
    pub manifest: Manifest,
    /// Each repeated member name, at any nesting level, once per repeat.
    pub duplicate_members: Vec<String>,
}

/// Parse `manifest.json` bytes (UTF-8 JSON, optional BOM).
///
/// Fails with [`ErrorCode::ManifestInvalid`] if the bytes aren't UTF-8 JSON
/// or the top-level value isn't an object. Repeated members are reported in
/// [`ParsedManifest::duplicate_members`]; the last one is kept.
pub fn parse_manifest(data: &[u8]) -> Result<ParsedManifest> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    let text = std::str::from_utf8(data).map_err(|e| {
        MdzError::new(
            ErrorCode::ManifestInvalid,
            format!("manifest.json is not valid UTF-8: {e}"),
        )
    })?;
    let duplicates = RefCell::new(Vec::new());
    let mut de = serde_json::Deserializer::from_str(text);
    let value = TrackDuplicates(&duplicates)
        .deserialize(&mut de)
        .and_then(|v| de.end().map(|_| v))
        .map_err(|e| {
            MdzError::new(
                ErrorCode::ManifestInvalid,
                format!("manifest.json is not valid JSON: {e}"),
            )
        })?;
    match value {
        Value::Object(manifest) => Ok(ParsedManifest {
            manifest,
            duplicate_members: duplicates.into_inner(),
        }),
        _ => Err(MdzError::new(
            ErrorCode::ManifestInvalid,
            "manifest.json must be a JSON object",
        )),
    }
}

/// Deserializes a JSON value like `serde_json::Value` does, recording
/// repeated object members.
struct TrackDuplicates<'a>(&'a RefCell<Vec<String>>);

impl<'de> DeserializeSeed<'de> for TrackDuplicates<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TrackDuplicates<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("number out of range"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(TrackDuplicates(self.0))? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(TrackDuplicates(self.0))?;
            if out.contains_key(&key) {
                self.0.borrow_mut().push(key.clone());
            }
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

/// Serialize a manifest as UTF-8 JSON with LF line endings, keeping field order.
pub fn serialize_manifest(manifest: &Manifest) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(manifest).expect("a JSON map always serializes");
    out.push(b'\n');
    out
}

fn describe(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// The archive's mode (Section 5.7): `document` when `mode` is absent.
///
/// Fails with [`ErrorCode::ManifestInvalid`] for a non-string value and
/// [`ErrorCode::ModeUnsupported`] for an unrecognized one (Section 5.7.5).
pub fn manifest_mode(manifest: Option<&Manifest>) -> Result<Mode> {
    let Some(value) = manifest.and_then(|m| m.get("mode")) else {
        return Ok(Mode::Document);
    };
    let Value::String(text) = value else {
        return Err(MdzError::new(
            ErrorCode::ManifestInvalid,
            format!("mode must be a string, not {}", describe(value)),
        ));
    };
    Mode::parse(text).ok_or_else(|| {
        MdzError::new(
            ErrorCode::ModeUnsupported,
            format!("unrecognized mode '{text}'; supported: document, project"),
        )
    })
}

/// The manifest's `spec.version`, if it has one.
pub fn spec_version(manifest: Option<&Manifest>) -> Option<&str> {
    manifest?.get("spec")?.get("version")?.as_str()
}

/// Reject a `spec` or `spec.version` with the wrong type, a version that
/// isn't semantic versioning, or a newer major version than this library
/// supports (Section 11.2). A missing version is read as 1.x (Section 11.3).
pub fn check_spec_version(manifest: Option<&Manifest>) -> Result<()> {
    let Some(spec) = manifest.and_then(|m| m.get("spec")) else {
        return Ok(());
    };
    let Value::Object(spec) = spec else {
        return Err(MdzError::new(
            ErrorCode::ManifestInvalid,
            format!("spec must be an object, not {}", describe(spec)),
        ));
    };
    let Some(version) = spec.get("version") else {
        return Ok(());
    };
    let Value::String(version) = version else {
        return Err(MdzError::new(
            ErrorCode::ManifestInvalid,
            format!("spec.version must be a string, not {}", describe(version)),
        ));
    };
    let Some(major) = semver_major(version) else {
        return Err(MdzError::new(
            ErrorCode::ManifestInvalid,
            format!("spec.version '{version}' is not a semantic version"),
        ));
    };
    if major > SUPPORTED_MAJOR {
        return Err(MdzError::new(
            ErrorCode::VersionUnsupported,
            format!("archive targets spec {version}; this library supports {SUPPORTED_MAJOR}.x"),
        ));
    }
    Ok(())
}

/// Validate a parsed manifest (Section 6.4) and return the descriptive
/// fields to ignore because they have the wrong type or an invalid value.
///
/// Fails for a critical field with a wrong type or value: `spec` and
/// `spec.version` ([`ErrorCode::ManifestInvalid`], or
/// [`ErrorCode::VersionUnsupported`] for a newer major), `mode`
/// ([`ErrorCode::ManifestInvalid`] or [`ErrorCode::ModeUnsupported`]) and,
/// when `check_entry_point` is set, `entryPoint`
/// ([`ErrorCode::EntryPointInvalid`]). Unrecognized fields are never checked.
pub fn validate_manifest(manifest: &Manifest, check_entry_point: bool) -> Result<Vec<&'static str>> {
    check_spec_version(Some(manifest))?;
    manifest_mode(Some(manifest))?;
    if check_entry_point {
        if let Some(value) = manifest.get("entryPoint") {
            if let Some(problem) = entry_point_value_problem(value) {
                return Err(MdzError::new(ErrorCode::EntryPointInvalid, problem));
            }
        }
    }
    Ok(DESCRIPTIVE
        .iter()
        .filter(|(name, ok)| manifest.get(*name).is_some_and(|v| !ok(v)))
        .map(|(name, _)| *name)
        .collect())
}

/// Why an `entryPoint` JSON value isn't a valid entry point path, or `None`.
pub(crate) fn entry_point_value_problem(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => entry_point_problem(s),
        _ => Some("entryPoint must be a non-empty string".into()),
    }
}

type FieldCheck = fn(&Value) -> bool;

const DESCRIPTIVE: [(&str, FieldCheck); 11] = [
    ("title", |v| v.as_str().is_some_and(|s| !s.is_empty())),
    ("language", Value::is_string),
    ("description", Value::is_string),
    ("version", Value::is_string),
    ("license", Value::is_string),
    ("keywords", |v| {
        v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
    }),
    ("author", Value::is_object),
    ("producer", Value::is_object),
    ("created", is_timestamp),
    ("modified", is_timestamp),
    ("cover", |v| v.as_str().is_some_and(|s| cover_problem(s).is_none())),
];

fn is_timestamp(value: &Value) -> bool {
    match value {
        Value::String(s) => is_date_time(s),
        Value::Object(o) => o.get("when").and_then(Value::as_str).is_some_and(is_date_time),
        _ => false,
    }
}

/// RFC 3339 `date-time`, such as `2026-03-08T14:30:00Z` (Section 6.1).
pub(crate) fn is_date_time(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    if b.len() < 20
        || !digits(0..4)
        || b[4] != b'-'
        || !digits(5..7)
        || b[7] != b'-'
        || !digits(8..10)
        || !matches!(b[10], b'T' | b't')
        || !digits(11..13)
        || b[13] != b':'
        || !digits(14..16)
        || b[16] != b':'
        || !digits(17..19)
    {
        return false;
    }
    let mut i = 19;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match &b[i..] {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => [h1, h2, m1, m2].iter().all(|d| d.is_ascii_digit()),
        _ => false,
    }
}

/// The major version of a Semantic Versioning 2.0.0 string, or `None` if it
/// isn't one.
fn semver_major(version: &str) -> Option<u64> {
    let (rest, build) = match version.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (version, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    let ident = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| numeric(p)) {
        return None;
    }
    if let Some(pre) = pre {
        let ok = pre
            .split('.')
            .all(|id| ident(id) && (!id.bytes().all(|c| c.is_ascii_digit()) || numeric(id)));
        if !ok {
            return None;
        }
    }
    if let Some(build) = build {
        if !build.split('.').all(ident) {
            return None;
        }
    }
    parts[0].parse().ok()
}

/// Set `modified` to `when`, keeping its existing form: an object with
/// `when` (the current form), or a plain datetime string (the legacy form).
pub(crate) fn set_modified(manifest: &mut Manifest, when: &str) {
    match manifest.get_mut("modified") {
        Some(Value::Object(o)) => {
            o.insert("when".into(), Value::String(when.into()));
        }
        Some(Value::String(s)) => *s = when.into(),
        _ => {
            let mut o = Map::new();
            o.insert("when".into(), Value::String(when.into()));
            manifest.insert("modified".into(), Value::Object(o));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver() {
        assert_eq!(semver_major("1.2.0"), Some(1));
        assert_eq!(semver_major("2.0.0-rc.1+build.5"), Some(2));
        assert_eq!(semver_major("01.0.0"), None);
        assert_eq!(semver_major("1.0"), None);
        assert_eq!(semver_major("1.0.0-01"), None);
        assert_eq!(semver_major("1.0.0-"), None);
    }

    #[test]
    fn date_times() {
        assert!(is_date_time("2026-03-08T14:30:00Z"));
        assert!(is_date_time("2026-03-08T09:30:00.123-05:00"));
        assert!(!is_date_time("2026-03-08"));
        assert!(!is_date_time("2026-03-08 14:30:00Z"));
        assert!(!is_date_time("2026-03-08T14:30:00"));
    }

    #[test]
    fn duplicates_are_reported_and_the_last_wins() {
        let parsed = parse_manifest(br#"{"title":"a","x":{"k":1,"k":2},"title":"b"}"#).unwrap();
        assert_eq!(parsed.manifest["title"], "b");
        assert_eq!(parsed.manifest["x"]["k"], 2);
        assert_eq!(parsed.duplicate_members, vec!["k", "title"]);
    }
}
