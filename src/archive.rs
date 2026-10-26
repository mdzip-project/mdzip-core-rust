//! Reading `.mdz` archives safely (Sections 4, 5, 13).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::entrypoint::resolve_entry_point;
use crate::error::{ErrorCode, MdzError, Result, err};
use crate::links::{FilesystemHost, HostKind, LinkHost, LinkSource, LinkTarget, LinkUse, resolve_link};
use crate::manifest::{Manifest, Mode, manifest_mode, parse_manifest, validate_manifest};
use crate::paths::{is_markdown, nfc, strip_dot_slash, validate_entry_set, validate_path};
use crate::zip::{self, METHOD_DEFLATE, METHOD_STORED, ZipEntry};

/// The manifest's entry name (Section 6).
pub const MANIFEST_NAME: &str = "manifest.json";

/// Resource limits applied when opening and reading archives (Section 13.1).
///
/// Declared sizes in the ZIP directory are checked when the archive is
/// opened; actual decompressed bytes are checked again while reading, since
/// declared sizes can lie. The defaults match the spec's reference limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Most entries an archive may declare. Default 100,000.
    pub max_entries: u64,
    /// Most bytes all file entries may declare together. Default 4 GiB.
    pub max_total_uncompressed: u64,
    /// Most bytes one entry may hold. Default 1 GiB.
    pub max_entry_uncompressed: u64,
    /// Highest uncompressed-to-compressed ratio for one entry. Default 1000.
    pub max_compression_ratio: f64,
    /// The ratio applies to entries declaring more than this. Default 1 MiB.
    pub ratio_applies_above: u64,
}

impl Limits {
    /// The default limits.
    pub const DEFAULT: Limits = Limits {
        max_entries: 100_000,
        max_total_uncompressed: 4 * 1024 * 1024 * 1024,
        max_entry_uncompressed: 1024 * 1024 * 1024,
        max_compression_ratio: 1000.0,
        ratio_applies_above: 1024 * 1024,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

/// How serious a [`Diagnostic`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// The archive doesn't conform; for example its entry point can't be resolved.
    Error,
    /// The archive is usable, but something was ignored or is missing.
    Warning,
    /// Not a problem; for example an archive without a manifest.
    Info,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Info => "info",
        })
    }
}

/// A finding from opening or validating an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious it is.
    pub level: Level,
    /// A stable code: an `ERR_` code from [`ErrorCode`], or a `WARN_`/`INFO_`
    /// code such as `WARN_FIELD_IGNORED`.
    pub code: &'static str,
    /// A human-readable message.
    pub message: String,
}

impl Diagnostic {
    fn new(level: Level, code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic {
            level,
            code,
            message: message.into(),
        }
    }
}

/// ZIP metadata of one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryInfo {
    /// The path as stored.
    pub name: String,
    /// Compression method: 0 (stored) or 8 (DEFLATE).
    pub compression_method: u16,
    /// Declared uncompressed size in bytes.
    pub size: u64,
    /// Stored (compressed) size in bytes.
    pub compressed_size: u64,
    /// CRC-32 of the uncompressed bytes.
    pub crc32: u32,
    /// Modification time as stored: `(MS-DOS date, MS-DOS time)`.
    pub dos_date_time: (u16, u16),
    /// Unix mode bits, if the entry was written by a Unix-style tool.
    pub unix_mode: Option<u32>,
}

impl From<&ZipEntry> for EntryInfo {
    fn from(e: &ZipEntry) -> Self {
        let mode = e.external_attributes >> 16;
        EntryInfo {
            name: e.name.clone(),
            compression_method: e.method,
            size: e.size,
            compressed_size: e.compressed_size,
            crc32: e.crc32,
            dos_date_time: (e.dos_date, e.dos_time),
            unix_mode: (mode != 0).then_some(mode),
        }
    }
}

/// An opened, read-only `.mdz` archive.
///
/// The archive bytes are kept in memory; nothing is extracted to disk.
/// Opening is safe for untrusted input: see [`MdzArchive::open`].
#[derive(Clone)]
pub struct MdzArchive {
    data: Vec<u8>,
    path: Option<PathBuf>,
    limits: Limits,
    pub(crate) files: Vec<ZipEntry>,
    pub(crate) directories: Vec<ZipEntry>,
    pub(crate) special: Vec<ZipEntry>,
    file_index: HashMap<String, usize>,
    manifest: Option<Manifest>,
    mode: Mode,
    ignored_fields: Vec<&'static str>,
    warnings: Vec<Diagnostic>,
}

