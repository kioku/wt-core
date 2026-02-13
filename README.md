# wt-core

Portable Git worktree lifecycle manager — a Rust CLI core with thin shell bindings for **Nushell**, **Bash**, **Zsh**, and **Fish**.

## Why

Git worktrees are powerful but clunky. `wt-core` gives you a fast, consistent interface for creating, navigating, listing, and removing worktrees across every shell you use — without duplicating logic per shell.

All worktree/branch operations live in a single Rust binary. Shell bindings handle only what a subprocess can't: `cd` in the parent shell.

## Commands

```
wt add <branch> [--base <rev>]   Create a worktree and branch
wt go <branch>                   Switch to an existing worktree
wt list                          List all worktrees
wt remove [<branch>] [--force]   Remove a worktree and its local branch
wt doctor                        Diagnose worktree/repo health
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

JSON envelope:

```json
{
  "ok": true,
  "message": "created worktree",
  "repo_root": "/abs/repo",
  "worktree_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4",
  "cd_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4"
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

<details>
<summary><strong>Nushell</strong></summary>

```nu
# ~/.config/nushell/config.nu
source path/to/bindings/nu/wt.nu
```
</details>

<details>
<summary><strong>Bash</strong></summary>

```bash
# ~/.bashrc
source path/to/bindings/bash/wt.bash
```
</details>

<details>
<summary><strong>Zsh</strong></summary>

```zsh
# ~/.zshrc
source path/to/bindings/zsh/wt.zsh
```
</details>

<details>
<summary><strong>Fish</strong></summary>

```fish
# ~/.config/fish/conf.d/wt.fish
source path/to/bindings/fish/wt.fish
```
</details>

## Install

```bash
cargo install --path .
```

Then source the appropriate shell binding.

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
