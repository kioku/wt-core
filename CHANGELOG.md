# Changelog

All notable changes to this project will be documented in this file.

This changelog is generated automatically by [git-cliff](https://git-cliff.org/)
during the release workflow and is append-only — existing entries are never modified.

## [0.3.0](https://github.com/kioku/wt-core/releases/tag/v0.3.0) — 2026-03-03

### Bug Fixes

- Restore init doc comment displaced by setup variant insertion
- Populate python entries for setup.py and setup.cfg markers
- Reject glob false positives when prefix+suffix exceeds name length
- Remove unnecessary clone of symlinked vec in cmd_add

### Documentation

- Update readme for v0.2.0 features and add conventions section
- Clarify json output is line-oriented

### Features

- Add git-cliff changelog generation to release workflow
- Symlink gitignored resources into new worktrees
- Add --json flag to setup command
- Emit event field in JSON output from mutating commands
- Emit single-line json output for machine parsing

### Miscellaneous

- Bump clap from 4.5.58 to 4.5.60
- Bump tempfile from 3.25.0 to 3.26.0
- Bump dialoguer from 0.11.0 to 0.12.0
- Bump actions/upload-artifact from 6 to 7
- Bump actions/download-artifact from 7 to 8

## [0.2.0](https://github.com/kioku/wt-core/releases/tag/v0.2.0) — 2026-02-15

### Features

- Add prune command with integration detection and dry-run/execute modes
- Add interactive picker to remove command
- Add merge command to complete worktree lifecycle
- Add shell bindings for merge command
- Support tracking remote branches in add
- Add is_current marker to list output

### Bug Fixes

- Auto-escalate branch deletion to -D for rebase-integrated prune
- Harden prune mainline resolution and input validation
- Prefer most-specific worktree match in cwd inference
- Allow interactive picker through shell bindings for remove
- Address review findings for interactive remove picker
- Handle wt-core failure in nu binding print-paths path
- Separate external command from pipeline in nu remove binding
- Add mainline pre-flight check and reduce merge/remove duplication
- Address review findings for merge command
- Canonicalize worktree paths in current-worktree detection

## [0.1.0](https://github.com/kioku/wt-core/releases/tag/v0.1.0) — 2026-02-14

### Features

- Add domain model and structured error types
- Add cli argument parsing with clap
- Add git process interface
- Add output formatting with json envelope
- Implement worktree operations and wire commands
- Add shell bindings for nu, bash, zsh, and fish
- Add Init variant to Command enum
- Add cmd_init with embedded bindings and wire into dispatcher
- Add flake.nix with build derivation and dev shell
- Add dialoguer dependency behind interactive feature flag
- Add interactive worktree picker to wt go

### Bug Fixes

- Resolve review issues — repo root, is_main, env isolation, and error classification
- Improve git error classification for stable exit codes
- Replace fragile json parsing in shell remove wrappers
- Emit branch name in --print-paths and fix shell wrapper quoting
- Correct PrintPaths doc comment and reclassify json serialization error
- Restore cd-out-of-removed-worktree logic for --json mode
- Resolve ast-grep no-println warnings
- Canonicalize temp dir path to resolve macOS symlink mismatch
- Cd to safe directory before cleanup in nu binding test
- Improve flake with dynamic version, makeWrapper, and source filtering
- Resolve review issues in interactive picker
- Improve interactive picker error messages and -i flag semantics
- Allow interactive picker through shell bindings
- Add nushell root wt command and harden shell tests
- Make shell binding help passthrough consistent

### Documentation

- Add AGENTS.md with project overview and dev cycle

### Refactor

- Flatten nesting to satisfy ast-grep structural lint
- Extract command layer from main.rs
- Improve RepoRoot and BranchName type ergonomics
- Separate output format types for navigation vs status commands
- Use clap ValueEnum for init shell argument

### Miscellaneous

- Add gitignore for rust
- Add crates.io package metadata and exclude dev-only files
- Ignore nix build result symlink
