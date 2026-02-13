# AGENTS.md — wt-core

## Project

`wt-core` is a Rust CLI that manages Git worktree lifecycles. It provides `add`, `go`, `list`, `remove`, and `doctor` commands. Thin shell bindings (Nu, Bash, Zsh, Fish) wrap the binary to handle `cd` in the parent shell. All Git/worktree logic lives in Rust; wrappers stay trivial.

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

## Conventions

- Conventional commits, no scopes, lowercase descriptions
- Structured error types with stable exit codes (0–5)
- `--json` and `--print-cd-path` output modes for machine consumption
- Worktree paths: `<repo>/.worktrees/<slug>--<8hex>/`
- Main worktree is never removable
- `remove` deletes the local branch (`-d` by default, `-D` with `--force`)
