# Changelog

## [0.1.0] - Unreleased

First release. Implements MDZip spec 1.2.0 and passes the spec's shared
fixtures from [v1.2.1](https://github.com/mdzip-project/mdzip-spec/releases/tag/v1.2.1),
including the T-018 ZIP-bomb tests. Rust 1.85+.

- Reading: `MdzArchive::open` (bytes) and `open_path`, entry point discovery,
  case-sensitive NFC lookups, `files`, `directories`, `special_files`,
  `documents`, `find`, `find_directory`, `read_bytes`, `read_text`,
  `manifest`, `mode`, `validate()`.
- Writing: `create`, `create_from_directory`, and `MdzArchive::update`, which
  copies untouched entries as stored and keeps unknown manifest fields (in
  order), directory entries and symbolic links (unless asked to drop them).
- Links: `resolve_link` for links inside, out of and into archives, with a
  trust boundary (spec §9.3–9.6); `FilesystemHost` for a folder on disk.
- Safety: rejects unsafe or duplicate paths, encrypted entries, unsupported
  compression and overlapping entries; never follows symbolic links; `Limits`
  caps entry count, sizes and compression ratio, checked on open and again
  while reading. Corrupt entry data fails with `ERR_ZIP_INVALID`.
- Errors: `MdzError` with a stable `ErrorCode`.