impl fmt::Debug for MdzArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdzArchive")
            .field("path", &self.path)
            .field("files", &self.files().collect::<Vec<_>>())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl MdzArchive {
    /// Open an archive from its bytes, with the default [`Limits`].
    ///
    /// Fails, with a stable [`ErrorCode`], for:
    /// - an invalid ZIP, an encrypted entry, or a compression method other
    ///   than stored or DEFLATE ([`ErrorCode::ZipInvalid`],
    ///   [`ErrorCode::ZipEncrypted`]);
    /// - entry names that are absolute, contain `..`, empty or `.` segments,
    ///   reserved or control characters, or that duplicate or conflict with
    ///   another entry after NFC normalization (the `ERR_PATH_*`,
    ///   [`ErrorCode::DuplicatePath`] and [`ErrorCode::PathConflict`] codes);
    /// - entries whose data overlaps another's, or that exceed the limits
    ///   ([`ErrorCode::LimitExceeded`]);
    /// - an unreadable manifest, a newer major `spec.version`, or an
    ///   unrecognized `mode`.
    ///
    /// Symbolic link entries are never followed or read: they're listed in
    /// [`special_files`](Self::special_files), with a warning.
    pub fn open(data: impl Into<Vec<u8>>) -> Result<Self> {
        Self::open_with_limits(data, Limits::DEFAULT)
    }

    /// Open an archive from its bytes with custom limits.
    pub fn open_with_limits(data: impl Into<Vec<u8>>, limits: Limits) -> Result<Self> {
        Self::from_bytes(data.into(), None, limits)
    }

    /// Read and open the archive at `path`.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_path_with_limits(path, Limits::DEFAULT)
    }

    /// Read and open the archive at `path` with custom limits.
    pub fn open_path_with_limits(path: impl AsRef<Path>, limits: Limits) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path)
            .map_err(|e| MdzError::new(ErrorCode::Io, format!("can't read '{}': {e}", path.display())))?;
        Self::from_bytes(data, Some(path.to_path_buf()), limits)
    }

    fn from_bytes(data: Vec<u8>, path: Option<PathBuf>, limits: Limits) -> Result<Self> {
        let mut entries = zip::read_directory(&data, limits.max_entries)?;
        for e in &entries {
            validate_path(&e.name)?;
        }
        // Duplicates and file/directory conflicts are rejected, never resolved by picking one (Section 13.1).
        validate_entry_set(entries.iter().map(|e| e.name.as_str()))?;

        let mut warnings = Vec::new();
        let mut total: u64 = 0;
        for e in &entries {
            if zip::is_encrypted(e.flags) {
                return err(ErrorCode::ZipEncrypted, format!("entry '{}' is encrypted", e.name));
            }
            if e.name.ends_with('/') {
                continue;
            }
            if e.method != METHOD_STORED && e.method != METHOD_DEFLATE {
                return err(
                    ErrorCode::ZipInvalid,
                    format!(
                        "entry '{}' uses compression method {}; MDZip allows 0 (stored) and 8 (DEFLATE)",
                        e.name, e.method
                    ),
                );
            }
            check_declared_size(e, &limits)?;
            if e.is_symlink() {
                warnings.push(Diagnostic::new(
                    Level::Warning,
                    ErrorCode::PathInvalid.as_str(),
                    format!(
                        "symbolic link '{}' ignored; an update keeps it unless asked to drop it (Sections 4, 13.1)",
                        e.name
                    ),
                ));
                continue;
            }
            total = total.saturating_add(e.size);
            if total > limits.max_total_uncompressed {
                return err(
                    ErrorCode::LimitExceeded,
                    format!(
                        "total uncompressed size exceeds {} bytes",
                        limits.max_total_uncompressed
                    ),
                );
            }
        }
        zip::locate_data(&data, &mut entries)?;

        let mut archive = MdzArchive {
            data,
            path,
            limits,
            files: Vec::new(),
            directories: Vec::new(),
            special: Vec::new(),
            file_index: HashMap::new(),
            manifest: None,
            mode: Mode::Document,
            ignored_fields: Vec::new(),
            warnings,
        };
        for e in entries {
            if e.name.ends_with('/') {
                archive.directories.push(e);
            } else if e.is_symlink() {
                archive.special.push(e);
            } else {
                archive.file_index.insert(nfc(&e.name), archive.files.len());
                archive.files.push(e);
            }
        }

        if archive.find(MANIFEST_NAME).is_some() {
            let parsed = parse_manifest(&archive.read_bytes(MANIFEST_NAME)?)?;
            for member in &parsed.duplicate_members {
                archive.warnings.push(Diagnostic::new(
                    Level::Warning,
                    "WARN_DUPLICATE_MEMBER",
                    format!("manifest.json repeats member '{member}'; the last one is used (Section 6.4)"),
                ));
            }
            // entryPoint is checked when the entry point is resolved, so the rest stays readable.
            archive.ignored_fields = validate_manifest(&parsed.manifest, false)?;
            for name in &archive.ignored_fields {
                archive.warnings.push(Diagnostic::new(
                    Level::Warning,
                    "WARN_FIELD_IGNORED",
                    format!(
                        "manifest field '{name}' has the wrong type or an invalid value and is ignored (Section 6.4)"
                    ),
                ));
            }
            archive.mode = manifest_mode(Some(&parsed.manifest))?;
            archive.manifest = Some(parsed.manifest);
        }
        Ok(archive)
    }

    // ---- The archive ----

    /// The archive bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The archive bytes, consuming the archive.
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// The file the archive was opened from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The limits the archive was opened with.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    // ---- Entries ----

    /// File entry paths, as stored, in archive order. Directory entries and
    /// symbolic links aren't included.
    pub fn files(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.files.iter().map(|e| e.name.as_str())
    }

    /// Directory entries the archive stores, such as an empty folder (each
    /// ends in `/`). A folder only implied by a file path like `docs/a.md`
    /// isn't listed.
    pub fn directories(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.directories.iter().map(|e| e.name.as_str())
    }

    /// Symbolic link entries, as stored. Never followed or read (Section 13.1).
    pub fn special_files(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.special.iter().map(|e| e.name.as_str())
    }

    /// Markdown file entries, sorted.
    pub fn documents(&self) -> Vec<&str> {
        let mut docs: Vec<&str> = self.files().filter(|p| is_markdown(p)).collect();
        docs.sort_unstable();
        docs
    }

    /// The stored file path matching `path` (NFC, case-sensitive; Section 5.4.1).
    pub fn find(&self, path: &str) -> Option<&str> {
        self.file_index.get(&nfc(path)).map(|&i| self.files[i].name.as_str())
    }

    /// Whether a file matching `path` exists (Section 5.4.1).
    pub fn contains(&self, path: &str) -> bool {
        self.find(path).is_some()
    }

    /// The stored directory entry matching `path`, with or without its
    /// trailing `/`.
    pub fn find_directory(&self, path: &str) -> Option<&str> {
        let wanted = nfc(&format!("{}/", path.trim_end_matches('/')));
        self.directories
            .iter()
            .find(|e| nfc(&e.name) == wanted)
            .map(|e| e.name.as_str())
    }

    pub(crate) fn entry(&self, path: &str) -> Option<&ZipEntry> {
        self.file_index.get(&nfc(path)).map(|&i| &self.files[i])
    }

    /// ZIP metadata of the file matching `path`.
    pub fn entry_info(&self, path: &str) -> Option<EntryInfo> {
        self.entry(path).map(EntryInfo::from)
    }

    /// A file's bytes. Fails with [`ErrorCode::NotFound`] if there's no such
    /// file, [`ErrorCode::ZipInvalid`] if its data is corrupt, and
    /// [`ErrorCode::LimitExceeded`] if it decompresses past its declared size
    /// or the limits.
    pub fn read_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let e = self
            .entry(path)
            .ok_or_else(|| MdzError::new(ErrorCode::NotFound, format!("'{path}' is not in the archive")))?;
        self.read_entry(e)
    }

    pub(crate) fn read_entry(&self, e: &ZipEntry) -> Result<Vec<u8>> {
        zip::read_entry(&self.data, e, self.limits.max_entry_uncompressed)
    }

    /// A file's text, decoded as UTF-8 (Section 7.1), with a leading BOM
    /// removed. Line endings are left as written. Fails with
    /// [`ErrorCode::TextInvalid`] if the file isn't UTF-8.
    pub fn read_text(&self, path: &str) -> Result<String> {
        let bytes = self.read_bytes(path)?;
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").map(<[u8]>::to_vec).unwrap_or(bytes);
        String::from_utf8(bytes)
            .map_err(|e| MdzError::new(ErrorCode::TextInvalid, format!("'{path}' is not valid UTF-8: {e}")))
    }

    // ---- Entry point and metadata ----

    /// The primary Markdown file (Section 5.5). Fails with
    /// [`ErrorCode::EntryPointInvalid`] or [`ErrorCode::EntryPointMissing`]
    /// for a bad manifest `entryPoint` (never a silent fallback), or
    /// [`ErrorCode::EntryPointUnresolved`].
    pub fn entry_point(&self) -> Result<String> {
        resolve_entry_point(self.files(), self.manifest.as_ref())
    }

    /// The parsed `manifest.json`, with every field kept, or `None`.
    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// `document` or `project` (Section 5.7).
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The manifest title, if it's a non-empty string.
    pub fn title(&self) -> Option<&str> {
        self.manifest.as_ref()?.get("title")?.as_str().filter(|s| !s.is_empty())
    }

    /// Descriptive manifest fields with a wrong type or value, which are
    /// ignored (Section 6.4).
    pub fn ignored_fields(&self) -> &[&'static str] {
        &self.ignored_fields
    }

    /// Warnings from opening the archive: symbolic links, repeated manifest
    /// members, ignored manifest fields.
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    // ---- Links ----

    /// Resolve `link` written in `document` of this archive (Sections 9.3–9.6).
    ///
    /// Opened from a file, links may reach files in the archive's own
    /// directory and below it, and into archives there. Opened from bytes,
    /// links that leave the archive are unresolved
    /// ([`UnresolvedReason::NoHostLocation`](crate::UnresolvedReason::NoHostLocation)).
    /// For other hosts, use [`resolve_link`] with a [`LinkHost`].
    pub fn resolve_link(&self, document: &str, link: &str, use_: LinkUse) -> LinkTarget {
        let host = DefaultHost::new(self);
        let archive = host.name.clone();
        resolve_link(
            LinkSource::Archive {
                archive: &archive,
                path: document,
            },
            link,
            &host,
            use_,
        )
    }

    // ---- Validation ----

    /// Conformance findings for this archive. Never fails.
    ///
    /// Opening already rejects what makes an archive unreadable; this adds
    /// the entry point, producer-manifest and cover checks to the warnings
    /// from opening.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut found = self.warnings.clone();
        if let Err(e) = self.entry_point() {
            found.push(Diagnostic::new(Level::Error, e.code().as_str(), e.message()));
        }
        match &self.manifest {
            None => found.push(Diagnostic::new(
                Level::Info,
                "INFO_NO_MANIFEST",
                "no manifest.json; valid, but a conforming producer would write one (Section 14.1)",
            )),
            Some(m) => {
                if m.get("spec").and_then(|s| s.get("version")).is_none() {
                    found.push(Diagnostic::new(
                        Level::Warning,
                        ErrorCode::ManifestInvalid.as_str(),
                        "manifest has no spec.version (required in producer-written manifests)",
                    ));
                }
                if !m.contains_key("entryPoint") {
                    found.push(Diagnostic::new(
                        Level::Warning,
                        ErrorCode::ManifestInvalid.as_str(),
                        "manifest has no entryPoint (required in producer-written manifests)",
                    ));
                }
                if let Some(cover) = m.get("cover").and_then(|c| c.as_str()) {
                    if !self.ignored_fields.contains(&"cover") && self.find(strip_dot_slash(cover)).is_none() {
                        found.push(Diagnostic::new(
                            Level::Warning,
                            "WARN_COVER_MISSING",
                            format!("cover '{cover}' is not in the archive"),
                        ));
                    }
                }
            }
        }
        found
    }
}

