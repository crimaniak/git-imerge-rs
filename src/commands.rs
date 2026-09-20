//! Implementations of each CLI subcommand (mirrors the Python original's
//! `cmd_*` functions -- see the architecture report, section 2.14).

use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use crate::cli::*;
use crate::diagram;
use crate::error::{ImergeError, Result};
use crate::git::Git;
use crate::state::{GoalOpts, MergeState, ALLOWED_GOALS};

fn validate_goal(goal: &str) -> Result<()> {
    if ALLOWED_GOALS.contains(&goal) {
        Ok(())
    } else {
        crate::error::failure(format!(
            "invalid --goal {goal:?} (choose from {})",
            ALLOWED_GOALS.join(", ")
        ))
    }
}

/// Append the upstream tool's "Perhaps use --first-parent?" hint to a
/// non-linear-ancestry error, unless `--first-parent` was already given.
fn hint_first_parent(e: ImergeError, first_parent: bool) -> ImergeError {
    match e {
        ImergeError::NonlinearAncestry(msg) => {
            if first_parent {
                ImergeError::Failure(msg)
            } else {
                ImergeError::Failure(format!("{msg}\nPerhaps use \"--first-parent\"?"))
            }
        }
        other => other,
    }
}

fn maybe_set_default(git: &Git, name: &str) -> Result<()> {
    if git.iter_existing_imerge_names()?.len() > 1 {
        git.set_default_imerge_name(Some(name))?;
    }
    Ok(())
}

/// Shared tail of `init`/`start`/`merge`/`rebase`/`drop`/`revert`: push the
/// frontier as far as it will go, then either report completion or set up
/// the working tree for the next manual merge.
fn drive_to_completion_or_conflict(git: &Git, ms: &mut MergeState) -> Result<()> {
    match ms.auto_complete_frontier(git) {
        Ok(()) => {
            eprintln!("Merge is complete!");
            Ok(())
        }
        Err(ImergeError::FrontierBlocked { i1, i2, .. }) => ms.request_user_merge(git, i1, i2),
        Err(e) => Err(e),
    }
}

pub fn choose_merge_name(git: &Git, name: Option<&str>) -> Result<String> {
    let names = git.iter_existing_imerge_names()?;

    if let Some(name) = name {
        if !names.iter().any(|n| n == name) {
            return crate::error::failure(format!("There is no incremental merge called '{name}'!"));
        }
        if names.len() > 1 {
            git.set_default_imerge_name(Some(name))?;
        }
        return Ok(name.to_string());
    }

    if let Some(default_name) = git.get_default_imerge_name()? {
        return if git.check_imerge_exists(&default_name)? {
            Ok(default_name)
        } else {
            git.set_default_imerge_name(None)?;
            crate::error::failure(format!(
                "Warning: The default incremental merge '{default_name}' has disappeared.\n\
                 (The setting imerge.default has been cleared.)\nPlease try again."
            ))
        };
    }

    if names.len() == 1 && git.check_imerge_exists(&names[0])? {
        return Ok(names[0].clone());
    }

    crate::error::failure("Please select an incremental merge using --name")
}

pub fn read_merge_state(git: &Git, name: Option<&str>) -> Result<MergeState> {
    MergeState::read(git, &choose_merge_name(git, name)?)
}

fn parse_range(git: &Git, range: &str) -> Result<(String, String)> {
    let re = regex::Regex::new(r"^(?P<start>.*[^.])(?P<sep>\.{2,})(?P<end>[^.].*)$").unwrap();
    if let Some(caps) = re.captures(range) {
        if &caps["sep"] != ".." {
            return crate::error::failure("Range must either be a single commit or in the form \"commit..commit\"");
        }
        let start = git.rev_parse_required(&caps["start"])?;
        let end = git.rev_parse_required(&caps["end"])?;
        Ok((start, end))
    } else {
        let end = git.rev_parse_required(range)?;
        let start = git.rev_parse_required(&format!("{end}^"))?;
        Ok((start, end))
    }
}

pub fn cmd_list(git: &Git) -> Result<()> {
    let names = git.iter_existing_imerge_names()?;
    let mut default_merge = git.get_default_imerge_name()?;
    if default_merge.is_none() && names.len() == 1 {
        default_merge = Some(names[0].clone());
    }
    for name in &names {
        if Some(name.as_str()) == default_merge.as_deref() {
            println!("* {name}");
        } else {
            println!("  {name}");
        }
    }
    Ok(())
}

