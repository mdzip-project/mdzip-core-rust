//! Link resolution (Sections 9.3–9.6): links inside an archive, links that
//! leave it for files next to the `.mdz`, and links into (other) archives.
//!
//! Resolution needs a *host*: something that knows what files and archives
//! exist around the archive. [`FilesystemHost`] uses a directory on disk;
//! applications and tests can implement [`LinkHost`] themselves.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::archive::MdzArchive;
use crate::paths::find_path;

/// What exists at a host path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// An ordinary file.
    File,
    /// A directory (including one named like `Folder.mdz`).
    Directory,
    /// An MDZip archive.
    Archive,
}

/// Whether a link is followed by the user or loaded automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LinkUse {
    /// A link the user follows.
    #[default]
    Navigate,
    /// A resource the document loads automatically: an image, a stylesheet.
    Embed,
}

/// Why a link didn't resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// Nothing exists at the target.
    NotFound,
    /// The link is malformed: an absolute path, a backslash, a bad
    /// percent-escape, or `..` above an archive's root.
    Invalid,
    /// The host doesn't support this kind of link (Sections 9.4, 9.5).
    Unsupported,
    /// The archive has no location on a host, so links can't leave it.
    NoHostLocation,
    /// The link goes into an archive inside another archive.
    NestedArchive,
    /// The target is outside the trust boundary, or an embed would load a
    /// resource from outside the archive (Section 9.6).
    Denied,
}

impl UnresolvedReason {
    /// The reason as the spec's fixtures write it, for example `"not-found"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            UnresolvedReason::NotFound => "not-found",
            UnresolvedReason::Invalid => "invalid",
            UnresolvedReason::Unsupported => "unsupported",
            UnresolvedReason::NoHostLocation => "no-host-location",
            UnresolvedReason::NestedArchive => "nested-archive",
            UnresolvedReason::Denied => "denied",
        }
    }
}

impl fmt::Display for UnresolvedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a link points. Fragments (`#section`) are kept, without the `#`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// An entry in the source archive.
    Internal {
        /// Stored entry path.
        entry: String,
        /// Fragment, if any.
        fragment: Option<String>,
    },
    /// A file on the host, outside any archive.
    Host {
        /// Host path, `/`-separated.
        path: String,
        /// Fragment, if any.
        fragment: Option<String>,
    },
    /// An entry in an archive addressed through the host (possibly the
    /// source archive itself).
    Archive {
        /// Host path of the archive.
        archive: String,
        /// Stored entry path.
        entry: String,
        /// Fragment, if any.
        fragment: Option<String>,
    },
    /// An external URL such as `https:` or `mailto:`, not resolved as a path.
    Url,
    /// The link didn't resolve.
    Unresolved(UnresolvedReason),
}

impl LinkTarget {
    /// `"internal"`, `"host"`, `"archive"`, `"url"` or `"unresolved"`.
    pub fn kind(&self) -> &'static str {
        match self {
            LinkTarget::Internal { .. } => "internal",
            LinkTarget::Host { .. } => "host",
            LinkTarget::Archive { .. } => "archive",
            LinkTarget::Url => "url",
            LinkTarget::Unresolved(_) => "unresolved",
        }
    }

    fn with_fragment(mut self, frag: &Option<String>) -> Self {
        if let LinkTarget::Internal { fragment, .. }
        | LinkTarget::Host { fragment, .. }
        | LinkTarget::Archive { fragment, .. } = &mut self
        {
            fragment.clone_from(frag);
        }
        self
    }
}

/// Where a link was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSource<'a> {
    /// In document `path` of the archive at host path `archive`.
    Archive {
        /// Host path of the archive.
        archive: &'a str,
        /// Entry path of the document.
        path: &'a str,
    },
    /// In an ordinary file at host path `host`.
    Host {
        /// Host path of the file.
        host: &'a str,
    },
}