fn check_declared_size(e: &ZipEntry, limits: &Limits) -> Result<()> {
    if e.size > limits.max_entry_uncompressed {
        return err(
            ErrorCode::LimitExceeded,
            format!(
                "'{}' is {} bytes (limit {})",
                e.name, e.size, limits.max_entry_uncompressed
            ),
        );
    }
    if e.size > limits.ratio_applies_above {
        if e.compressed_size == 0 {
            return err(
                ErrorCode::LimitExceeded,
                format!("'{}' declares data but stores none", e.name),
            );
        }
        let ratio = e.size as f64 / e.compressed_size as f64;
        if ratio > limits.max_compression_ratio {
            return err(
                ErrorCode::LimitExceeded,
                format!(
                    "'{}' has compression ratio {ratio:.0}:1 (limit {:.0}:1)",
                    e.name, limits.max_compression_ratio
                ),
            );
        }
    }
    Ok(())
}

/// The host [`MdzArchive::resolve_link`] uses: the archive itself, plus the
/// directory it was opened from, if any.
struct DefaultHost<'a> {
    archive: &'a MdzArchive,
    name: String,
    fs: Option<FilesystemHost>,
}

impl<'a> DefaultHost<'a> {
    fn new(archive: &'a MdzArchive) -> Self {
        let name = archive
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive.mdz".into());
        let fs = archive.path.as_deref().map(|p| {
            let dir = std::path::absolute(p)
                .ok()
                .and_then(|a| a.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            FilesystemHost::new(dir)
        });
        DefaultHost { archive, name, fs }
    }
}

impl LinkHost for DefaultHost<'_> {
    fn kind(&self, path: &str) -> Option<HostKind> {
        self.fs.as_ref()?.kind(path)
    }
    fn find_entry(&self, archive: &str, entry: &str) -> Option<String> {
        if archive == self.name {
            return self.archive.find(entry).map(str::to_owned);
        }
        self.fs.as_ref()?.find_entry(archive, entry)
    }
    fn entry_point(&self, archive: &str) -> Option<String> {
        if archive == self.name {
            return self.archive.entry_point().ok();
        }
        self.fs.as_ref()?.entry_point(archive)
    }
    fn host_location(&self) -> bool {
        self.fs.is_some()
    }
}
