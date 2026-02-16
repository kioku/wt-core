# wt-core

Portable Git worktree lifecycle manager — a Rust CLI core with thin shell bindings for **Nushell**, **Bash**, **Zsh**, and **Fish**.

## Opinions & Conventions

`wt-core` is opinionated about how worktrees should be managed. Understanding
these conventions upfront makes everything else predictable.

- **All logic lives in Rust.** Shell bindings are thin wrappers that handle
  only what a subprocess cannot: `cd` in the parent shell. No worktree or
  branch logic is duplicated across shells.

- **One worktree per branch, one branch per worktree.** Each `wt add` creates
  both a new worktree directory and a new local branch in one atomic step.
  There is no way to attach a worktree to an existing local branch.

- **Deterministic, collision-safe paths.** Worktree directories are placed
  under `<repo>/.worktrees/<slug>--<8hex>`, where the slug is derived from
  the branch name and the hash disambiguates collisions
  (e.g. `feature/a-b` vs. `feature-a/b`).

- **The main worktree is sacred.** You can never `remove`, `merge`, or `prune`
  the main worktree. It is always protected.

- **Branch cleanup is the default.** `remove` and `merge` delete the local
  branch after removing the worktree (`git branch -d` by default, `-D` with
  `--force`). This keeps the branch namespace clean.

- **Mainline is auto-detected.** Commands that need a mainline branch (`merge`,
  `prune`) resolve it automatically from `HEAD` of the default remote, so you
  don't need to hard-code `main` or `master`.

- **Dry-run first.** Destructive batch operations (`prune`) default to dry-run
  and require `--execute` to take action.

- **Three output modes everywhere.** Every command supports human-readable
  output (default), `--json` for machine consumption, and a
  `--print-cd-path` / `--print-paths` mode for shell wrappers.

- **Interactive when appropriate.** When a branch argument is omitted in a
  TTY, `go`, `remove`, and `merge` present a fuzzy picker instead of failing.
  Non-TTY and `--json` contexts fall through to cwd inference or an error,
  keeping scripts deterministic.

## Commands

```
wt add <branch> [--base <rev>]         Create a worktree and branch
wt go [<branch>] [-i]                  Switch to an existing worktree
wt list                                List all worktrees
wt remove [<branch>] [--force]         Remove a worktree and its local branch
wt merge [<branch>] [--push]           Merge a branch into mainline and clean up
wt prune [--execute] [--force]         Remove worktrees integrated into mainline
wt doctor                              Diagnose worktree/repo health
```

### `wt add`

Creates a new worktree and branch. If `--base` is omitted and the branch
exists on `origin`, the worktree is created tracking the remote branch with
the upstream set automatically — `git pull` and `git push` work immediately
without extra configuration.

```
wt add feature/auth              # new branch from HEAD
wt add feature/auth --base v1.0  # new branch from tag
wt add bugfix/login              # tracks origin/bugfix/login if it exists
```

### `wt go`

Switches to an existing worktree. When called without a branch in a TTY, a
fuzzy picker is shown. Use `-i` to force the picker even when there is
exactly one candidate.

```
wt go feature/auth     # switch directly
wt go                  # interactive picker (auto-selects if only one)
wt go -i               # force picker even with one candidate
```

### `wt list`

Lists all worktrees with branch, commit, and status information. The current
worktree (based on `cwd`) is marked with `← here`.

```
/home/user/repo                                    main                 a1b2c3d [main]
/home/user/repo/.worktrees/feature-auth--d4e5f6a7   feature/auth         b2c3d4e ← here
```

### `wt remove`

Removes a worktree and deletes its local branch. When called without a branch
argument, infers the target from `cwd` or opens a fuzzy picker in a TTY.

```
wt remove feature/auth     # explicit branch
wt remove                  # infer from cwd or pick interactively
wt remove --force          # remove even if dirty, use -D for branch
```

### `wt merge`

Merges a worktree's branch into the auto-detected mainline using
`--no-ff`, then removes the worktree and branch by default. Conflicts
cause an automatic `merge --abort` to keep the main worktree clean.

