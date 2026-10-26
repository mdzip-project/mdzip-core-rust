//! Writing `.mdz` archives as a conforming producer (Section 14.1).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::archive::{MANIFEST_NAME, MdzArchive};
use crate::entrypoint::resolve_entry_point;
use crate::error::{ErrorCode, MdzError, Result, err};
use crate::filters::{DEFAULT_FILTERS, matches_filters};
use crate::manifest::{
    Manifest, Mode, SPEC_NAME, SPEC_VERSION, entry_point_value_problem, parse_manifest, serialize_manifest,
    set_modified,
};
use crate::paths::{find_path, is_markdown, nfc, strip_dot_slash, validate_entry_set, validate_path};
use crate::time::Utc;
use crate::zip::{WriteEntry, write_zip};

/// This library, as written in `producer.core` (Section 6.1).
pub const PRODUCER_CORE_NAME: &str = "mdzip-core-rust";
const PRODUCER_CORE_URL: &str = "https://github.com/mdzip-project/mdzip-core-rust";

fn producer_core() -> Value {
    let mut core = Map::new();
    core.insert("name".into(), PRODUCER_CORE_NAME.into());
    core.insert("version".into(), env!("CARGO_PKG_VERSION").into());
    core.insert("url".into(), PRODUCER_CORE_URL.into());
    Value::Object(core)
}

/// The application writing the archive, recorded in `producer.application`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    /// Application name, for example `"Marco"`.
    pub name: String,
    /// Application version.
    pub version: Option<String>,
    /// Application home page.
    pub url: Option<String>,
}

impl Application {
    /// An application with a name and version.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Application {
            name: name.into(),
            version: Some(version.into()),
            url: None,
        }
    }

    /// Set the home page.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("name".into(), self.name.clone().into());
        if let Some(v) = &self.version {
            o.insert("version".into(), v.clone().into());
        }
        if let Some(u) = &self.url {
            o.insert("url".into(), u.clone().into());
        }
        Value::Object(o)
    }
}

/// Options for [`create`].
#[derive(Clone, Debug, Default)]
pub struct CreateOptions {
    /// Title. Defaults to the manifest's, then the entry point's first
    /// heading, then its file name.
    pub title: Option<String>,
    /// Mode. Defaults to the manifest's, then `document`.
    pub mode: Option<Mode>,
    /// Entry point. Defaults to the manifest's, then discovery (Section 5.5).
    pub entry_point: Option<String>,
    /// Starting manifest: its fields are kept, with this library's required
    /// fields set over them.
    pub manifest: Option<Manifest>,
    /// The application writing the archive.
    pub application: Option<Application>,
    /// Timestamp for `created`, `modified` and the ZIP entries. Defaults to now.
    pub when: Option<SystemTime>,
    /// Keep CRLF in Markdown files. By default Markdown files and the
    /// manifest get LF line endings; other files are always written as given
    /// (Section 12.2).
    pub keep_line_endings: bool,
    /// Store every file without compression.
    pub store_only: bool,
}

impl CreateOptions {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }
    /// Set the title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    /// Set the mode.
    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = Some(mode);
        self
    }
    /// Set the entry point.
    pub fn entry_point(mut self, path: impl Into<String>) -> Self {
        self.entry_point = Some(path.into());
        self
    }
    /// Set the starting manifest.
    pub fn manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = Some(manifest);
        self
    }
    /// Set the application.
    pub fn application(mut self, application: Application) -> Self {
        self.application = Some(application);
        self
    }
    /// Set the timestamp.
    pub fn when(mut self, when: SystemTime) -> Self {
        self.when = Some(when);
        self
    }
    /// Keep CRLF line endings in Markdown files.
    pub fn keep_line_endings(mut self) -> Self {
        self.keep_line_endings = true;
        self
    }
    /// Store files without compression.
    pub fn store_only(mut self) -> Self {
        self.store_only = true;
        self
    }
}

/// Markdown files and the manifest get LF line endings (Section 12.2).
fn gets_lf(path: &str) -> bool {
    is_markdown(path) || path == MANIFEST_NAME
}

fn to_lf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