/// What link resolution needs to know about the files around an archive.
///
/// Host paths are `/`-separated and relative to the host's root. The
/// capability and trust methods have defaults matching the spec: links may
/// leave archives and go into them, the whole host is trusted, and embeds
/// never load resources from outside an archive.
pub trait LinkHost {
    /// What exists at `path`, or `None`.
    fn kind(&self, path: &str) -> Option<HostKind>;
    /// The stored entry path in `archive` matching `entry` (NFC,
    /// case-sensitive), or `None`.
    fn find_entry(&self, archive: &str, entry: &str) -> Option<String>;
    /// The entry point of `archive` (Section 5.5), or `None`.
    fn entry_point(&self, archive: &str) -> Option<String>;

    /// Whether the source archive has a location on this host (Section 9.4).
    fn host_location(&self) -> bool {
        true
    }
    /// Whether links may leave an archive (Section 9.4).
    fn outside_links(&self) -> bool {
        true
    }
    /// Whether links may point into archives (Section 9.5).
    fn links_into_archives(&self) -> bool {
        true
    }
    /// The trust boundary (Section 9.6): targets outside it are denied.
    /// `Some("")` trusts the whole host; `None` trusts no location at all.
    fn trust_root(&self) -> Option<&str> {
        Some("")
    }
    /// Host paths outside the boundary that the user explicitly allowed.
    fn is_allowed(&self, _path: &str) -> bool {
        false
    }
    /// Whether embeds may load resources from outside an archive (inside the
    /// trust boundary only).
    fn load_outside_resources(&self) -> bool {
        false
    }
}

/// True for a name with an `.mdz` extension, compared ASCII case-insensitively.
pub fn is_mdz_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".mdz")
}

fn has_scheme(link: &str) -> bool {
    let mut chars = link.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return false;
        }
    }
    false
}