```
wt merge                    # merge current worktree's branch
wt merge feature/auth       # explicit branch
wt merge --push             # push mainline to origin after merge
wt merge --no-cleanup       # keep worktree and branch after merge
```

### `wt prune`

Scans all worktrees and identifies branches that are fully integrated into
mainline. Integration is detected via both ancestry checks (merge/fast-forward)
and patch-id comparison (rebase merges). Defaults to dry-run.

```
wt prune                               # dry-run: show what would be pruned
wt prune --execute                     # actually remove integrated worktrees
wt prune --execute --force             # also remove dirty worktrees
wt prune --mainline develop            # override mainline branch
```

### `wt doctor`

Diagnoses worktree and repository health — orphaned directories, detached
HEADs, and general consistency.

```
wt doctor
```

## Path Convention

Worktrees are placed under `<repo>/.worktrees/` with collision-safe directory names:

```
<slug>--<8hex>
```

Example: branch `feature/auth` → `.worktrees/feature-auth--a1b2c3d4/`

## Output Modes

| Flag              | Behavior                                     |
|-------------------|----------------------------------------------|
| *(default)*       | Human-readable text                          |
| `--json`          | Structured JSON envelope on stdout           |
| `--print-cd-path` | Bare absolute path on stdout (for wrappers)  |
| `--print-paths`   | Multi-line key values on stdout (for wrappers) |

JSON envelope example (`add`, `go`, `remove`):

```json
{
  "ok": true,
  "message": "created worktree for branch 'feature/auth'",
  "repo_root": "/abs/repo",
  "worktree_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4",
  "cd_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4",
  "branch": "feature/auth",
  "tracking": false
}
```

JSON envelope example (`merge`):

```json
{
  "ok": true,
  "message": "merged 'feature/auth' into main",
  "branch": "feature/auth",
  "mainline": "main",
  "repo_root": "/abs/repo",
  "cleaned_up": true,
  "removed_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4",
  "pushed": false
}
```

## Exit Codes

| Code | Meaning                                           |
|------|---------------------------------------------------|
| 0    | Success                                           |
| 1    | Usage / argument error                            |
| 2    | Git invocation error                              |
| 3    | Not a git repository / repo resolution failure    |
| 4    | Invariant violation (e.g. removing main worktree) |
| 5    | State conflict (dirty tree, branch exists, etc.)  |

## Shell Integration

Each binding wraps the binary and handles `cd` in the parent shell.

You can either source files from `bindings/` directly, or generate them with
`wt-core init <shell>`.

<details>
<summary><strong>Nushell</strong></summary>

```bash
wt-core init nu > ~/.config/nushell/wt.nu
```

```nu
# ~/.config/nushell/config.nu
source ~/.config/nushell/wt.nu
```
</details>

<details>
<summary><strong>Bash</strong></summary>

```bash
wt-core init bash > ~/.config/wt/wt.bash
echo 'source ~/.config/wt/wt.bash' >> ~/.bashrc
```
</details>

<details>
<summary><strong>Zsh</strong></summary>

```zsh
wt-core init zsh > ~/.config/wt/wt.zsh
echo 'source ~/.config/wt/wt.zsh' >> ~/.zshrc
```
</details>

<details>
<summary><strong>Fish</strong></summary>

```fish
wt-core init fish > ~/.config/fish/conf.d/wt.fish
```
</details>

## Install

```bash
cargo install --path .
```

Then source the appropriate shell binding.

The interactive fuzzy picker (`wt go`, `wt remove`, `wt merge` without a
branch argument) is enabled by default via the `interactive` feature flag.
To build without it:

```bash
cargo install --path . --no-default-features
```

## Compatibility

| Dependency | Minimum Version |
|------------|-----------------|
| Git        | 2.39            |
| Rust       | stable (MSRV pinned in `Cargo.toml`) |
| Nushell    | 0.109           |
| Bash       | 4.4             |
| Zsh        | 5.8             |
| Fish       | 3.6             |

## License

[MIT](LICENSE)
