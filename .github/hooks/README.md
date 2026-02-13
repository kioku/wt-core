# Git Hooks

Shared git hooks for wt-core that provide automated code quality checks.

## Installation

```bash
bash scripts/setup-hooks.sh
```

This copies hooks from `.github/hooks/` to your local `.git/hooks/` and makes them executable.

## Hooks

### pre-commit

Runs automatically on `git commit`:

- **cargo fmt** — auto-formats and re-stages changed `.rs` files
- **cargo clippy** — lints with `-D warnings`
- **cargo test** — runs the full test suite
- **ast-grep** — structural lint (if `ast-grep` is installed)

### commit-msg

Validates commit messages against [Conventional Commits](https://www.conventionalcommits.org/) (no scopes):

**Format:** `<type>: <lowercase description>`

- **Types:** `feat` `fix` `docs` `style` `refactor` `perf` `test` `build` `ci` `chore` `revert`
- Description must start with a lowercase letter
- Subject line >72 characters triggers a warning

**Auto-allowed formats:**
- Merge commits (`Merge pull request ...`)
- Rebase commits (`fixup!` / `squash!` / `amend!`)
- Revert commits (`Revert "..."`)

## Usage

```bash
# hooks run automatically
git commit -m "feat: add worktree creation"

# bypass if needed (not recommended)
git commit --no-verify -m "wip"
```

## Updating

Pull latest and re-run: `bash scripts/setup-hooks.sh`
