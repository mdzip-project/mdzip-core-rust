//! Errors with stable codes.
//!
//! Codes follow the MDZip specification's error categories (Section 16.5)
//! where one exists, and match the other MDZip core libraries and the spec's
//! shared fixtures. A few codes are specific to this library; they're marked
//! below.

use std::fmt;

/// A stable, machine-readable error code, such as `ERR_ENTRYPOINT_MISSING`.
///
/// Compare codes, not messages: messages can change between releases, codes
/// don't. [`ErrorCode::as_str`] gives the code as the spec writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// Not a valid ZIP archive, or an entry is corrupt or uses an unsupported
    /// compression method (Section 4).
    ZipInvalid,
    /// An entry is encrypted (Section 4).
    ZipEncrypted,
    /// An entry path breaks a rule in Section 5.4 not covered by a more
    /// specific code below (for example an empty or `.` segment).
    PathInvalid,
    /// An entry path is empty.
    PathEmpty,
    /// An entry path starts with `/`.
    PathLeadingSlash,
    /// An entry path has a `..` segment.
    PathTraversal,
    /// An entry path has a reserved character: `\ : * ? " < > |`.
    PathReservedChar,
    /// An entry path has a control character (U+0000–U+001F, U+007F).
    PathControlChar,
    /// Two entries have the same path after NFC normalization.
    DuplicatePath,
    /// A file's path is also the directory of another entry, such as `docs`
    /// and `docs/intro.md`.
    PathConflict,
    /// `manifest.json` is unreadable, or a critical field has the wrong type
    /// or value (Section 6.4).
    ManifestInvalid,
    /// The manifest `entryPoint` isn't a valid entry point path (Section 5.5).
    EntryPointInvalid,
    /// The manifest `entryPoint` is valid but matches no entry (Section 5.5).
    EntryPointMissing,
    /// No unambiguous entry point could be found (Section 5.5, rule 4).
    EntryPointUnresolved,
    /// The archive targets a newer major version of the spec (Section 11).
    VersionUnsupported,
    /// The manifest `mode` isn't `document` or `project` (Section 5.7.5).
    ModeUnsupported,
    /// A resource limit (entry count, size, compression ratio) was exceeded
    /// (Section 13.1).
    LimitExceeded,
    /// Library-specific: an update would change or break which file the entry
    /// point resolves to (Section 14.1).
    EntryPointChanged,
    /// Library-specific: a requested entry isn't in the archive.
    NotFound,
    /// Library-specific: an entry read as text isn't valid UTF-8 (Section 7.1).
    TextInvalid,
    /// Library-specific: reading or writing a file on disk failed.
    Io,
}

impl ErrorCode {
    /// The code as the spec writes it, for example `"ERR_ZIP_INVALID"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ZipInvalid => "ERR_ZIP_INVALID",
            ErrorCode::ZipEncrypted => "ERR_ZIP_ENCRYPTED",
            ErrorCode::PathInvalid => "ERR_PATH_INVALID",
            ErrorCode::PathEmpty => "ERR_PATH_EMPTY",
            ErrorCode::PathLeadingSlash => "ERR_PATH_LEADING_SLASH",
            ErrorCode::PathTraversal => "ERR_PATH_TRAVERSAL",
            ErrorCode::PathReservedChar => "ERR_PATH_RESERVED_CHAR",
            ErrorCode::PathControlChar => "ERR_PATH_CONTROL_CHAR",
            ErrorCode::DuplicatePath => "ERR_DUPLICATE_PATH",
            ErrorCode::PathConflict => "ERR_PATH_CONFLICT",
            ErrorCode::ManifestInvalid => "ERR_MANIFEST_INVALID",
            ErrorCode::EntryPointInvalid => "ERR_ENTRYPOINT_INVALID",
            ErrorCode::EntryPointMissing => "ERR_ENTRYPOINT_MISSING",
            ErrorCode::EntryPointUnresolved => "ERR_ENTRYPOINT_UNRESOLVED",
            ErrorCode::VersionUnsupported => "ERR_VERSION_UNSUPPORTED",
            ErrorCode::ModeUnsupported => "ERR_MODE_UNSUPPORTED",
            ErrorCode::LimitExceeded => "ERR_LIMIT_EXCEEDED",
            ErrorCode::EntryPointChanged => "ERR_ENTRYPOINT_CHANGED",
            ErrorCode::NotFound => "ERR_NOT_FOUND",
            ErrorCode::TextInvalid => "ERR_TEXT_INVALID",
            ErrorCode::Io => "ERR_IO",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An MDZip error: a stable [`ErrorCode`] and a human-readable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdzError {
    code: ErrorCode,
    message: String,
}

impl MdzError {
    /// A new error with `code` and `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        MdzError {
            code,
            message: message.into(),
        }
    }

    /// The stable error code.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The human-readable message, without the code.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MdzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MdzError {}

/// `Result` with [`MdzError`].
pub type Result<T, E = MdzError> = std::result::Result<T, E>;

pub(crate) fn err<T>(code: ErrorCode, message: impl Into<String>) -> Result<T> {
    Err(MdzError::new(code, message))
}
