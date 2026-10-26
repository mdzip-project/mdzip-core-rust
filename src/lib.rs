//! Read and write **MDZip** (`.mdz`) archives: one or more Markdown documents
//! and their images and other assets, in a single ZIP file.
//!
//! This crate implements the [MDZip specification] 1.2.0 and is tested
//! against the specification's shared conformance fixtures, the same ones
//! the JavaScript, .NET and Python cores run, so an archive written here
//! opens the same way in every MDZip tool.
//!
//! [MDZip specification]: https://github.com/mdzip-project/mdzip-spec
//!
//! # Reading
//!
//! ```
//! # let data = mdzip_core::create([("index.md", "# My guide\n")], &Default::default())?;
//! use mdzip_core::MdzArchive;
//!
//! let archive = MdzArchive::open(data)?;          // or MdzArchive::open_path("guide.mdz")
//! let entry = archive.entry_point()?;             // "index.md" (Section 5.5)
//! let text = archive.read_text(&entry)?;
//! assert_eq!(archive.title(), Some("My guide"));
//! for finding in archive.validate() {             // conformance notes; never fails
//!     println!("{} {} {}", finding.level, finding.code, finding.message);
//! }
//! # Ok::<(), mdzip_core::MdzError>(())
//! ```
//!
//! Opening is safe for untrusted input: invalid or encrypted ZIPs, unsafe or
//! duplicate entry names, ZIP bombs and newer major spec versions are
//! rejected with a stable [`ErrorCode`], and symbolic links are never
//! followed. See [`MdzArchive::open`].
//!
//! # Writing
//!
//! ```
//! use mdzip_core::{create, Application, CreateOptions};
//!
//! let data = create(
//!     [
//!         ("index.md", "# My guide\n\n![logo](images/logo.png)\n".as_bytes()),
//!         ("images/logo.png", b"\x89PNG..."),
//!     ],
//!     &CreateOptions::new().application(Application::new("my-editor", "1.0")),
//! )?;
//! # Ok::<(), mdzip_core::MdzError>(())
//! ```
//!
//! Every archive gets the `manifest.json` the spec requires of producers.
//! To change an existing archive, use [`MdzArchive::update`]: untouched
//! entries and unknown manifest fields are kept exactly.
//!
//! # Links
//!
//! [`MdzArchive::resolve_link`] resolves a link written in a document, per
//! Sections 9.3–9.6. For links out of the archive to files beside it, or
//! into other archives, supply a [`LinkHost`] such as [`FilesystemHost`] to
//! [`resolve_link`].

#![warn(missing_docs)]

mod archive;
mod entrypoint;
mod error;
mod filters;
mod links;
mod manifest;
mod paths;
mod time;
mod writer;
mod zip;

pub use archive::{Diagnostic, EntryInfo, Level, Limits, MANIFEST_NAME, MdzArchive};
pub use entrypoint::resolve_entry_point;
pub use error::{ErrorCode, MdzError, Result};
pub use filters::{DEFAULT_FILTERS, matches_filters};
pub use links::{
    FilesystemHost, HostKind, HostOptions, LinkHost, LinkSource, LinkTarget, LinkUse, UnresolvedReason, is_mdz_name,
    resolve_link,
};
pub use manifest::{
    Manifest, Mode, ParsedManifest, SPEC_NAME, SPEC_VERSION, SUPPORTED_MAJOR, check_spec_version, manifest_mode,
    parse_manifest, serialize_manifest, spec_version, validate_manifest,
};
pub use paths::{SUPPORT_FILE_NAMES, is_markdown, is_support_file, nfc, same_path, validate_entry_set, validate_path};
pub use writer::{Application, CreateOptions, PRODUCER_CORE_NAME, Update, create, create_from_directory, update};
