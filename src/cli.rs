//! Command-line surface, mirroring the Python original's `argparse` setup
//! subcommand-for-subcommand (see the architecture report, section 5).

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "git-imerge",
    version,
    about = "Perform a merge or rebase between two branches incrementally."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new incremental merge
    Init(InitArgs),
    /// Start a new incremental merge (init + continue)
    Start(InitArgs),
    /// Start a simple merge via incremental merge
    Merge(MergeArgs),
    /// Start a simple rebase via incremental merge
    Rebase(RebaseArgs),
    /// Drop one or more commits via incremental merge
    Drop(RangeArgs),
    /// Revert one or more commits via incremental merge
    Revert(RangeArgs),
    /// Record the merge at branch imerge/NAME and continue
    Continue(EditArgs),
    /// Record the merge at branch imerge/NAME
    Record(EditArgs),
    /// Autofill non-conflicting merges
    Autofill(NameArgs),
    /// Simplify a completed incremental merge
    Simplify(SimplifyArgs),
    /// Simplify then remove a completed incremental merge
    Finish(SimplifyArgs),
    /// Display a diagram of the current state of a merge
    Diagram(DiagramArgs),
    /// List the names of incremental merges in progress
    List,
    /// Irrevocably remove an incremental merge
    Remove(NameArgs),
    /// Change the parents of a commit and propagate to HEAD
    Reparent(ReparentArgs),
}

#[derive(clap::Args)]
pub struct NameArgs {
    /// Name of the incremental merge
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(clap::Args)]
pub struct InitArgs {
    #[arg(long)]
    pub name: Option<String>,
    /// The goal of the incremental merge
    #[arg(long, default_value = "merge")]
    pub goal: String,
    /// The branch to which the result will be stored
    #[arg(long)]
    pub branch: Option<String>,
    /// Ask the user to complete all merges manually
    #[arg(long)]
    pub manual: bool,
    /// Disable automatic resolution of conflicts caused by the same patch
    /// having been independently applied to both branches (e.g. a
    /// cherry-picked hotfix), as detected via `git patch-id --stable`
    #[arg(long = "no-dedupe-patches", action = clap::ArgAction::SetFalse)]
    pub dedupe_patches: bool,
    /// Handle only first-parent commits (required if history is nonlinear)
    #[arg(long)]
    pub first_parent: bool,
    /// The tip of the branch to be merged into HEAD
    pub tip2: String,
}

#[derive(clap::Args)]
pub struct MergeArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, default_value = "merge")]
    pub goal: String,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub manual: bool,
    /// Disable automatic resolution of conflicts caused by the same patch
    /// having been independently applied to both branches (e.g. a
    /// cherry-picked hotfix), as detected via `git patch-id --stable`
    #[arg(long = "no-dedupe-patches", action = clap::ArgAction::SetFalse)]
    pub dedupe_patches: bool,
    #[arg(long, hide = true, default_value_t = true, action = clap::ArgAction::SetTrue)]
    pub first_parent: bool,
    pub tip2: String,
}

#[derive(clap::Args)]
pub struct RebaseArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, default_value = "rebase")]
    pub goal: String,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub manual: bool,
    /// Disable automatic resolution of conflicts caused by the same patch
    /// having been independently applied to both branches (e.g. a
    /// cherry-picked hotfix), as detected via `git patch-id --stable`
    #[arg(long = "no-dedupe-patches", action = clap::ArgAction::SetFalse)]
    pub dedupe_patches: bool,
    #[arg(long, hide = true, default_value_t = true, action = clap::ArgAction::SetTrue)]
    pub first_parent: bool,
    /// The tip of the branch onto which the current branch should be rebased
    pub tip1: String,
}

#[derive(clap::Args)]
pub struct RangeArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub manual: bool,
    /// Disable automatic resolution of conflicts caused by the same patch
    /// having been independently applied to both branches (e.g. a
    /// cherry-picked hotfix), as detected via `git patch-id --stable`
    #[arg(long = "no-dedupe-patches", action = clap::ArgAction::SetFalse)]
    pub dedupe_patches: bool,
    #[arg(long, default_value_t = true, action = clap::ArgAction::SetTrue)]
    pub first_parent: bool,
    /// The commit or range of commits ("commit" or "commit..commit")
    pub range: String,
}

#[derive(clap::Args)]
pub struct EditArgs {
    #[arg(long)]
    pub name: Option<String>,
    /// Commit staged changes with --edit
    #[arg(short = 'e', long, action = clap::ArgAction::SetTrue)]
    pub edit: bool,
    /// Commit staged changes with --no-edit
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub no_edit: bool,
}

impl EditArgs {
    pub fn edit_log_msg(&self) -> Option<bool> {
        if self.no_edit {
            Some(false)
        } else if self.edit {
            Some(true)
        } else {
            None
        }
    }
}

#[derive(clap::Args)]
pub struct SimplifyArgs {
    #[arg(long)]
    pub name: Option<String>,
    /// Simplification goal (default: the value provided to init/start)
    #[arg(long)]
    pub goal: Option<String>,
    #[arg(long)]
    pub branch: Option<String>,
    /// Allow the target branch to be updated non-fast-forward
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args)]
pub struct DiagramArgs {
    #[arg(long)]
    pub name: Option<String>,
    /// Show the merges that have been made so far
    #[arg(long)]
    pub commits: bool,
    /// Show the current merge frontier
    #[arg(long)]
    pub frontier: bool,
    /// Generate an HTML diagram at this path
    #[arg(long)]
    pub html: Option<String>,
    /// Draw diagram with colors
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub color: bool,
    /// Draw diagram without colors
    #[arg(long = "no-color", action = clap::ArgAction::SetTrue)]
    pub no_color: bool,
}

#[derive(clap::Args)]
pub struct ReparentArgs {
    /// Target commit to reparent
    #[arg(long, default_value = "HEAD")]
    pub commit: String,
    /// The new parent commits
    pub parents: Vec<String>,
}
