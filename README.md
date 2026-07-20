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
  `--force`). Use `remove --keep-branch` when staged integration still needs
  the local branch, without keeping its worktree alive.

- **Mainline is auto-detected.** Commands that need a mainline branch (`merge`,
  `prune`) resolve it automatically from `HEAD` of the default remote, so you
  don't need to hard-code `main` or `master`.

- **Dry-run first.** Destructive batch operations (`prune`) default to dry-run
  and require `--execute` to take action.

- **One output contract for navigation.** `add`, `go`, `remove`, and `merge`
  use human-readable output by default and `--json` as their canonical machine
  format. The legacy `--print-cd-path` / `--print-paths` flags remain
  available for existing shell scripts; if a legacy path flag is combined
  with `--json`, JSON takes precedence. `exec --json` is the exception: it
  writes resolution metadata to stderr so the child owns stdout unchanged;
  child diagnostics may follow on that stderr stream.

- **Interactive when appropriate.** When a branch argument is omitted in a
  TTY, `go`, `remove`, and `merge` present a fuzzy picker instead of failing.
  Non-TTY and `--json` contexts fall through to cwd inference or an error,
  keeping scripts deterministic.

## Commands

```
wt add <branch> [--base <rev>]         Create a worktree and branch
wt go [<branch>] [-i]                  Switch to an existing worktree
wt exec <branch> -- <command...>       Run a command in a worktree
wt list [--stats]                      List all worktrees
wt remove [<branch>] [--force]         Remove a worktree and its local branch
                                      [--keep-branch] preserves the branch
wt merge [<branch>] [--into <branch>]  Merge a branch and clean up
wt merge --status                     Show a paused managed merge
wt merge --continue                   Finish a resolved managed merge
wt merge --abort                      Abort a managed merge safely
wt merge [<branch>] --inspect         Inspect merge topology without mutation
wt diff [<branch>] [--dry-run]         Open difftool for branch or dirty changes
wt materialize --sha <sha> ...         Create an explicit detached checkout
wt prune [--execute] [--force]         Remove integrated worktrees and branches
                                      [--integrated-into <rev>] sets the target
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

### `wt exec`

Resolves the branch using the same worktree lookup as `wt go`, then runs the
command directly in that worktree. Arguments after `--` are passed unchanged;
no shell is inserted. The child inherits stdin, stdout, and stderr, and its
exit status is returned by `wt-core`.

```bash
wt exec feature/auth -- cargo test
wt exec feature/auth -- make preview
```

Use `--json` to emit one compact `exec_resolved` metadata object on stderr
before the child starts. The first line is resolution metadata, not a command
result (`resolved: true` means only that the worktree was resolved). Child
stdout and stderr remain inherited. After the metadata line, stderr may contain
inherited child diagnostics or a `wt-core` launch error if the command cannot
be started, so the entire stream is not one parseable JSON document.

The metadata schema is:

```json
{
  "event": "exec_resolved",
  "resolved": true,
  "message": "resolved worktree for branch 'feature/auth'",
  "branch": "feature/auth",
  "repo_root": "/abs/repo",
  "worktree_path": "/abs/repo/.worktrees/feature-auth--a1b2c3d4"
}
```

On Unix, `wt-core` replaces itself with the resolved command, preserving
native stdio, status, and signal behavior. On Windows, it waits for the child
inside a kill-on-close Job Object so descendants are terminated if `wt-core`
is terminated. If containment cannot be established, execution is stopped
rather than leaving an uncontained child. Windows console-control behavior is
still governed by Windows console semantics, and the short CreateProcess/job
attachment interval is an unavoidable residual race.

### `wt list`

Lists all worktrees with branch, commit, and status information. The current
worktree (based on `cwd`) is marked with `← here`. Use `--stats` to include
commit and diff statistics for each non-main worktree against the resolved
mainline, or `--against <rev>` to compare against another revision.

```
/home/user/repo                                    main                 a1b2c3d [main]
/home/user/repo/.worktrees/feature-auth--d4e5f6a7   feature/auth         b2c3d4e ← here