fn dir_segments(path: &str) -> Vec<String> {
    let mut segs: Vec<String> = path.split('/').map(str::to_owned).collect();
    segs.pop();
    segs.retain(|s| !s.is_empty());
    segs
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Percent-decode exactly once; invalid escapes and non-UTF-8 results fail.
fn percent_decode(raw: &str) -> Option<String> {
    if !raw.contains('%') {
        return Some(raw.to_owned());
    }
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hi = b.get(i + 1).copied().and_then(hex)?;
            let lo = b.get(i + 2).copied().and_then(hex)?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Destination to decoded segments (Section 9.3), or `None` if invalid.
fn segments(dest: &str) -> Option<Vec<String>> {
    if dest.starts_with('/') || dest.contains('\\') {
        return None;
    }
    dest.split('/')
        .map(|raw| {
            let s = percent_decode(raw)?;
            let bad = s
                .chars()
                .any(|c| c == '/' || c == '\\' || (c as u32) < 0x20 || c as u32 == 0x7f);
            (!bad).then_some(s)
        })
        .collect()
}

fn permitted(host: &dyn LinkHost, path: &str) -> bool {
    if host.is_allowed(path) {
        return true;
    }
    match host.trust_root() {
        None => false,
        Some("") => true,
        Some(root) => path == root || path.starts_with(&format!("{}/", root.trim_end_matches('/'))),
    }
}

/// Resolve `link` written in `source` (Sections 9.3–9.6).
///
/// Host targets outside the host's trust boundary are
/// [`UnresolvedReason::Denied`] whether or not they exist, so a link can't
/// probe for files. Embeds that leave an archive are denied unless the host
/// enables [`LinkHost::load_outside_resources`]. Nothing is ever loaded from
/// the network: a link with a URL scheme is [`LinkTarget::Url`].
pub fn resolve_link(source: LinkSource<'_>, link: &str, host: &dyn LinkHost, use_: LinkUse) -> LinkTarget {
    if has_scheme(link) {
        return LinkTarget::Url;
    }
    // Split at the first "#" before removing the query: a "?" in the fragment stays there.
    let (dest, fragment) = match link.split_once('#') {
        Some((d, f)) => (d, (!f.is_empty()).then(|| f.to_owned())),
        None => (link, None),
    };
    let dest = dest.split_once('?').map_or(dest, |(d, _)| d);
    // A trailing "/" names a directory: never an entry or a host file (only an archive).
    let directory = dest.ends_with('/');

    if dest.is_empty() {
        return match source {
            LinkSource::Archive { path, .. } => LinkTarget::Internal {
                entry: path.to_owned(),
                fragment,
            },
            LinkSource::Host { host: path } => LinkTarget::Host {
                path: path.to_owned(),
                fragment,
            },
        };
    }
    let Some(segs) = segments(dest) else {
        return LinkTarget::Unresolved(UnresolvedReason::Invalid);
    };

    let mut i = 0;
    let mut stack = match source {
        LinkSource::Archive { archive, path } => {
            let mut stack = dir_segments(path);
            let mut left = false;
            while i < segs.len() {
                let s = &segs[i];
                i += 1;
                match s.as_str() {
                    "" | "." => {}
                    ".." => {
                        if stack.pop().is_none() {
                            left = true;
                            break;
                        }
                    }
                    _ => stack.push(s.clone()),
                }
            }
            if !left {
                if directory {
                    return LinkTarget::Unresolved(UnresolvedReason::NotFound);
                }
                return match host.find_entry(archive, &stack.join("/")) {
                    Some(entry) => LinkTarget::Internal { entry, fragment: None }.with_fragment(&fragment),
                    None => LinkTarget::Unresolved(UnresolvedReason::NotFound),
                };
            }
            if !host.outside_links() {
                return LinkTarget::Unresolved(UnresolvedReason::Unsupported);
            }
            if !host.host_location() {
                return LinkTarget::Unresolved(UnresolvedReason::NoHostLocation);
            }
            if use_ == LinkUse::Embed && !host.load_outside_resources() {
                return LinkTarget::Unresolved(UnresolvedReason::Denied);
            }
            dir_segments(archive)
        }
        LinkSource::Host { host: path } => dir_segments(path),
    };

    while i < segs.len() {
        let s = &segs[i];
        i += 1;
        match s.as_str() {
            "" | "." => continue,
            ".." => {
                if stack.pop().is_none() {
                    return LinkTarget::Unresolved(UnresolvedReason::NotFound);
                }
                continue;
            }
            _ => stack.push(s.clone()),
        }
        let host_path = stack.join("/");
        if is_mdz_name(s) && permitted(host, &host_path) && host.kind(&host_path) == Some(HostKind::Archive) {
            if !host.links_into_archives() {
                return LinkTarget::Unresolved(UnresolvedReason::Unsupported);
            }
            if let LinkSource::Archive { archive, .. } = source {
                if host_path != archive && use_ == LinkUse::Embed && !host.load_outside_resources() {
                    return LinkTarget::Unresolved(UnresolvedReason::Denied);
                }
            }
            return inside_archive(&host_path, &segs[i..], host, &fragment, directory);
        }
    }

    let host_path = stack.join("/");
    // Checked before existence, so a link can't probe for files outside the boundary.
    if !permitted(host, &host_path) {
        return LinkTarget::Unresolved(UnresolvedReason::Denied);
    }
    if directory || host.kind(&host_path) != Some(HostKind::File) {
        return LinkTarget::Unresolved(UnresolvedReason::NotFound);
    }
    LinkTarget::Host {
        path: host_path,
        fragment,
    }
}

fn inside_archive(
    archive: &str,
    segs: &[String],
    host: &dyn LinkHost,
    fragment: &Option<String>,
    directory: bool,
) -> LinkTarget {
    let mut inner: Vec<&str> = Vec::new();
    for (j, s) in segs.iter().enumerate() {
        match s.as_str() {
            "" | "." => continue,
            ".." => {
                if inner.pop().is_none() {
                    return LinkTarget::Unresolved(UnresolvedReason::Invalid);
                }
                continue;
            }
            _ => inner.push(s),
        }
        if is_mdz_name(s) && j < segs.len() - 1 {
            return LinkTarget::Unresolved(UnresolvedReason::NestedArchive);
        }
    }
    let entry = if inner.is_empty() {
        host.entry_point(archive)
    } else if directory {
        None
    } else {
        host.find_entry(archive, &inner.join("/"))
    };
    match entry {
        Some(entry) => LinkTarget::Archive {
            archive: archive.to_owned(),
            entry,
            fragment: None,
        }
        .with_fragment(fragment),
        None => LinkTarget::Unresolved(UnresolvedReason::NotFound),
    }
}

/// Options for [`FilesystemHost`].
#[derive(Clone, Debug)]
pub struct HostOptions {
    /// Whether links may leave an archive (Section 9.4). Default `true`.
    pub outside_links: bool,
    /// Whether links may point into archives (Section 9.5). Default `true`.
    pub links_into_archives: bool,
    /// Trust boundary relative to the host root (Section 9.6). `Some("")`,
    /// the default, trusts the whole root; `None` trusts nothing.
    pub trust_root: Option<String>,
    /// Host paths outside the boundary the user explicitly allowed.
    pub allowed_paths: Vec<String>,
    /// Whether embeds may load resources from outside an archive. Default `false`.
    pub load_outside_resources: bool,
}

impl Default for HostOptions {
    fn default() -> Self {
        HostOptions {
            outside_links: true,
            links_into_archives: true,
            trust_root: Some(String::new()),
            allowed_paths: Vec::new(),
            load_outside_resources: false,
        }
    }
}

type ArchiveCache = HashMap<String, (Option<SystemTime>, Option<Arc<MdzArchive>>)>;

/// A [`LinkHost`] backed by a directory on disk.
///
/// Host paths are relative to `root`; links can't climb above it. Archives
/// are opened read-only on demand, and cached by modification time, to look
/// up entries. Nothing is extracted or written (Section 9.6).
#[derive(Debug)]
pub struct FilesystemHost {
    root: PathBuf,
    options: HostOptions,
    cache: Mutex<ArchiveCache>,
}

impl FilesystemHost {
    /// A host rooted at `root`, with default options.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_options(root, HostOptions::default())
    }

    /// A host rooted at `root`.
    pub fn with_options(root: impl Into<PathBuf>, options: HostOptions) -> Self {
        FilesystemHost {
            root: root.into(),
            options,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn full(&self, path: &str) -> PathBuf {
        let mut full = self.root.clone();
        full.extend(path.split('/').filter(|s| !s.is_empty()));
        full
    }

    fn archive(&self, path: &str) -> Option<Arc<MdzArchive>> {
        let full = self.full(path);
        let modified = std::fs::metadata(&full).ok()?.modified().ok();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((when, archive)) = cache.get(path) {
            if *when == modified && modified.is_some() {
                return archive.clone();
            }
        }
        let archive = MdzArchive::open_path(&full).ok().map(Arc::new);
        cache.insert(path.to_owned(), (modified, archive.clone()));
        archive
    }
}

fn starts_like_zip(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|_| crate::zip::looks_like_zip(&head))
}

impl LinkHost for FilesystemHost {
    fn kind(&self, path: &str) -> Option<HostKind> {
        let full = self.full(path);
        let meta = std::fs::metadata(&full).ok()?;
        if meta.is_dir() {
            Some(HostKind::Directory)
        } else if meta.is_file() {
            if is_mdz_name(path) && starts_like_zip(&full) {
                Some(HostKind::Archive)
            } else {
                Some(HostKind::File)
            }
        } else {
            None
        }
    }

    fn find_entry(&self, archive: &str, entry: &str) -> Option<String> {
        let a = self.archive(archive)?;
        find_path(a.files(), entry).map(str::to_owned)
    }

    fn entry_point(&self, archive: &str) -> Option<String> {
        self.archive(archive)?.entry_point().ok()
    }

    fn outside_links(&self) -> bool {
        self.options.outside_links
    }
    fn links_into_archives(&self) -> bool {
        self.options.links_into_archives
    }
    fn trust_root(&self) -> Option<&str> {
        self.options.trust_root.as_deref()
    }
    fn is_allowed(&self, path: &str) -> bool {
        self.options.allowed_paths.iter().any(|p| p == path)
    }
    fn load_outside_resources(&self) -> bool {
        self.options.load_outside_resources
    }
}