/// NFC paths, validated (Section 5.4); no two equal after NFC. Keeps order.
fn normalize_files<P, C>(
    files: impl IntoIterator<Item = (P, C)>,
    keep_line_endings: bool,
) -> Result<Vec<(String, Vec<u8>)>>
where
    P: AsRef<str>,
    C: AsRef<[u8]>,
{
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut seen = HashSet::new();
    for (raw, content) in files {
        let raw = raw.as_ref();
        // Archive paths, never OS paths: a backslash is invalid (Section 5.4).
        let path = nfc(raw);
        validate_path(&path)?;
        if path.ends_with('/') {
            return err(
                ErrorCode::PathInvalid,
                format!("'{raw}' ends in '/'; a file path can't name a directory"),
            );
        }
        if !seen.insert(path.clone()) {
            return err(
                ErrorCode::DuplicatePath,
                format!("'{raw}' duplicates another path after NFC normalization"),
            );
        }
        let content = content.as_ref();
        let data = if !keep_line_endings && gets_lf(&path) {
            to_lf(content)
        } else {
            content.to_vec()
        };
        out.push((path, data));
    }
    Ok(out)
}

/// The first ATX level-1 heading's text, if any.
fn first_heading(markdown: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(markdown);
    text.lines().find_map(|line| {
        let rest = line.strip_prefix('#')?;
        if !rest.starts_with([' ', '\t']) {
            return None;
        }
        let mut title = rest.trim();
        // An optional closing sequence of #s, preceded by a space.
        let without = title.trim_end_matches('#');
        if without.len() != title.len() && (without.is_empty() || without.ends_with([' ', '\t'])) {
            title = without.trim_end();
        }
        (!title.is_empty()).then(|| title.to_owned())
    })
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_owned(),
        _ => name.to_owned(),
    }
}

fn choose_entry_point(files: &[(String, Vec<u8>)], requested: Option<&Value>) -> Result<String> {
    let names = files.iter().map(|(p, _)| p.as_str());
    match requested {
        Some(value) => {
            if let Some(problem) = entry_point_value_problem(value) {
                return err(ErrorCode::EntryPointInvalid, problem);
            }
            let wanted = value.as_str().unwrap_or_default();
            find_path(names, strip_dot_slash(wanted))
                .map(str::to_owned)
                .ok_or_else(|| {
                    MdzError::new(
                        ErrorCode::EntryPointMissing,
                        format!("entry point '{wanted}' is not among the files"),
                    )
                })
        }
        None => resolve_entry_point(names, None),
    }
}