wt list --stats
wt list --stats --against develop
wt list --stats --color never
```

### `wt remove`

Removes a worktree and deletes its local branch. When called without a branch
argument, infers the target from `cwd` or opens a fuzzy picker in a TTY. Use
`--keep-branch` to remove only the worktree; dirty-worktree checks still apply,
and default branch-deletion safety is unchanged.

```
wt remove feature/auth               # explicit branch and branch cleanup
wt remove                             # infer from cwd or pick interactively
wt remove --force                     # remove even if dirty, use -D for branch
wt remove feature/auth --keep-branch  # remove worktree, preserve local branch
```

### `wt merge`

Merges a worktree's branch into the auto-detected mainline using
`--no-ff`, then removes the source worktree and branch by default. Use
`--into` to merge into a different branch that is already checked out in the
main or a linked worktree. The merge and conflict lifecycle runs in the
destination worktree; conflicts remain there for resolution or an explicit
`wt merge --abort`.

```
wt merge                         # merge current worktree's branch
wt merge feature/auth            # explicit branch
wt merge feature/auth --into rc  # merge into checked-out branch rc
wt merge feature/auth --into release/1.0  # destination may be linked
wt merge --push                  # push target branch to origin after merge
wt merge --no-cleanup            # keep worktree and branch after merge
wt merge feature/auth --inspect  # report topology without changing the repo
```

Before a merge, wt-core reports the destination's configured upstream and
commit counts. Synchronized destinations are ready to merge; destinations
that are ahead are called out prominently because a subsequent push includes
those local commits. Destinations behind or diverged from their upstream are
refused before Git's content merge starts, so update or reconcile the target
first. A configured upstream whose remote-tracking ref is unavailable is also
refused rather than treated as an untracked destination. A source that appears
in a standard Git revert record is reported as previously merged then reverted.

`--inspect` performs the same read-only preflight and never fetches, prunes
worktree metadata, merges, pushes, or cleans up. With `--json`, the
`preflight` object contains `upstream`, `ahead`, `behind`, `topology`, source
history, and any structured refusal reason. Topology refusals are distinct
from `content_conflict` refusals.

A content conflict is intentionally left in the destination worktree. The
source worktree and branch are not cleaned up, and wt-core records the
operation under Git's common directory at
`git rev-parse --git-path wt-core/merge-operation.json`. The record is written atomically and includes the schema version, exact
source/destination worktree registrations, captured source and destination
heads, merge commit identity, typed lifecycle progress, push intent, and cleanup
policy. Its directory is private and its state/temp records are owner-only;
insecure existing records are refused. Resolve
and stage every path, then continue through Git's normal merge commit and hook
path:

```bash
wt merge --status              # unresolved paths and pending actions
wt merge --continue            # commit, then perform the original push/cleanup
wt merge --abort               # git merge --abort, then clear matching state
```

`--status --json` reports `state`, `unresolved_paths`, `pending_actions`, and
`recovery` (when state is stale, interrupted, or corrupt). A continuation hook
failure preserves the merge and state for retry. Cleanup and push failures are
reported as warnings and leave committed operation state so `--continue` can
retry them; pushes never force-update a remote. If worktree registration
identity, branch heads, or Git's merge marker no longer match the record,
wt-core refuses mutation and prints recovery guidance rather than selecting a
replacement by branch name.

Only one mutating merge lifecycle may own a repository at a time. An
OS-backed lock is held from preflight through Git subprocess finalization and
all durable journal/cleanup updates; its lock state is released safely by the
OS after process death. A live owner makes a new merge, `--continue`, or
`--abort` fail with recovery guidance, while `--status` remains read-only.

On Windows 10 and Windows Server 2016 or newer, lifecycle Git is managed by a
normally-created guardian outside a private kill-on-close Job Object. The
owner first gives the guardian only a duplicated owner-process handle. The
guardian acquires the child lease by path and publishes a durable READY status
containing its PID and operation ID; only then does the owner publish the
nonce-bound command handoff with non-inheritable stdio/event handles and the
final start authorization. Git is then atomically created suspended in the Job
Object with `PROC_THREAD_ATTRIBUTE_JOB_LIST` and resumed. Git receives the
exact args, environment, working directory, stdio, and supported creation
flags, and cannot break away from the job. The guardian waits Git and
synchronous hooks, terminates and waits every remaining member, and only then
releases the child lease. If the owner dies, the guardian uses its duplicated
owner-process handle rather than capture-pipe EOF as the death signal and
continues that cleanup; recovery remains busy until job quiescence.
Bootstrap and protocol files are owner-only ACL-validated, nonce-bound, and
swept on the next startup after a crash. On Windows, “owner-only” means the
current user plus trusted local SYSTEM and the built-in Administrators group;
those two machine principals intentionally retain full lifecycle access for
system administration and recovery. Existing files and directories must
already satisfy this explicit, protected DACL policy; only paths just created
by wt-core receive the policy. This protects cooperative `wt` operations;
deliberate tampering, killing, or path replacement by an arbitrary same-user
process is outside the threat boundary. Git hooks that daemonize,
detach, or mutate the repository after Git exits are outside the supported
contract and must not be used for lifecycle repository mutation.
Journal updates also compare the operation identity and generation before
replacement or deletion, so a stale process cannot overwrite a newer journal.

The lifecycle lock serializes supported mutating `wt` operations, and Git ref
updates use compare-and-swap checks where a detectable tip race matters. This
is a cooperation boundary, not an operating-system capability claim: an
untrusted same-user process can still swap a filesystem path or mount between
syscalls, and Git provides no atomic API that closes that gap. wt-core fails
closed when its registration, identity, ref, or HEAD checks detect a change,
Removal deliberately has no separate filesystem quarantine or deletion
journal. It retains native `git worktree remove` for Git's cross-platform
cleanup behavior. A successful `wt merge --continue` also consumes the private
navigation record in each shell binding, so callers are moved out of a source
worktree that was removed; successful stderr warnings remain visible in
Nushell.

The Bash, Zsh, Fish, and Nushell bindings pass lifecycle commands through
without applying legacy path-only parsing; `--continue` consumes only its
private navigation record. JSON output remains available for all three
lifecycle commands.

### `wt diff`

Opens Git's configured difftool in directory-diff mode for a worktree branch
against the auto-detected mainline. When called without a branch in a TTY, a
fuzzy picker containing non-main worktrees is shown. Use dirty modes to inspect
uncommitted changes in the selected worktree instead of branch history.

```
wt diff feature/auth                 # git difftool --dir-diff main...feature/auth
wt diff --against develop feature/auth
wt diff --tool vimdiff feature/auth
wt diff --dry-run feature/auth       # print the resolved git command
wt diff --dirty feature/auth         # all uncommitted changes
wt diff --staged feature/auth        # staged changes only
wt diff --unstaged feature/auth      # unstaged changes only
```

### `wt materialize`

Creates a clean detached checkout for a full commit SHA at an explicit absolute
workspace path. This is intended for automation that needs to materialize an
exact repository state without creating or managing a local branch. `--cache-root`
keeps a conservative bare mirror cache per repository slug; `--object-source`
uses a read-only bare repository instead and takes precedence over cache use.

```
wt materialize \
  --repo-slug owner/repo \
  --remote-url https://github.com/owner/repo.git \
  --ref main \
  --sha 0123456789abcdef0123456789abcdef01234567 \
  --cache-root /var/cache/wt-core \
  --workspace-root /tmp/workspaces/owner-repo-01234567 \
  --json