pub fn cmd_init(git: &Git, args: &InitArgs) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    validate_goal(&args.goal)?;
    let name = args
        .name
        .clone()
        .ok_or_else(|| ImergeError::Failure("Please specify the --name to be used for this incremental merge".to_string()))?;
    let tip1 = git.get_head_refname(true).unwrap_or_else(|| "HEAD".to_string());
    let tip2 = args.tip2.clone();

    let (merge_base, commits1, commits2) = git
        .get_boundaries(&tip1, &tip2, args.first_parent)
        .map_err(|e| hint_first_parent(e, args.first_parent))?;

    let mut ms = MergeState::initialize(
        git, &name, &merge_base, &tip1, &commits1, &tip2, &commits2, &args.goal, None, args.manual, args.dedupe_patches, args.branch.as_deref(),
    )?;
    ms.save(git)?;
    maybe_set_default(git, &name)
}

pub fn cmd_start(git: &Git, args: &InitArgs) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    validate_goal(&args.goal)?;
    let name = args
        .name
        .clone()
        .ok_or_else(|| ImergeError::Failure("Please specify the --name to be used for this incremental merge".to_string()))?;
    let tip1 = git.get_head_refname(true).unwrap_or_else(|| "HEAD".to_string());
    let tip2 = args.tip2.clone();

    let (merge_base, commits1, commits2) = git
        .get_boundaries(&tip1, &tip2, args.first_parent)
        .map_err(|e| hint_first_parent(e, args.first_parent))?;

    let mut ms = MergeState::initialize(
        git, &name, &merge_base, &tip1, &commits1, &tip2, &commits2, &args.goal, None, args.manual, args.dedupe_patches, args.branch.as_deref(),
    )?;
    ms.save(git)?;
    maybe_set_default(git, &name)?;
    drive_to_completion_or_conflict(git, &mut ms)
}

pub fn cmd_merge(git: &Git, args: &MergeArgs) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    validate_goal(&args.goal)?;
    let tip2 = args.tip2.clone();

    let name = if let Some(n) = &args.name {
        n.clone()
    } else {
        git.check_imerge_name_format(&tip2)?;
        tip2.clone()
    };

    let mut branch = args.branch.clone();
    let tip1 = match git.get_head_refname(true) {
        Some(t1) => {
            if branch.is_none() && git.check_branch_name_format(&t1).is_ok() {
                branch = Some(t1.clone());
            }
            t1
        }
        None => "HEAD".to_string(),
    };

    if branch.is_none() {
        if args.name.is_some() {
            branch = args.name.clone();
        } else {
            return crate::error::failure("HEAD is not a simple branch.  Please specify --branch for storing results.");
        }
    }

    let (merge_base, commits1, commits2) = match git.get_boundaries(&tip1, &tip2, args.first_parent) {
        Ok(v) => v,
        Err(ImergeError::NothingToDo) => {
            println!("Already up-to-date.");
            return Ok(());
        }
        Err(e) => return Err(hint_first_parent(e, args.first_parent)),
    };

    let mut ms = MergeState::initialize(
        git, &name, &merge_base, &tip1, &commits1, &tip2, &commits2, &args.goal, None, args.manual, args.dedupe_patches, branch.as_deref(),
    )?;
    ms.save(git)?;
    maybe_set_default(git, &name)?;
    drive_to_completion_or_conflict(git, &mut ms)
}

pub fn cmd_rebase(git: &Git, args: &RebaseArgs) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    validate_goal(&args.goal)?;
    let tip1 = args.tip1.clone();

    let mut branch = args.branch.clone();
    let mut name = args.name.clone();
    let tip2 = match git.get_head_refname(true) {
        Some(t2) => {
            if branch.is_none() && git.check_branch_name_format(&t2).is_ok() {
                branch = Some(t2.clone());
            }
            if name.is_none() {
                name = Some(t2.clone());
            }
            t2
        }
        None => git.rev_parse_required("HEAD")?,
    };

    let name = match name {
        Some(n) => n,
        None => {
            return crate::error::failure(
                "The checked-out branch could not be used as the imerge name.\nPlease use the --name option.",
            )
        }
    };
    if branch.is_none() {
        branch = Some(name.clone());
    }

    let (merge_base, commits1, commits2) = match git.get_boundaries(&tip1, &tip2, args.first_parent) {
        Ok(v) => v,
        Err(ImergeError::NothingToDo) => {
            println!("Already up-to-date.");
            return Ok(());
        }
        Err(e) => return Err(hint_first_parent(e, args.first_parent)),
    };

    let mut ms = MergeState::initialize(
        git, &name, &merge_base, &tip1, &commits1, &tip2, &commits2, &args.goal, None, args.manual, args.dedupe_patches, branch.as_deref(),
    )?;
    ms.save(git)?;
    maybe_set_default(git, &name)?;
    drive_to_completion_or_conflict(git, &mut ms)
}

