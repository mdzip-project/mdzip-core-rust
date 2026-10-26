//! Archive path rules: validation (Section 5.4), matching (Section 5.4.1),
//! Markdown extensions (Section 7.3) and support files (Section 5.5).

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

use crate::error::{ErrorCode, MdzError};

/// Support file names (Section 5.5), compared ASCII case-insensitively, with
/// a `.md` or `.markdown` extension.
pub const SUPPORT_FILE_NAMES: [&str; 5] = ["readme", "agents", "license", "changelog", "contributing"];

/// The path in Unicode Normalization Form C.
pub fn nfc(path: &str) -> String {
    path.nfc().collect()
}

/// Lowercase `A`–`Z` only, never locale-aware (Section 5.4.1).
pub(crate) fn ascii_lower(text: &str) -> String {
    text.to_ascii_lowercase()
}

/// Path equality per Section 5.4.1: case-sensitive, after NFC normalization.
pub fn same_path(a: &str, b: &str) -> bool {
    a == b || nfc(a) == nfc(b)
}

/// The stored path in `paths` that matches `wanted` (Section 5.4.1).
pub(crate) fn find_path<'a, I>(paths: I, wanted: &str) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let target = nfc(wanted);
    paths.into_iter().find(|p| nfc(p) == target)
}

/// True for `.md` and `.markdown` files, extension compared ASCII
/// case-insensitively (Section 7.3).
pub fn is_markdown(path: &str) -> bool {
    let lower = ascii_lower(path);
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn markdown_stem(lower: &str) -> &str {
    lower
        .strip_suffix(".markdown")
        .or_else(|| lower.strip_suffix(".md"))
        .unwrap_or(lower)
}

/// True for a root-level support file (Section 5.5): `README`, `AGENTS`,
/// `LICENSE`, `CHANGELOG` or `CONTRIBUTING`, with `.md` or `.markdown`.
pub fn is_support_file(path: &str) -> bool {
    if path.contains('/') || !is_markdown(path) {
        return false;
    }
    let lower = ascii_lower(path);
    SUPPORT_FILE_NAMES.contains(&markdown_stem(&lower))
}

fn is_control(c: char) -> bool {
    (c as u32) < 0x20 || c as u32 == 0x7f
}

fn is_reserved(c: char) -> bool {
    matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// Check that `path` is a valid archive entry name (Section 5.4).
///
/// A trailing `/` marks a directory entry; every segment before it must be a
/// real name. Returns the specific error code the spec's fixtures use.
pub fn validate_path(path: &str) -> Result<(), MdzError> {
    if path.is_empty() {
        return Err(MdzError::new(ErrorCode::PathEmpty, "path is empty"));
    }
    if path.starts_with('/') {
        return Err(MdzError::new(
            ErrorCode::PathLeadingSlash,
            format!("'{path}' starts with '/'"),
        ));
    }
    if path.chars().any(is_control) {
        return Err(MdzError::new(
            ErrorCode::PathControlChar,
            format!("{path:?} contains a control character"),
        ));
    }
    if path.chars().any(is_reserved) {
        return Err(MdzError::new(
            ErrorCode::PathReservedChar,
            format!("'{path}' contains a reserved character (\\ : * ? \" < > |)"),
        ));
    }
    let body = path.strip_suffix('/').unwrap_or(path);
    if body.split('/').any(|s| s == "..") {
        return Err(MdzError::new(
            ErrorCode::PathTraversal,
            format!("'{path}' contains a '..' segment"),
        ));
    }
    if body.split('/').any(|s| s.is_empty() || s == ".") {
        return Err(MdzError::new(
            ErrorCode::PathInvalid,
            format!("'{path}' contains an empty or '.' segment"),
        ));
    }
    Ok(())
}

/// Check that a set of entry names can form one archive (Section 5.4).
///
/// Names ending in `/` are directory entries. Two names equal after NFC are
/// duplicates ([`ErrorCode::DuplicatePath`]); a file whose path is also a
/// directory of another entry is a conflict ([`ErrorCode::PathConflict`]).
pub fn validate_entry_set<'a, I>(names: I) -> Result<(), MdzError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut files: Vec<(String, &str)> = Vec::new();
    let mut file_index: HashMap<String, &str> = HashMap::new();
    let mut directories: HashMap<String, &str> = HashMap::new();
    for name in names {
        let is_dir = name.ends_with('/');
        let key = nfc(if is_dir { &name[..name.len() - 1] } else { name });
        if !is_dir {
            if let Some(other) = file_index.get(&key) {
                return Err(MdzError::new(
                    ErrorCode::DuplicatePath,
                    format!("'{name}' and '{other}' are the same path (Section 5.4)"),
                ));
            }
            file_index.insert(key.clone(), name);
            files.push((key.clone(), name));
        } else {
            directories.entry(key.clone()).or_insert(name);
        }
        let mut end = 0;
        while let Some(i) = key[end..].find('/') {
            end += i;
            directories.entry(key[..end].to_string()).or_insert(name);
            end += 1;
        }
    }
    for (key, name) in &files {
        if let Some(dir) = directories.get(key) {
            return Err(MdzError::new(
                ErrorCode::PathConflict,
                format!("'{name}' is a file and also a directory of '{dir}' (Section 5.4)"),
            ));
        }
    }
    Ok(())
}

/// Strip a single leading `./` from an entry point or cover value.
pub(crate) fn strip_dot_slash(value: &str) -> &str {
    value.strip_prefix("./").unwrap_or(value)
}

/// Why a manifest `entryPoint` string isn't a valid entry point path
/// (Section 5.5), or `None` if it is.
pub(crate) fn entry_point_problem(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some("entryPoint must be a non-empty string".into());
    }
    let path = strip_dot_slash(value);
    if path.ends_with('/') {
        return Some(format!("entryPoint '{value}' names a directory"));
    }
    if let Err(e) = validate_path(path) {
        return Some(format!(
            "entryPoint '{value}' is not a valid archive path ({})",
            e.message()
        ));
    }
    if !is_markdown(path) {
        return Some(format!("entryPoint '{value}' is not a .md or .markdown file"));
    }
    None
}

/// Why a manifest `cover` string isn't a valid path (Section 6.2), or `None`.
pub(crate) fn cover_problem(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some("cover must be a non-empty string".into());
    }
    let path = strip_dot_slash(value);
    if path.ends_with('/') {
        return Some(format!("cover '{value}' names a directory"));
    }
    validate_path(path)
        .err()
        .map(|e| format!("cover '{value}' is not a valid archive path ({})", e.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_files_are_ascii_case_insensitive() {
        assert!(is_support_file("README.md"));
        assert!(is_support_file("readme.MARKDOWN"));
        assert!(is_support_file("AGENTS.MD"));
        assert!(!is_support_file("docs/README.md"));
        assert!(!is_support_file("README-draft.md"));
        // Turkish dotted/dotless i must not match (Section 5.4.1, T-010).
        assert!(!is_support_file("L\u{130}CENSE.md"));
    }

    #[test]
    fn nfc_and_nfd_match_but_case_does_not() {
        assert!(same_path("caf\u{e9}.md", "cafe\u{301}.md"));
        assert!(!same_path("Index.md", "index.md"));
    }
}
