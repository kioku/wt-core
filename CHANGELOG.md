# Changelog

All notable changes to this project will be documented in this file.

This changelog is generated automatically by [git-cliff](https://git-cliff.org/)
during the release workflow and is append-only — existing entries are never modified.

## [0.5.0](https://github.com/kioku/wt-core/releases/tag/v0.5.0) — 2026-10-01

### Bug Fixes

- Preserve merge state and expose linked targets
- Harden cleanup lifecycle guards
- Preserve valid cleanup markers on retry
- Make worktree execution signal-safe
- Unify navigation output selection
- Harden shell navigation protocol
- Preserve shell binding failure statuses
- Unify machine-readable output selection
- Harden merge preflight and failure reporting
- Verify merge worktree identities
- Preserve stable merge worktree identities
- Harden resumable merge recovery
- Serialize merge lifecycle ownership
- Keep merge status read-only without journal
- Harden lifecycle cleanup and git context isolation
- Harden integrated lifecycle safety
- Scope worktree lifecycle safety
- Close scoped lifecycle recovery gaps
- Scope worktree lifecycle safety
- Scope lifecycle lock inheritance
- Restrict lifecycle lock inheritance on windows
- Contain windows lifecycle git atomically
- Harden Windows lifecycle launcher containment
- Fail closed Windows lifecycle cleanup
- Keep Windows lifecycle lease with surviving guardian
- Close Windows guardian handshake race
- Harden guardian handshake readiness tests
- Contain every lifecycle Git mutation
- Contain worktree metadata pruning
- Handle stale worktree navigation safely
- Shell-quote stale worktree guidance
- Harden Windows lifecycle ACL policy
- Close Windows ACL lifecycle review findings
- Make Windows restricted ACL tests account-aware
- Scope Windows ACL test imports
- Avoid Windows ACL owner reassignment
- Reject Windows ACL setup under impersonation
- Simplify Windows lifecycle handling
- Contain lifecycle Git mutations
- Reject trailing components on symlink materialize sources
- Preserve private materialize destination permissions
- Preserve empty materialize destination ownership
- Normalize local materialize publication paths
- Preserve linux materialize destination acls
- Allow materialize on filesystems without xattrs
- Remove inherited materialize destination acl grants
- Reject unsupported destination access control replacement
- Preserve existing materialize directories in place
- Inherit native controls at new materialize paths
- Guard new materialize root cleanup by identity
- Retain materialize root identity through final verification
- Normalize materialize root identity inspection
- Preserve materialize validation under git preferences
- Fail closed during materialize verification
- Enforce byte writes for forced materialize copies
- Integrate materialize validation and portable reflink requests
- Preserve complete materialize snapshots in existing roots
- Inspect branch locks through their owning handle
- Use git-compatible canonical paths on windows
- Support native windows worktree removal and path assertions
- Distinguish lifecycle ownership from read-only lock probes
- Enforce supported rust minimum and portable nushell navigation
- Report doctor failures and bound paths with json error fallback
- Align validation with supported rust minimum
- Normalize surviving navigation ancestors on windows
- Recover cache ownership safely across process lifetimes

### Documentation

- Update changelog for v0.4.0
- Clarify exec json stderr contract

### Features

- Merge into linked worktree targets
- Add linked-worktree merge targets
- Separate worktree and branch cleanup
- Separate worktree and branch cleanup
- Add worktree command execution
- Add worktree command execution
- Add merge topology preflight
- Add merge topology preflight
- Add resumable merge lifecycle
- Add resumable merge lifecycle

### Miscellaneous

- Bump libc from 0.2.182 to 0.2.189
- Integrate reviewed libc update
- Bump serde_json from 1.0.149 to 1.0.151
- Integrate reviewed serde json update
- Bump serde from 1.0.228 to 1.0.229
- Integrate reviewed serde update
- Bump clap from 4.6.1 to 4.6.6
- Integrate reviewed clap update
- Bump codecov/codecov-action from 6 to 7
- Integrate reviewed codecov action update
- Bump actions/checkout from 6 to 7
- Integrate reviewed checkout action update
- Bump actions/cache from 5 to 6
- Integrate reviewed cache action update

### Performance

- Copy local materialize objects independently
- Parallelize materialize checkout with bounded workers
- Parallelize large workspace cleanliness verification
- Automatically reflink independent materialize objects
- Collect ordered worktree stats with bounded workers

## [0.4.0](https://github.com/kioku/wt-core/releases/tag/v0.4.0) — 2026-07-04

### Bug Fixes

- Avoid pnpm node_modules symlinks
- Deduplicate parsed worktree paths
- Reject empty diff tool names
- Expose list stats in nushell binding
- Align list stats table dynamically
- Remove unrelated nushell binding changes
- Measure visible stats columns by characters
- Preserve nu list stats options
- Measure stats columns by display width
- Ignore difftool help section headings
- Enforce merge --into invariants
- Reject unsafe materialize repo slugs
- Reject malformed materialize refs

### Documentation

- Update changelog for v0.3.1
- Document binary numstat handling

### Features

- Add list stats flags
- Compute list git stats
- Colorize list stats output
- Add worktree diff command
- Add diff dirty mode flags
- Run dirty worktree diffs
- Add difftool preflight
- Support merge into target branch
- Add materialize cli surface
- Implement detached materialization

### Miscellaneous

- Bump tempfile from 3.26.0 to 3.27.0
- Bump assert_cmd from 2.1.2 to 2.2.1 (#41)
- Bump clap from 4.5.60 to 4.6.1 (#42)
- Bump codecov/codecov-action from 5 to 6
- Bump softprops/action-gh-release from 2 to 3
- Bump assert_cmd from 2.2.1 to 2.2.2 (#59)

## [0.3.1](https://github.com/kioku/wt-core/releases/tag/v0.3.1) — 2026-03-03

### Bug Fixes

- Support windows symlink creation for worktree links

### Documentation

- Update changelog for v0.3.0

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
