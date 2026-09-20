mod block;
mod cli;
mod commands;
mod diagram;
mod error;
mod frontier;
mod git;
mod state;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    let git = git::Git::new();

    // Mirrors the upstream tool's GIT_IMERGE=1 environment hint for hook
    // scripts to detect that they're running under git-imerge.
    std::env::set_var("GIT_IMERGE", "1");

    let result = match cli.command {
        None => {
            eprintln!("Unrecognized subcommand");
            std::process::exit(1);
        }
        Some(cmd) => run(&git, cmd),
    };

    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(git: &git::Git, cmd: cli::Command) -> error::Result<()> {
    use cli::Command::*;
    match cmd {
        Init(args) => commands::cmd_init(git, &args),
        Start(args) => commands::cmd_start(git, &args),
        Merge(args) => commands::cmd_merge(git, &args),
        Rebase(args) => commands::cmd_rebase(git, &args),
        Drop(args) => commands::cmd_drop(git, &args),
        Revert(args) => commands::cmd_revert(git, &args),
        Continue(args) => commands::cmd_continue(git, &args),
        Record(args) => commands::cmd_record(git, &args),
        Autofill(args) => commands::cmd_autofill(git, &args),
        Simplify(args) => commands::cmd_simplify(git, &args),
        Finish(args) => commands::cmd_finish(git, &args),
        Diagram(args) => commands::cmd_diagram(git, &args),
        List => commands::cmd_list(git),
        Remove(args) => commands::cmd_remove(git, &args),
        Reparent(args) => commands::cmd_reparent(git, &args),
    }
}