/// Build a new `.mdz` archive and return its bytes.
///
/// `files` are archive paths and their content, written in order. The
/// archive always gets a `manifest.json` (Section 14.1) with `spec`,
/// `producer`, `title`, `mode`, `entryPoint`, `created` and `modified`;
/// fields from [`CreateOptions::manifest`] are kept. A `manifest.json` among
/// `files` is used as the starting manifest when none is given.
///
/// Paths are written in NFC with the UTF-8 flag. Fails for an invalid or
/// duplicate path, a file that's also a directory, an entry point that's
/// invalid, missing or ambiguous, or an unknown mode.
///
/// ```
/// let data = mdzip_core::create(
///     [("index.md", "# Hello\n\n![logo](images/logo.png)\n".as_bytes()), ("images/logo.png", b"\x89PNG")],
///     &mdzip_core::CreateOptions::new(),
/// )?;
/// let archive = mdzip_core::MdzArchive::open(data)?;
/// assert_eq!(archive.entry_point()?, "index.md");
/// assert_eq!(archive.title(), Some("Hello"));
/// # Ok::<(), mdzip_core::MdzError>(())
/// ```
pub fn create<P, C>(files: impl IntoIterator<Item = (P, C)>, options: &CreateOptions) -> Result<Vec<u8>>
where
    P: AsRef<str>,
    C: AsRef<[u8]>,
{
    let mut contents = normalize_files(files, options.keep_line_endings)?;
    let mut base: Manifest = options.manifest.clone().unwrap_or_default();
    if let Some(i) = contents.iter().position(|(p, _)| p == MANIFEST_NAME) {
        let (_, given) = contents.remove(i);
        if options.manifest.is_none() {
            base = parse_manifest(&given)?.manifest;
        }
    }

    let mode = match options.mode {
        Some(m) => m,
        None => match base.get("mode") {
            None => Mode::Document,
            Some(v) => v
                .as_str()
                .and_then(Mode::parse)
                .ok_or_else(|| MdzError::new(ErrorCode::ModeUnsupported, "mode must be \"document\" or \"project\""))?,
        },
    };
    let requested = options.entry_point.clone().map(Value::String);
    let entry_point = choose_entry_point(&contents, requested.as_ref().or_else(|| base.get("entryPoint")))?;
    let stamp = Utc::from_system_time(options.when.unwrap_or_else(SystemTime::now));
    let iso = stamp.rfc3339();

    let mut out = Manifest::new();
    let mut spec = base.get("spec").and_then(Value::as_object).cloned().unwrap_or_default();
    spec.insert("name".into(), SPEC_NAME.into());
    spec.insert("version".into(), SPEC_VERSION.into());
    out.insert("spec".into(), Value::Object(spec));
    let mut producer = base
        .get("producer")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    producer.insert("core".into(), producer_core());
    if let Some(app) = &options.application {
        producer.insert("application".into(), app.to_value());
    }
    out.insert("producer".into(), Value::Object(producer));
    let title = options
        .title
        .clone()
        .or_else(|| {
            base.get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            let data = &contents.iter().find(|(p, _)| *p == entry_point)?.1;
            first_heading(data)
        })
        .unwrap_or_else(|| file_stem(&entry_point));
    out.insert("title".into(), title.into());
    out.insert("mode".into(), mode.as_str().into());
    out.insert("entryPoint".into(), entry_point.into());
    for (k, v) in &base {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    if !out.contains_key("created") {
        let mut created = Map::new();
        created.insert("when".into(), iso.clone().into());
        out.insert("created".into(), Value::Object(created));
    }
    set_modified(&mut out, &iso);

    let date = stamp.dos();
    let compress = !options.store_only;
    let mut entries = vec![WriteEntry::file(
        MANIFEST_NAME.into(),
        &serialize_manifest(&out),
        compress,
        date,
    )];
    for (path, data) in &contents {
        entries.push(WriteEntry::file(path.clone(), data, compress, date));
    }
    finish(&entries)
}

fn finish(entries: &[WriteEntry]) -> Result<Vec<u8>> {
    validate_entry_set(entries.iter().map(|e| e.name.as_str()))?;
    // A ZIP name length is 16 bits (Section 4).
    if let Some(e) = entries.iter().find(|e| e.name.len() > 0xFFFF) {
        return err(
            ErrorCode::PathInvalid,
            format!("a path of {} bytes is longer than ZIP allows (65,535)", e.name.len()),
        );
    }
    Ok(write_zip(entries))
}

/// Pack a folder into an `.mdz`, with paths relative to `directory`.
///
/// Only files matching `filters` are included ([`DEFAULT_FILTERS`] when
/// `None`: Markdown and common images; pass `Some(&["**"])` for everything).
/// Symbolic links are skipped. A `manifest.json` in the folder is the
/// starting manifest unless `options` has one.
pub fn create_from_directory(
    directory: impl AsRef<Path>,
    filters: Option<&[&str]>,
    options: &CreateOptions,
) -> Result<Vec<u8>> {
    let root = directory.as_ref();
    let filters = filters.unwrap_or(&DEFAULT_FILTERS);
    let mut files = Vec::new();
    let mut existing = None;
    let io = |e: std::io::Error, p: &Path| MdzError::new(ErrorCode::Io, format!("'{}': {e}", p.display()));
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut items: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| io(e, &dir))?
            .collect::<std::io::Result<_>>()
            .map_err(|e| io(e, &dir))?;
        items.sort_by_key(|e| e.file_name());
        for item in items.into_iter().rev() {
            let path = item.path();
            let kind = item.file_type().map_err(|e| io(e, &path))?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if rel == MANIFEST_NAME {
                existing = Some(parse_manifest(&std::fs::read(&path).map_err(|e| io(e, &path))?)?.manifest);
            } else if matches_filters(&rel, filters) {
                files.push((rel, std::fs::read(&path).map_err(|e| io(e, &path))?));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut options = options.clone();
    if options.manifest.is_none() {
        options.manifest = existing;
    }
    create(files, &options)
}

/// Changes for [`MdzArchive::update`]. Build one with the methods below.
///
/// ```
/// # let data = mdzip_core::create([("index.md", "# Hi\n")], &Default::default())?;
/// let archive = mdzip_core::MdzArchive::open(data)?;
/// let changes = mdzip_core::Update::new()
///     .write("images/logo.png", b"\x89PNG")
///     .set("title", "New title");
/// let updated = mdzip_core::MdzArchive::open(archive.update(&changes)?)?;
/// assert_eq!(updated.title(), Some("New title"));
/// # Ok::<(), mdzip_core::MdzError>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct Update {
    write: Vec<(String, Vec<u8>)>,
    remove: Vec<String>,
    fields: Vec<(String, Option<Value>)>,
    entry_point: Option<String>,
    when: Option<SystemTime>,
    keep_line_endings: bool,
    drop_special_files: bool,
}

impl Update {
    /// No changes: saving keeps everything and refreshes `modified`.
    pub fn new() -> Self {
        Self::default()
    }
    /// Add or replace a file.
    pub fn write(mut self, path: impl Into<String>, content: impl AsRef<[u8]>) -> Self {
        self.write.push((path.into(), content.as_ref().to_vec()));
        self
    }
    /// Remove a file, directory entry or symbolic link.
    pub fn remove(mut self, path: impl Into<String>) -> Self {
        self.remove.push(path.into());
        self
    }
    /// Set a top-level manifest field.
    pub fn set(mut self, field: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.push((field.into(), Some(value.into())));
        self
    }
    /// Delete a top-level manifest field.
    pub fn unset(mut self, field: impl Into<String>) -> Self {
        self.fields.push((field.into(), None));
        self
    }
    /// Change the entry point. Without this, an update that would change
    /// which file the entry point resolves to fails.
    pub fn entry_point(mut self, path: impl Into<String>) -> Self {
        self.entry_point = Some(path.into());
        self
    }
    /// Timestamp for `modified` and written entries. Defaults to now.
    pub fn when(mut self, when: SystemTime) -> Self {
        self.when = Some(when);
        self
    }
    /// Keep CRLF in written Markdown files.
    pub fn keep_line_endings(mut self) -> Self {
        self.keep_line_endings = true;
        self
    }
    /// Drop symbolic link entries (when the user agrees, Section 14.1).
    pub fn drop_special_files(mut self) -> Self {
        self.drop_special_files = true;
        self
    }
}

enum Kept<'a> {
    Copy(&'a crate::zip::ZipEntry),
    New(Vec<u8>),
}

impl MdzArchive {
    /// Apply `changes` and return the new archive bytes (Section 14.1). The
    /// archive itself isn't changed.
    ///
    /// - Entries not written or removed are copied as stored: same bytes,
    ///   compression, timestamp and attributes. Each is checked (CRC-32,
    ///   limits) before it's copied.
    /// - Directory entries, such as empty folders, are kept.
    /// - Symbolic links are kept unless removed by name or with
    ///   [`Update::drop_special_files`]; the archive then stays
    ///   non-conforming and the warning still applies. Writing a file at the
    ///   same path replaces the link.
    /// - The manifest keeps every field, including unknown ones; it's added
    ///   if missing, with the entry point the archive resolved to before.
    /// - The entry point must resolve to the same file afterwards unless
    ///   [`Update::entry_point`] names a new one; otherwise
    ///   [`ErrorCode::EntryPointChanged`].
    pub fn update(&self, changes: &Update) -> Result<Vec<u8>> {
        let before = self.entry_point().ok();
        let writes = normalize_files(changes.write.iter().map(|(p, c)| (p, c)), changes.keep_line_endings)?;
        if writes.iter().any(|(p, _)| p == MANIFEST_NAME) {
            return err(
                ErrorCode::ManifestInvalid,
                "change the manifest with Update::set, not by writing manifest.json",
            );
        }
        let mut removals = HashSet::new();
        for path in &changes.remove {
            let stored = self
                .find(path)
                .or_else(|| find_path(self.special_files(), path))
                .or_else(|| self.find_directory(path))
                .ok_or_else(|| MdzError::new(ErrorCode::NotFound, format!("'{path}' is not in the archive")))?;
            removals.insert(nfc(stored));
        }
        let mut write_map: HashMap<String, Vec<u8>> = HashMap::new();
        let mut write_order = Vec::new();
        for (p, data) in writes {
            write_order.push(p.clone());
            write_map.insert(p, data);
        }

        // Kept entries in archive order, replaced in place; then new files.
        let mut entries: Vec<(String, Kept)> = Vec::new();
        let mut placed = HashSet::new();
        for e in &self.files {
            let key = nfc(&e.name);
            if e.name == MANIFEST_NAME || removals.contains(&key) {
                continue;
            }
            if let Some(data) = write_map.get(&key) {
                entries.push((key.clone(), Kept::New(data.clone())));
                placed.insert(key);
            } else {
                self.read_entry(e)?; // never copy corrupt data
                entries.push((e.name.clone(), Kept::Copy(e)));
            }
        }
        for e in &self.directories {
            if !removals.contains(&nfc(&e.name)) {
                entries.push((e.name.clone(), Kept::Copy(e)));
            }
        }
        let mut kept_special = HashSet::new();
        if !changes.drop_special_files {
            for e in &self.special {
                let key = nfc(&e.name);
                if !removals.contains(&key) && !write_map.contains_key(&key) {
                    entries.push((e.name.clone(), Kept::Copy(e)));
                    kept_special.insert(e.name.clone());
                }
            }
        }
        for p in &write_order {
            if !placed.contains(p) {
                entries.push((p.clone(), Kept::New(write_map[p].clone())));
            }
        }

        let stamp = Utc::from_system_time(changes.when.unwrap_or_else(SystemTime::now));
        let iso = stamp.rfc3339();
        let mut manifest = self.manifest().cloned().unwrap_or_default();
        if self.manifest().is_none() {
            let mut spec = Map::new();
            spec.insert("name".into(), SPEC_NAME.into());
            spec.insert("version".into(), SPEC_VERSION.into());
            manifest.insert("spec".into(), Value::Object(spec));
            let mut producer = Map::new();
            producer.insert("core".into(), producer_core());
            manifest.insert("producer".into(), Value::Object(producer));
            if let Some(ep) = &before {
                manifest.insert("entryPoint".into(), ep.clone().into());
            }
        }
        for (field, value) in &changes.fields {
            match value {
                Some(v) => {
                    manifest.insert(field.clone(), v.clone());
                }
                None => {
                    manifest.shift_remove(field);
                }
            }
        }
        if let Some(ep) = &changes.entry_point {
            manifest.insert("entryPoint".into(), ep.clone().into());
        }
        set_modified(&mut manifest, &iso);

        // Links and directories are never documents.
        let paths = entries
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| !kept_special.contains(*p) && !p.ends_with('/'));
        let after = resolve_entry_point(paths, Some(&manifest)).map_err(|e| {
            MdzError::new(
                ErrorCode::EntryPointChanged,
                format!("the update would leave the entry point unresolvable ({})", e.message()),
            )
        })?;
        if changes.entry_point.is_none() {
            if let Some(before) = &before {
                if nfc(&after) != nfc(before) {
                    return err(
                        ErrorCode::EntryPointChanged,
                        format!("the update would change the entry point from '{before}' to '{after}'"),
                    );
                }
            }
        }

        let date = stamp.dos();
        let mut out = vec![WriteEntry::file(
            MANIFEST_NAME.into(),
            &serialize_manifest(&manifest),
            true,
            date,
        )];
        for (path, kept) in entries {
            out.push(match kept {
                Kept::Copy(e) => WriteEntry::copy(path, e, self.as_bytes()),
                Kept::New(data) => WriteEntry::file(path, &data, true, date),
            });
        }
        finish(&out)
    }
}

/// Open `data`, apply `changes` and return the new bytes. See
/// [`MdzArchive::update`].
pub fn update(data: impl Into<Vec<u8>>, changes: &Update) -> Result<Vec<u8>> {
    MdzArchive::open(data)?.update(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings() {
        assert_eq!(first_heading(b"intro\n# Guide ##\n").as_deref(), Some("Guide"));
        assert_eq!(first_heading(b"# C#\n").as_deref(), Some("C#"));
        assert_eq!(first_heading(b"#Tag\n## Sub\n").as_deref(), None);
    }

    #[test]
    fn crlf_becomes_lf() {
        assert_eq!(to_lf(b"a\r\nb\rc\n"), b"a\nb\rc\n");
    }
}
