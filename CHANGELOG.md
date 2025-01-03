# Changelog
All notable changes will be documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

## [0.1.2] - 2025-01-03
### Fixed
- Fixed a compilation bug on Windows.

## [0.1.1] - 2025-01-01
### Fixed
- Fixed panic when attempting to read large files.

### Changed
- Improved performance of `tev backup verify`.
- Moved `tev backup mount` behind a default-enabled `mount` feature flag, to
  enable using the rest of `tev` on Windows.

## [0.1.0] - 2024-12-31
Initial release!
