use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "wt-core",
    version,
    about = "Portable Git worktree lifecycle manager"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List all worktrees in the repository
    List {
        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Include commit and diff stats for each worktree
        #[arg(long)]
        stats: bool,

        /// Compare stats against this revision (defaults to resolved mainline)
        #[arg(long, requires = "stats")]
        against: Option<String>,

        /// When to color stats output
        #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
        color: ColorChoice,
    },

    /// Create a new worktree and branch
    Add {
        /// Branch name to create
        branch: String,

        /// Base revision to branch from (defaults to HEAD)
        #[arg(long)]
        base: Option<String>,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print only the worktree path (for shell wrappers)
        #[arg(long, conflicts_with = "json")]
        print_cd_path: bool,
    },

    /// Switch to an existing worktree
    Go {
        /// Branch name of the worktree to switch to
        branch: Option<String>,

        /// Force the interactive picker (skip auto-select)
        #[arg(short, long, conflicts_with_all = ["branch", "json"])]
        interactive: bool,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print only the worktree path (for shell wrappers)
        #[arg(long, conflicts_with = "json")]
        print_cd_path: bool,
    },

    /// Execute a command in an existing worktree
    Exec {
        /// Branch name of the worktree to execute in
        branch: String,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Emit one resolved-worktree metadata line on stderr
        /// (child stderr follows on the same stream)
        #[arg(long)]
        json: bool,

        /// Command and arguments to execute; must follow `--`
        #[arg(last = true, required = true, num_args = 1.., value_name = "COMMAND")]
        command: Vec<OsString>,
    },

    /// Remove a worktree and its local branch
    Remove {
        /// Branch name (defaults to current worktree's branch)
        branch: Option<String>,

        /// Force removal even if dirty; use -D for branch deletion unless the branch is kept
        #[arg(long)]
        force: bool,

        /// Remove the worktree but preserve its local branch
        #[arg(long)]
        keep_branch: bool,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print the legacy three-line removed_path, repo_root, branch protocol for shell wrappers
        #[arg(long, conflicts_with = "json")]
        print_paths: bool,
    },

    /// Merge a worktree's branch into a checked-out target and clean up
    Merge {
        /// Branch name (defaults to current worktree's branch)
        branch: Option<String>,

        /// Merge into this branch checked out in the main or a linked worktree
        #[arg(long, value_name = "BRANCH")]
        into: Option<String>,

        /// Push the target branch to origin after successful merge
        #[arg(long)]
        push: bool,

        /// Keep worktree and branch after merge (skip cleanup)
        #[arg(long)]
        no_cleanup: bool,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print merge info (repo_root, branch, mainline, cleaned_up, removed_path, pushed — one per line) for shell wrappers
        #[arg(long, conflicts_with_all = ["json", "print_paths_v2"])]
        print_paths: bool,

        /// Print version 2 merge info, including destination_path, for shell wrappers
        #[arg(long, conflicts_with_all = ["json", "print_paths"])]
        print_paths_v2: bool,
    },

    /// Materialize an explicit detached checkout at a workspace path
    ///
    /// Creates a clean detached checkout for a full commit SHA at an explicit
    /// absolute --workspace-root. When --cache-root is provided, wt-core keeps
    /// a conservative bare mirror cache under <cache-root>/<safe-repo-key>.git
    /// and serializes refreshes per repository. When --object-source is
    /// provided, it is treated as a read-only bare repository and takes
    /// precedence over cache use. JSON output includes repository,
    /// workspace_path, cache_path, requested_ref, requested_sha,
    /// resolved_commit, mode, cache_status, source, and timings_ms.
    Materialize {
        /// Relative owner/repository-style repository slug
        #[arg(long)]
        repo_slug: String,

        /// Remote repository URL used for fetching or cache population
        #[arg(long)]
        remote_url: Option<String>,

        /// Ref used as fetch/reachability context
        #[arg(long = "ref")]
        ref_: Option<String>,

        /// Full 40-character commit SHA to check out
        #[arg(long)]
        sha: String,

        /// Absolute directory containing wt-core bare mirror caches
        #[arg(long)]
        cache_root: Option<PathBuf>,

        /// Absolute path where the detached workspace will be created
        #[arg(long)]
        workspace_root: PathBuf,

        /// Absolute path to a read-only bare repository used as an object source
        #[arg(long)]
        object_source: Option<PathBuf>,

        /// Checkout mode; only detached is supported in this version
        #[arg(long, value_enum, default_value_t = MaterializeMode::Detached)]
        mode: MaterializeMode,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Open a difftool for a worktree branch or dirty worktree changes
    Diff {
        /// Branch name of the worktree to diff
        branch: Option<String>,

        /// Compare against this revision (defaults to resolved mainline)
        #[arg(long)]
        against: Option<String>,

        /// Inspect all uncommitted changes in the selected worktree
        #[arg(long)]
        dirty: bool,

        /// Inspect staged changes only in the selected worktree
        #[arg(long)]
        staged: bool,

        /// Inspect unstaged changes only in the selected worktree
        #[arg(long)]
        unstaged: bool,

        /// Git difftool name to use
        #[arg(long)]
        tool: Option<String>,

        /// Print the resolved command without launching difftool
        #[arg(long)]
        dry_run: bool,

        /// Print the resolved command without launching difftool
        #[arg(long)]
        print_command: bool,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Remove worktrees and branches fully integrated into a target revision
    Prune {
        /// Actually remove integrated worktrees and branches (default is dry-run)
        #[arg(long)]
        execute: bool,

        /// Force removal of dirty worktrees and use -D for branch deletion
        #[arg(long, requires = "execute")]
        force: bool,

        /// Revision to evaluate integration against (default: auto-detect)
        #[arg(long = "integrated-into", visible_alias = "mainline")]
        integrated_into: Option<String>,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Generate .wt/symlinks config from detected project ecosystems
    Setup {
        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Print shell bindings to stdout
    Init {
        /// Shell to generate bindings for
        shell: Shell,
    },

    /// Diagnose worktree and repository health
    Doctor {
        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Nu,
}

#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterializeMode {
    Detached,
}

#[derive(ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}
