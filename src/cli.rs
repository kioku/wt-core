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

    /// Remove a worktree and its local branch
    Remove {
        /// Branch name (defaults to current worktree's branch)
        branch: Option<String>,

        /// Force removal even if dirty; use -D for branch deletion
        #[arg(long)]
        force: bool,

        /// Repository path (defaults to current directory)
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Print removed_path, repo_root, and branch (one per line) for shell wrappers
        #[arg(long, conflicts_with = "json")]
        print_paths: bool,
    },

    /// Merge a worktree's branch into mainline and clean up
    Merge {
        /// Branch name (defaults to current worktree's branch)
        branch: Option<String>,

        /// Push mainline to origin after successful merge
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
        #[arg(long, conflicts_with = "json")]
        print_paths: bool,
    },

    /// Remove worktrees whose branches are fully integrated into mainline
    Prune {
        /// Actually remove integrated worktrees (default is dry-run)
        #[arg(long)]
        execute: bool,

        /// Force removal of dirty worktrees and use -D for branch deletion
        #[arg(long, requires = "execute")]
        force: bool,

        /// Override mainline branch (default: auto-detect)
        #[arg(long)]
        mainline: Option<String>,

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
