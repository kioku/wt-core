# AGENTS.md — wt-core

## Project

`wt-core` is a Rust CLI that manages Git worktree lifecycles. It provides `add`, `go`, `list`, `remove`, and `doctor` commands. Thin shell bindings (Nu, Bash, Zsh, Fish) wrap the binary to handle `cd` in the parent shell. All Git/worktree logic lives in Rust; wrappers stay trivial.

See `IMPLEMENTATION_PLAN.md` for the full design, domain model, CLI contract, and milestones.

## Repository layout

```
src/           Rust source (cli, domain, git, worktree, output, error)
bindings/      Shell wrappers (nu, bash, zsh, fish)
tests/         Integration tests (temp git repos)
.github/       CI workflows, hooks, CODEOWNERS
.ast-grep/     Structural lint rules (Rust)
scripts/       Dev tooling (hook installer)
```

## Development cycle

All quality gates run automatically via Git hooks before code reaches CI.

### On every commit (pre-commit hook)

1. **cargo fmt** — auto-formats staged `.rs` files and re-stages them
2. **cargo clippy** — lints with `-D warnings`; blocks commit on failure
3. **cargo test** — runs the full test suite; blocks commit on failure
4. **ast-grep** — structural lint rules (nesting depth, no-unwrap, no-nested-if, etc.); blocks commit on failure

### On every commit message (commit-msg hook)

Enforces **Conventional Commits** without scopes:

```
<type>: <lowercase description>
```

Types: `feat` `fix` `docs` `style` `refactor` `perf` `test` `build` `ci` `chore` `revert`

Examples:
- `feat: add worktree creation with collision-safe naming`
- `fix: handle missing branch in remove command`
- `test: add integration tests for go subcommand`

Merge, fixup, squash, amend, and revert commits are allowed through.

### CI (GitHub Actions)

- **check** — fmt + clippy + test on Linux and macOS
- **coverage** — generates lcov via cargo-llvm-cov
- **release** — manual dispatch; builds cross-platform binaries and creates a GitHub release

## Conventions

- Conventional commits, no scopes, lowercase descriptions
- Structured error types with stable exit codes (0–5)
- `--json` and `--print-cd-path` output modes for machine consumption
- Worktree paths: `<repo>/.worktrees/<slug>--<8hex>/`
- Main worktree is never removable
- `remove` deletes the local branch (`-d` by default, `-D` with `--force`)