fn cmd_drop_or_revert(git: &Git, args: &RangeArgs, goal: &str) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    let (start, end) = parse_range(git, &args.range)?;
    let to_drop = git
        .linear_ancestry(&start, &end, args.first_parent)
        .map_err(|e| hint_first_parent(e, args.first_parent))?;

    let mut branch = args.branch.clone();
    let mut name = args.name.clone();
    let tip1 = match git.get_head_refname(true) {
        Some(t1) => {
            if branch.is_none() && git.check_branch_name_format(&t1).is_ok() {
                branch = Some(t1.clone());
            }
            if name.is_none() {
                name = Some(t1.clone());
            }
            t1
        }
        None => git.rev_parse_required("HEAD")?,
    };
    let name = match name {
        Some(n) => n,
        None => {
            return crate::error::failure(
                "The checked-out branch could not be used as the imerge name.\nPlease use the --name option.",
            )
        }
    };
    if branch.is_none() {
        branch = Some(name.clone());
    }

    // Build a branch based on `end` containing the inverse of the commits
    // to drop/revert; this becomes tip2.
    git.checkout(&end, false)?;
    for commit in to_drop.iter().rev() {
        git.revert(commit)?;
    }
    let tip2 = git.rev_parse_required("HEAD")?;

    let (merge_base, commits1, commits2) = match git.get_boundaries(&tip1, &tip2, args.first_parent) {
        Ok(v) => v,
        Err(ImergeError::NothingToDo) => {
            println!("Already up-to-date.");
            return Ok(());
        }
        Err(e) => return Err(hint_first_parent(e, args.first_parent)),
    };

    let goalopts = if goal == "drop" {
        Some(GoalOpts { base: Some(start) })
    } else {
        None
    };

    let mut ms = MergeState::initialize(
        git, &name, &merge_base, &tip1, &commits1, &tip2, &commits2, goal, goalopts, args.manual, args.dedupe_patches, branch.as_deref(),
    )?;
    ms.save(git)?;
    maybe_set_default(git, &name)?;
    drive_to_completion_or_conflict(git, &mut ms)
}

pub fn cmd_drop(git: &Git, args: &RangeArgs) -> Result<()> {
    cmd_drop_or_revert(git, args, "drop")
}

pub fn cmd_revert(git: &Git, args: &RangeArgs) -> Result<()> {
    cmd_drop_or_revert(git, args, "revert")
}

pub fn cmd_remove(git: &Git, args: &NameArgs) -> Result<()> {
    let name = choose_merge_name(git, args.name.as_deref())?;
    MergeState::remove(git, &name)
}

pub fn cmd_continue(git: &Git, args: &EditArgs) -> Result<()> {
    let mut ms = read_merge_state(git, args.name.as_deref())?;
    match ms.incorporate_user_merge(git, args.edit_log_msg()) {
        Ok(()) | Err(ImergeError::NoManualMerge(_)) => {}
        Err(e) => return Err(e),
    }
    drive_to_completion_or_conflict(git, &mut ms)
}

