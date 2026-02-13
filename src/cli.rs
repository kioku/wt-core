use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
        branch: String,

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
