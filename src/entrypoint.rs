//! Entry point discovery (Section 5.5).

use crate::error::{ErrorCode, MdzError, Result};
use crate::manifest::{Manifest, entry_point_value_problem};
use crate::paths::{find_path, is_markdown, is_support_file, strip_dot_slash};

/// The archive's primary Markdown file, by the ordered algorithm in Section 5.5.
///
/// 1. A manifest `entryPoint` is used if it's a valid entry point path that
///    matches a file. Otherwise this fails with
///    [`ErrorCode::EntryPointInvalid`] or [`ErrorCode::EntryPointMissing`];
///    it never falls back silently.
/// 2. `index.md` at the archive root.
/// 3. The single root-level Markdown file, setting support files aside; or,
///    if every root-level Markdown file is a support file and there's exactly
///    one, that file.
/// 4. Otherwise [`ErrorCode::EntryPointUnresolved`].
///
/// `paths` are the archive's entry names; directory entries (ending in `/`)
/// are ignored. Returns the path as stored.
pub fn resolve_entry_point<'a, I>(paths: I, manifest: Option<&Manifest>) -> Result<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let files: Vec<&str> = paths.into_iter().filter(|p| !p.ends_with('/')).collect();

    if let Some(value) = manifest.and_then(|m| m.get("entryPoint")) {
        if let Some(problem) = entry_point_value_problem(value) {
            return Err(MdzError::new(ErrorCode::EntryPointInvalid, problem));
        }
        let wanted = value.as_str().unwrap_or_default();
        return find_path(files.iter().copied(), strip_dot_slash(wanted))
            .map(str::to_owned)
            .ok_or_else(|| {
                MdzError::new(
                    ErrorCode::EntryPointMissing,
                    format!("entryPoint '{wanted}' is not in the archive"),
                )
            });
    }

    if let Some(index) = find_path(files.iter().copied(), "index.md") {
        return Ok(index.to_owned());
    }

    let root_markdown: Vec<&str> = files
        .iter()
        .copied()
        .filter(|p| !p.contains('/') && is_markdown(p))
        .collect();
    let content: Vec<&str> = root_markdown.iter().copied().filter(|p| !is_support_file(p)).collect();
    if content.len() == 1 {
        return Ok(content[0].to_owned());
    }
    if content.is_empty() && root_markdown.len() == 1 {
        return Ok(root_markdown[0].to_owned());
    }
    if root_markdown.is_empty() {
        return Err(MdzError::new(
            ErrorCode::EntryPointUnresolved,
            "no Markdown file at the archive root",
        ));
    }
    let mut names = root_markdown;
    names.sort_unstable();
    Err(MdzError::new(
        ErrorCode::EntryPointUnresolved,
        format!(
            "several root Markdown files and no index.md or manifest entryPoint: {}",
            names.join(", ")
        ),
    ))
}