pub fn cmd_record(git: &Git, args: &EditArgs) -> Result<()> {
    let mut ms = read_merge_state(git, args.name.as_deref())?;
    ms.incorporate_user_merge(git, args.edit_log_msg())?;
    match ms.auto_complete_frontier(git) {
        Ok(()) => eprintln!("Merge is complete!"),
        Err(ImergeError::FrontierBlocked { .. }) => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

pub fn cmd_autofill(git: &Git, args: &NameArgs) -> Result<()> {
    git.require_clean_work_tree("proceed")?;
    let mut ms = read_merge_state(git, args.name.as_deref())?;
    let _temp_head = git.temporary_head("imerge: restoring")?;
    match ms.auto_complete_frontier(git) {
        Ok(()) => Ok(()),
        Err(ImergeError::FrontierBlocked { msg, .. }) => crate::error::failure(msg),
        Err(e) => Err(e),
    }
}

fn simplify_common(git: &Git, args: &SimplifyArgs) -> Result<MergeState> {
    git.require_clean_work_tree("proceed")?;
    let mut ms = read_merge_state(git, args.name.as_deref())?;
    if !ms.map_frontier()?.is_complete(&ms.grid)? {
        return crate::error::failure(format!("Merge {} is not yet complete!", ms.name()));
    }
    let branch = args.branch.clone().unwrap_or_else(|| ms.branch.clone());
    let refname = format!("refs/heads/{branch}");
    if let Some(goal) = &args.goal {
        validate_goal(goal)?;
        ms.set_goal(git, goal)?;
        ms.save(git)?;
    }
    ms.simplify(git, &refname, args.force)?;
    Ok(ms)
}

pub fn cmd_simplify(git: &Git, args: &SimplifyArgs) -> Result<()> {
    simplify_common(git, args)?;
    Ok(())
}

pub fn cmd_finish(git: &Git, args: &SimplifyArgs) -> Result<()> {
    let ms = simplify_common(git, args)?;
    MergeState::remove(git, ms.name())
}

pub fn cmd_diagram(git: &Git, args: &DiagramArgs) -> Result<()> {
    let show_commits = args.commits;
    let mut show_frontier = args.frontier;
    if !show_commits && !show_frontier {
        show_frontier = true;
    }
    // Explicit --color/--no-color always win; otherwise follow isatty.
    let color_enabled = if args.no_color {
        false
    } else if args.color {
        true
    } else {
        std::io::stdout().is_terminal()
    };

    let ms = read_merge_state(git, args.name.as_deref())?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if show_commits {
        diagram::write_commits_diagram(&mut out, &ms.grid, &ms.tip1, &ms.tip2, color_enabled)?;
        writeln!(out)?;
    }
    if show_frontier {
        let frontier = ms.map_frontier()?;
        diagram::write_frontier_diagram(&mut out, &ms.grid, &frontier, &ms.tip1, &ms.tip2, color_enabled)?;
        writeln!(out)?;
    }
    if let Some(html_path) = &args.html {
        let frontier = ms.map_frontier()?;
        let mut f = std::fs::File::create(html_path)?;
        diagram::write_html(&mut f, &ms.grid, &frontier, ms.name(), "imerge.css", 7)?;
    }

    // Silence the unused-assignment warning when both flags were given.
    let _ = show_commits;

    writeln!(out, "Key:")?;
    if show_frontier {
        write!(out, "{}", diagram::FRONTIER_LEGEND_LINE)?;
    }
    write!(out, "{}", diagram::LEGEND)?;
    Ok(())
}

fn reparent_recursively(git: &Git, start_commit: &str, parents: &[String], end_commit: &str) -> Result<String> {
    let mut replacements: HashMap<String, String> = HashMap::new();
    replacements.insert(start_commit.to_string(), git.reparent(start_commit, parents, None)?);

    for (commit, orig_parents) in git.rev_list_with_parents(&[
        "--ancestry-path",
        "--topo-order",
        "--reverse",
        &format!("{start_commit}..{end_commit}"),
    ])? {
        let new_parents: Vec<String> = orig_parents
            .iter()
            .map(|p| replacements.get(p).cloned().unwrap_or_else(|| p.clone()))
            .collect();
        replacements.insert(commit.clone(), git.reparent(&commit, &new_parents, None)?);
    }

    replacements
        .remove(end_commit)
        .ok_or_else(|| ImergeError::Failure(format!("{start_commit} is not an ancestor of {end_commit}")))
}

pub fn cmd_reparent(git: &Git, args: &ReparentArgs) -> Result<()> {
    let start_commit = git.get_commit_sha1(&args.commit)?;
    let head = git.get_commit_sha1("HEAD")?;
    let mut parents = Vec::new();
    for p in &args.parents {
        parents.push(git.get_commit_sha1(p)?);
    }
    let new_head = reparent_recursively(git, &start_commit, &parents, &head)?;
    println!("{new_head}");
    Ok(())
}
