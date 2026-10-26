# mdzip-core (Rust)

Read and write **MDZip** (`.mdz`) archives in Rust: one or more Markdown documents and their images and other assets, packaged in a single ZIP file.

This crate implements the [MDZip specification](https://github.com/mdzip-project/mdzip-spec) **1.2.0**. It's a standalone implementation (not a binding to the JavaScript, .NET or Python cores) and it passes the specification's shared conformance fixtures, the same ones every MDZip core runs. An archive written with this crate opens the same way in the other MDZip tools, and theirs open here.

- Rust 1.85+
- Small, pure-Rust dependencies: `miniz_oxide` (DEFLATE), `crc32fast`, `unicode-normalization`, `serde_json`. No C code, no async runtime.
- Install with `cargo add mdzip-core`, import as `mdzip_core`.

## Reading

```rust
use mdzip_core::MdzArchive;

let archive = MdzArchive::open_path("guide.mdz")?;   // or MdzArchive::open(bytes)
println!("{:?} {}", archive.title(), archive.mode()); // Some("My Guide") document
let entry = archive.entry_point()?;                  // e.g. "index.md" (Section 5.5)
let text = archive.read_text(&entry)?;
let logo = archive.read_bytes("images/logo.png")?;
for doc in archive.documents() { /* every Markdown file */ }
for finding in archive.validate() {                  // conformance notes; never fails
    println!("{} {} {}", finding.level, finding.code, finding.message);
}
```

The archive is held in memory; nothing is extracted to disk.

Opening is safe for untrusted input:

- It rejects invalid or encrypted ZIPs, entry names that are absolute, contain `..`, empty or `.` segments, or reserved or control characters, and entries that duplicate or conflict with each other after Unicode NFC normalization.
- It rejects a `spec.version` from a newer major version, and unrecognized `mode` values.
- `Limits` caps entry count, total and per-entry size, and compression ratio. Sizes are checked against the ZIP directory when opening, then again while reading, since declared sizes can lie. Entries whose data overlaps another's are rejected.
- Symbolic links are never followed or read. They're listed in `special_files()` (not in `files()`), with a warning.
- Corrupt entry data (a bad CRC-32, broken compression, or data longer than declared) fails the read with `ERR_ZIP_INVALID` or `ERR_LIMIT_EXCEEDED`.

`files()` lists file entries. Directory entries the archive stores, such as an empty folder, are in `directories()` (each ends in `/`); `find_directory("docs")` finds one with or without the slash.

Path lookups follow the spec's matching rule: case-sensitive, after NFC normalization. So `café.md` is found however it's encoded, and `Index.md` doesn't match `index.md`.

The manifest is a `serde_json::Map` in its original field order (`archive.manifest()`), so fields this crate doesn't know are kept.

## Writing

```rust
use mdzip_core::{create, Application, CreateOptions};

let data = create(
    [
        ("index.md", "# My Guide\n\n![logo](images/logo.png)\n".as_bytes()),
        ("images/logo.png", &png_bytes),
    ],
    &CreateOptions::new()
        .title("My Guide")                          // defaults to the entry point's first heading
        .application(Application::new("my-editor", "1.0")),
)?;
std::fs::write("guide.mdz", data)?;

let data = mdzip_core::create_from_directory("my-guide/", None, &CreateOptions::new())?; // Markdown + images
```

Every archive gets a `manifest.json` with `spec.version` (`1.2.0`), `entryPoint`, `title` and `mode`, as the spec requires of producers. The entry point is the one you name, or else found by the spec's rules: `index.md`, or the only Markdown file at the root (support files such as `README.md` are set aside). Paths are written in NFC with UTF-8 names. Files are DEFLATE-compressed when that makes them smaller. Markdown files get LF line endings; other files are written exactly as given, since some formats (Windows batch files, RFC 4180 CSV) depend on CRLF. Use `.keep_line_endings()` to keep CRLF in Markdown too.

### Updating

```rust
use mdzip_core::{MdzArchive, Update};

let archive = MdzArchive::open_path("guide.mdz")?;
let data = archive.update(
    &Update::new()
        .write("images/new.png", &png_bytes)
        .write("index.md", new_text)
        .remove("old.md")
        .set("title", "Second edition")             // .unset("field") deletes one
)?;
```

`update` follows the spec's rules for modifying archives:

- Entries you don't touch are copied as stored: same bytes, compression, timestamp and attributes. Each is checked before it's copied, so corrupt data is never passed on.
- Unknown manifest fields are kept, at every level, in their original order.
- Directory entries are kept, including empty folders. `.remove("empty")` drops one.
- Symbolic link entries are kept unchanged too, so the archive stays non-conforming and the warning from opening it still applies. Use `.drop_special_files()` or `.remove(name)` once the user agrees to remove them.
- A manifest is added if the archive has none.
- The entry point must resolve to the same file afterwards. Use `.entry_point(path)` to change it on purpose; otherwise the update fails with `ERR_ENTRYPOINT_CHANGED`.

## Links

```rust
use mdzip_core::{LinkTarget, LinkUse};

match archive.resolve_link("chapters/intro.md", "../images/a.png", LinkUse::Embed) {
    LinkTarget::Internal { entry, fragment } => { /* an entry in this archive */ }
    LinkTarget::Host { path, .. } => { /* a file beside the .mdz */ }
    LinkTarget::Archive { archive, entry, .. } => { /* an entry in another archive */ }
    LinkTarget::Url => { /* https:, mailto:, ... never loaded by this crate */ }
    LinkTarget::Unresolved(reason) => { /* not-found, invalid, denied, ... */ }
}
```

Links resolve per spec Sections 9.3–9.6: inside the archive, out of it to files next to the `.mdz`, and into archives like `Project.mdz/docs/design.md`. They're never clamped at the archive root. Opened from a file, links can reach the archive's own folder; opened from bytes, links that leave it are unresolved. Use `resolve_link` with a `FilesystemHost` to resolve against a larger folder, or implement `LinkHost` for anything else.

The host's folder is the trust boundary (Section 9.6): targets outside it are `denied`, whether or not they exist. Narrow it with `HostOptions::trust_root`, or allow specific targets with `allowed_paths`. Resolve automatically loaded resources, such as images, with `LinkUse::Embed`: embeds that leave the archive are `denied` unless the host sets `load_outside_resources`, and even then only inside the boundary.

## Errors

Failures return `MdzError`, whose `code()` is a stable `ErrorCode`; `code().as_str()` gives it as the spec writes it. Codes follow the spec's suggested categories (Section 16.5), for example `ERR_ENTRYPOINT_INVALID`, `ERR_ENTRYPOINT_MISSING`, `ERR_DUPLICATE_PATH`, `ERR_ZIP_ENCRYPTED` and `ERR_LIMIT_EXCEEDED`, plus a few specific to this crate (`ERR_ENTRYPOINT_CHANGED`, `ERR_NOT_FOUND`, `ERR_TEXT_INVALID`, `ERR_IO`). Compare codes, not messages.

## Core contract

How this crate covers the required operations in the MDZip [core library contract](https://github.com/mdzip-project/.github/blob/main/docs/core-contract.md):

| Contract | Rust |
|---|---|
| R1 open/read | `MdzArchive::open` (bytes), `open_path`, `open_with_limits` |
| R2 entries and metadata | `files`, `directories`, `special_files`, `documents`, `find`, `find_directory`, `contains`, `entry_info`, `read_bytes`, `read_text`, `manifest`, `entry_point`, `mode` |
| R3 links | `MdzArchive::resolve_link`, `resolve_link` with a `LinkHost` |
| R4 validation and errors | `validate()` diagnostics; `MdzError::code`; open-time rejections |
| R5 creation | `create`, `create_from_directory` |
| R6 modification | `MdzArchive::update` with `Update` (`write`, `remove`, `set`, `unset`, `entry_point`) |
| R7 safety | Path, duplicate, overlap, symlink and link rules on open; `Limits` (built-in defaults, configurable) |

Optional extensions: configurable resource limits, host link resolution (`FilesystemHost`) and packaging filters (`create_from_directory`, `matches_filters`). Image analysis and a workspace/editor model aren't included.

Every consumer test (T-001 to T-018) and producer check (P-001 to P-008) in the spec's fixtures runs in `cargo test`, except T-016 (hostile content), which applies only to apps that render Markdown. An app that renders documents from this crate should run T-016 itself.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`tests/fixtures/` is a copy of the spec's shared fixtures, made with the spec's sync tool and checked by `SPEC-FIXTURES.json` (the `fixtures_match_marker` test). Don't edit it by hand. To refresh it, with `mdzip-spec` checked out alongside:

```sh
node ../mdzip-spec/tools/sync-fixtures.mjs tests/fixtures --ref v1.2.1
```

## License

Apache-2.0
