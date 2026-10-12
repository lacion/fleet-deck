# Changelog

All notable changes to Fleet Deck are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Fleet Deck 2.0 (Rust rewrite).** One `fleetdeck` binary with a deck and a
  station drives Claude Code and Codex sessions on many machines through their
  native protocols, with a web board that also works on a phone. See
  [README.md](README.md) and [docs/v2/PLAN.md](docs/v2/PLAN.md).

### Removed

- The v1 TypeScript daemon, board, hooks and Claude Code plugin. The v1
  changelog (releases 0.1.0 to 0.23.6) is on branch `main` and at the `v0.*`
  release tags.