```

### `wt prune`

Scans linked worktrees and preserved local branches and identifies branches
that are fully integrated into the selected revision. Integration is detected
via both ancestry checks (merge/fast-forward) and patch-id comparison (rebase
merges). Defaults to dry-run. `--integrated-into` accepts any revision and
`--mainline` remains an alias.

```
wt prune                                      # dry-run: show cleanup candidates
wt prune --execute                            # remove worktrees and branches
wt prune --execute --force                    # also remove dirty worktrees
wt prune --integrated-into integration/release # use a staged integration target
wt prune --mainline develop                   # backwards-compatible alias
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

| Flag                | Behavior                                                                  |
|---------------------|---------------------------------------------------------------------------|
| *(default)*         | Human-readable text                                                       |
| `--json`            | Single-line JSON on stdout (except `exec`, which emits metadata on stderr) |
| `--print-cd-path`   | Bare absolute path on stdout for `add` and `go`                          |
| `--print-paths`     | Legacy multi-line fields for `remove` and `merge`                       |
| `--print-paths-v2`  | Merge fields plus `destination_path` as line seven                       |
| `--inspect`         | Read-only merge topology preflight (use with `--json` for facts)         |

For `add`, `go`, `remove`, and `merge`, `--json` is authoritative when it is
combined with either legacy path flag. This lets a wrapper safely append its
legacy selector while a caller requests JSON, without producing competing
stdout formats. JSON is exactly one document on stdout; warnings and errors
remain on stderr.

For `remove --print-paths`, the exact legacy protocol is three lines:
`removed_path`, `repo_root`, and `branch`. Use `--json` when lifecycle status is
needed: `worktree_removed` and `branch_deleted` explicitly report each action.
The merge `--print-paths` protocol remains six lines; `--print-paths-v2` adds
`destination_path` as line seven for linked-worktree destinations.

Prune dry-runs report `worktree_present` and `branch_will_be_deleted`; execute
results report `worktree_removed` and `branch_deleted`. A preserved branch has a
null prune `path` because no worktree remains. For commands other than `exec`,
`--json` emits one compact JSON object on stdout. `exec --json` emits only its
first resolution line as JSON on stderr; post-metadata stderr may contain
inherited child diagnostics or a `wt-core` launch error if the command cannot
be started.

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

A `remove --keep-branch --json` response includes the distinct cleanup state:

```json
{
  "worktree_removed": true,
  "branch_deleted": false
}
```

JSON envelope example (`merge`):

```json
{
  "ok": true,
  "message": "merged 'feature/auth' into main",
  "branch": "feature/auth",
  "mainline": "main",
  "destination_path": "/abs/repo",
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

The direct `wt-core` CLI never changes its caller's cwd. Each binding wraps the
binary and may `cd` in the parent shell: normal `add`/`go` use the legacy path
output to enter a worktree, while normal `remove`/`merge` return to the
repository root when cleanup removes the current worktree. With `--json`,
`add` and `go` leave cwd unchanged; JSON `remove` and `merge` still perform that
safe reset when needed. The JSON document remains raw on stdout (including in
Nushell); pipe it through Nushell's `from json` explicitly when structured
values are desired. Warnings and diagnostics remain on stderr.

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
