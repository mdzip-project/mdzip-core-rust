# Upstream requests

## Baseline fixtures and conditional extension profiles

Owner: `mdzip-spec`. Request: https://github.com/mdzip-project/mdzip-spec/issues/9

- Dependency: a versioned baseline R1-R7 contract/fixture schema and sync path is
  required for shared baseline acceptance and CI adapters. Until it lands, this
  crate runs the released spec fixtures (v1.2.1) plus its own regression tests.
- Advertised optional capabilities: configurable resource limits, host link
  resolution (`FilesystemHost`) and packaging filters. Image analysis and a
  workspace model aren't implemented; their suites are not applicable.
- Reference: hub `.github/docs/core-contract.md` (draft 0.1) and
  `.github/docs/core-parity.md`.

Close the baseline request when the shared baseline adapter is running in CI.
