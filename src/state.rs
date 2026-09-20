//! `MergeState`: the root object for one incremental merge. Owns the grid,
//! reads/writes it to `refs/imerge/<name>/*`, and drives the auto-merge
//! loop and the various "simplify to a final result" strategies.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::block::{Grid, MergeRecord, Rect};
use crate::error::{ImergeError, Result};
use crate::frontier::{self, Frontier};
use crate::git::Git;

/// Major.minor.patch state-format version. Matches the upstream Python
/// tool's own `STATE_VERSION` deliberately: the on-disk format (refs
/// layout + JSON schema) is identical, so imerges created by either
/// implementation can be read/continued by the other.
pub const STATE_VERSION: (u32, u32, u32) = (1, 3, 0);

pub const ALLOWED_GOALS: &[&str] = &[
    "full",
    "rebase",
    "rebase-with-history",
    "border",
    "border-with-history",
    "border-with-history2",
    "merge",
    "drop",
    "revert",
];

#[allow(dead_code)]
pub const DEFAULT_GOAL: &str = "merge";

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Eq, Debug)]
pub struct GoalOpts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct StateDict {
    pub version: String,
    #[serde(default)]
    pub blockers: Vec<(usize, usize)>,
    #[serde(default)]
    pub tip1: Option<String>,
    #[serde(default)]
    pub tip2: Option<String>,
    pub goal: String,
    #[serde(default)]
    pub goalopts: Option<GoalOpts>,
    #[serde(default)]
    pub manual: bool,
    #[serde(default = "default_true")]
    pub dedupe_patches: bool,
    #[serde(default)]
    pub branch: Option<String>,
}

/// Either a raw SHA-1 or a reference to a grid cell, resolved lazily. Used
/// by [`MergeState::simplify_to_path`] to describe a chain of commits
/// whose tree/metadata may come from cells that aren't known yet.
pub enum PathRef {
    Sha1(String),
    Cell(usize, usize),
}

impl PathRef {
    fn to_sha1(&self, grid: &Grid) -> Result<String> {
        match self {
            PathRef::Sha1(s) => Ok(s.clone()),
            PathRef::Cell(i1, i2) => grid
                .get(*i1, *i2)
                .sha1
                .clone()
                .ok_or(ImergeError::MissingMerge { i1: *i1, i2: *i2 }),
        }
    }
}

pub fn scratch_refname(name: &str) -> String {
    format!("refs/heads/imerge/{name}")
}

fn check_no_merges(git: &Git, commits: &[String]) -> Result<()> {
    let mut bad = Vec::new();
    for c in commits {
        if git.get_commit_parents(c)?.len() > 1 {
            bad.push(c.clone());
        }
    }
    if !bad.is_empty() {
        return crate::error::failure(format!(
            "The following commits on the to-be-rebased branch are merge commits:\n    {}\n--goal='rebase' is not yet supported for branches that include merges.\n",
            bad.join("\n    ")
        ));
    }
    Ok(())
}

pub struct MergeState {
    pub grid: Grid,
    pub tip1: String,
    pub tip2: String,
    pub goal: String,
    pub goalopts: Option<GoalOpts>,
    pub manual: bool,
    pub branch: String,
}

impl MergeState {
    pub fn name(&self) -> &str {
        &self.grid.name
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        name: &str,
        merge_base: &str,
        tip1: &str,
        commits1: &[String],
        tip2: &str,
        commits2: &[String],
        source: u8,
        goal: &str,
        goalopts: Option<GoalOpts>,
        manual: bool,
        dedupe_patches: bool,
        branch: String,
    ) -> MergeState {
        let len1 = commits1.len() + 1;
        let len2 = commits2.len() + 1;
        let mut grid = Grid::new(name, len1, len2);
        grid.dedupe_patches = dedupe_patches;
        grid.get_mut(0, 0).record_merge(merge_base, source);
        for (i1, c) in commits1.iter().enumerate() {
            grid.get_mut(i1 + 1, 0).record_merge(c, source);
        }
        for (i2, c) in commits2.iter().enumerate() {
            grid.get_mut(0, i2 + 1).record_merge(c, source);
        }
        let branch = if branch.is_empty() { name.to_string() } else { branch };
        MergeState {
            grid,
            tip1: tip1.to_string(),
            tip2: tip2.to_string(),
            goal: goal.to_string(),
            goalopts,
            manual,
            branch,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        git: &Git,
        name: &str,
        merge_base: &str,
        tip1: &str,
        commits1: &[String],
        tip2: &str,
        commits2: &[String],
        goal: &str,
        goalopts: Option<GoalOpts>,
        manual: bool,
        dedupe_patches: bool,
        branch: Option<&str>,
    ) -> Result<MergeState> {
        git.verify_imerge_name_available(name)?;
        let branch = match branch {
            Some(b) if !b.is_empty() => {
                git.check_branch_name_format(b)?;
                b.to_string()
            }
            _ => name.to_string(),
        };
        if goal == "rebase" {
            check_no_merges(git, commits2)?;
        }
        Ok(MergeState::new(
            name,
            merge_base,
            tip1,
            commits1,
            tip2,
            commits2,
            MergeRecord::NEW_MANUAL,
            goal,
            goalopts,
            manual,
            dedupe_patches,
            branch,
        ))
    }

    pub fn read(git: &Git, name: &str) -> Result<MergeState> {
        let (state, merges_raw) = git.read_imerge_state(name)?;
        let mut merges: HashMap<(usize, usize), (String, u8)> = HashMap::new();
        for ((i1, i2), (sha1, source)) in merges_raw {
            let src = match source.as_str() {
                "auto" => MergeRecord::SAVED_AUTO,
                "manual" => MergeRecord::SAVED_MANUAL,
                other => return crate::error::failure(format!("unknown merge source {other:?}")),
            };
            merges.insert((i1, i2), (sha1, src));
        }
        let blockers = state.blockers.clone();

        let (merge_base, msrc) = merges
            .remove(&(0, 0))
            .ok_or_else(|| ImergeError::Failure("Merge base is missing!".to_string()))?;
        if msrc != MergeRecord::SAVED_MANUAL {
            return crate::error::failure("Merge base should be manual!");
        }

        let mut commits1 = Vec::new();
        let mut i1 = 1usize;
        loop {
            match merges.remove(&(i1, 0)) {
                Some((sha1, src)) => {
                    if src != MergeRecord::SAVED_MANUAL {
                        return crate::error::failure(format!("Merge {i1}-0 should be manual!"));
                    }
                    commits1.push(sha1);
                    i1 += 1;
                }
                None => break,
            }
        }

        let mut commits2 = Vec::new();
        let mut i2 = 1usize;
        loop {
            match merges.remove(&(0, i2)) {
                Some((sha1, src)) => {
                    if src != MergeRecord::SAVED_MANUAL {
                        return crate::error::failure(format!("Merge 0-{i2} should be manual!"));
                    }
                    commits2.push(sha1);
                    i2 += 1;
                }
                None => break,
            }
        }

        let tip1 = state
            .tip1
            .clone()
            .or_else(|| commits1.last().cloned())
            .ok_or_else(|| ImergeError::Failure("could not determine tip1".to_string()))?;
        let tip2 = state
            .tip2
            .clone()
            .or_else(|| commits2.last().cloned())
            .ok_or_else(|| ImergeError::Failure("could not determine tip2".to_string()))?;

        if !ALLOWED_GOALS.contains(&state.goal.as_str()) {
            return crate::error::failure(format!("Goal {:?}, read from state, is not recognized.", state.goal));
        }

        let branch = state.branch.clone().unwrap_or_else(|| name.to_string());

        let mut ms = MergeState::new(
            name,
            &merge_base,
            &tip1,
            &commits1,
            &tip2,
            &commits2,
            MergeRecord::SAVED_MANUAL,
            &state.goal,
            state.goalopts.clone(),
            state.manual,
            state.dedupe_patches,
            branch,
        );

        for ((i1, i2), (sha1, source)) in merges {
            if i1 == 0 && i2 >= ms.grid.len2 {
                return crate::error::failure(format!("Merge 0-{} is missing!", ms.grid.len2));
            }
            if i1 >= ms.grid.len1 && i2 == 0 {
                return crate::error::failure(format!("Merge {}-0 is missing!", ms.grid.len1));
            }
            if i1 >= ms.grid.len1 || i2 >= ms.grid.len2 {
                return crate::error::failure(format!(
                    "Merge {i1}-{i2} is out of range [0:{},0:{}]",
                    ms.grid.len1, ms.grid.len2
                ));
            }
            ms.grid.get_mut(i1, i2).record_merge(&sha1, source);
        }

        for (i1, i2) in blockers {
            ms.grid.get_mut(i1, i2).record_blocked(true);
        }

        Ok(ms)
    }

    pub fn remove(git: &Git, name: &str) -> Result<()> {
        let scratch = scratch_refname(name);
        if git.get_head_refname(false).as_deref() == Some(scratch.as_str()) {
            let _ = git.abort_merge();
            git.detach(&format!("Detach HEAD from {scratch}"))?;
        }
        let _ = git.delete_ref(&scratch, &format!("imerge {name}: remove scratch reference"));
        git.delete_imerge_refs(name)?;
        if git.get_default_imerge_name()?.as_deref() == Some(name) {
            git.set_default_imerge_name(None)?;
        }
        Ok(())
    }

    pub fn set_goal(&mut self, git: &Git, goal: &str) -> Result<()> {
        if !ALLOWED_GOALS.contains(&goal) {
            return crate::error::failure(format!("{goal:?} is not an allowed goal"));
        }
        if goal == "rebase" {
            let commits2: Vec<String> = (1..self.grid.len2)
                .map(|i2| self.grid.get(0, i2).sha1.clone().unwrap())
                .collect();
            check_no_merges(git, &commits2)?;
        }
        self.goal = goal.to_string();
        Ok(())
    }

    pub fn map_frontier(&self) -> Result<Frontier> {
        let full = Rect::full(self.grid.len1, self.grid.len2);
        if self.manual {
            Ok(Frontier::Manual(full))
        } else if self.goal == "full" {
            Ok(Frontier::Full(full))
        } else {
            let (top, blocks) = frontier::map_known_frontier(&self.grid, full)?;
            Ok(Frontier::Blockwise { top, blocks })
        }
    }

    /// Complete the frontier using automerges as far as possible, saving
    /// state after every step so interrupted progress is never lost.
    pub fn auto_complete_frontier(&mut self, git: &Git) -> Result<()> {
        let mut progress_made = false;
        loop {
            let frontier = self.map_frontier()?;
            let result = frontier.auto_expand(git, &mut self.grid);
            self.save(git)?;
            match result {
                Ok(()) => {
                    progress_made = true;
                }
                Err(ImergeError::BlockComplete) => return Ok(()),
                Err(ImergeError::FrontierBlocked { i1, i2, msg }) => {
                    if !progress_made {
                        return Err(ImergeError::FrontierBlocked {
                            i1,
                            i2,
                            msg: format!("No progress was possible; suggest manual merge of {i1}-{i2}"),
                        });
                    } else {
                        return Err(ImergeError::FrontierBlocked { i1, i2, msg });
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn find_index(&self, commit: &str) -> Result<(usize, usize)> {
        for i2 in 0..self.grid.len2 {
            for i1 in 0..self.grid.len1 {
                if let Some(sha1) = &self.grid.get(i1, i2).sha1 {
                    if sha1 == commit {
                        return Ok((i1, i2));
                    }
                }
            }
        }
        Err(ImergeError::CommitNotFound(commit.to_string()))
    }

    /// Prepare the working tree for the user to resolve a manual merge at
    /// `(i1, i2)`; assumes the merges above and to the left are known.
    pub fn request_user_merge(&self, git: &Git, i1: usize, i2: usize) -> Result<()> {
        let above = self
            .grid
            .get(i1, i2 - 1)
            .sha1
            .clone()
            .ok_or_else(|| ImergeError::Failure(format!("The parents of merge {i1}-{i2} are not ready")))?;
        let left = self
            .grid
            .get(i1 - 1, i2)
            .sha1
            .clone()
            .ok_or_else(|| ImergeError::Failure(format!("The parents of merge {i1}-{i2} are not ready")))?;

        let refname = scratch_refname(self.name());
        git.update_ref(&refname, &above, &format!("imerge '{}': Prepare merge {i1}-{i2}", self.name()), true)?;
        git.checkout(&refname, false)?;
        let logmsg = format!("imerge '{}': manual merge {i1}-{i2}", self.name());
        // A conflict here is expected -- that's the whole point.
        let _ = git.manualmerge(&left, &logmsg);

        eprintln!("\nOriginal first commit:");
        git.summarize_commit(&self.grid.get(i1, 0).sha1.clone().unwrap())?;
        eprintln!("\nOriginal second commit:");
        git.summarize_commit(&self.grid.get(0, i2).sha1.clone().unwrap())?;
        eprintln!(
            "\nThere was a conflict merging commit {i1}-{i2}, shown above.\n\
             Please resolve the conflict, commit the result, then type\n\n    git-imerge continue\n"
        );
        Ok(())
    }

    /// Record `commit` (made by the user) as a manual merge of its two
    /// parents, which must be known, diagonally-adjacent grid cells.
    pub fn incorporate_manual_merge(&mut self, git: &Git, commit: &str) -> Result<(usize, usize)> {
        let parents = git.get_commit_parents(commit)?;
        if parents.len() < 2 {
            return Err(ImergeError::ManualMergeUnusable {
                commit: commit.to_string(),
                msg: "it is not a merge".to_string(),
            });
        }
        if parents.len() > 2 {
            return Err(ImergeError::ManualMergeUnusable {
                commit: commit.to_string(),
                msg: "it is an octopus merge".to_string(),
            });
        }

        let (mut i1first, mut i2first) = match self.find_index(&parents[0]) {
            Ok(idx) => idx,
            Err(_) => {
                return Err(ImergeError::ManualMergeUnusable {
                    commit: commit.to_string(),
                    msg: "its parents are not known merge commits".to_string(),
                })
            }
        };
        let (mut i1second, mut i2second) = match self.find_index(&parents[1]) {
            Ok(idx) => idx,
            Err(_) => {
                return Err(ImergeError::ManualMergeUnusable {
                    commit: commit.to_string(),
                    msg: "its parents are not known merge commits".to_string(),
                })
            }
        };

        let mut swapped = false;
        if i1first < i1second {
            std::mem::swap(&mut i1first, &mut i1second);
            std::mem::swap(&mut i2first, &mut i2second);
            swapped = true;
        }
        if i1first as i64 != i1second as i64 + 1 || i2first as i64 != i2second as i64 - 1 {
            return Err(ImergeError::ManualMergeUnusable {
                commit: commit.to_string(),
                msg: "it is not a pairwise merge of adjacent parents".to_string(),
            });
        }

        let commit = if swapped {
            git.reparent(commit, &[parents[1].clone(), parents[0].clone()], None)?
        } else {
            commit.to_string()
        };

        let (i1, i2) = (i1first, i2second);
        self.grid.get_mut(i1, i2).record_merge(&commit, MergeRecord::NEW_MANUAL);
        Ok((i1, i2))
    }

    /// If the user has completed the manual merge we asked for, incorporate
    /// it. See the architecture report for the exact recovery-message
    /// wording this mirrors.
    pub fn incorporate_user_merge(&mut self, git: &Git, edit_log_msg: Option<bool>) -> Result<()> {
        let refname = scratch_refname(self.name());
        let mut commit = git
            .get_commit_sha1(&refname)
            .map_err(|_| ImergeError::NoManualMerge(format!("Reference {refname} does not exist.")))?;

        let head_name = git.get_head_refname(false);
        match &head_name {
            None => return Err(ImergeError::NoManualMerge("HEAD is currently detached.".to_string())),
            Some(h) if h != &refname => {
                if self.find_index(&commit).is_ok() {
                    git.delete_ref(&refname, &format!("imerge '{}': Remove obsolete scratch reference", self.name()))?;
                    eprintln!("{refname} did not point to a new merge; it has been deleted.");
                    return Err(ImergeError::NoManualMerge(format!("Reference {refname} was not checked out.")));
                } else {
                    return crate::error::failure(format!(
                        "The scratch reference, {refname}, already exists but is not\n\
                         checked out.  If it points to a merge commit that you would like\n\
                         to use, please check it out using\n\n    git checkout {refname}\n\n\
                         and then try to continue again.  If it points to a commit that is\n\
                         unneeded, then please delete the reference using\n\n    git update-ref -d {refname}\n\n\
                         and then continue."
                    ));
                }
            }
            Some(_) => {}
        }

        if git.commit_user_merge(edit_log_msg)? {
            commit = git.get_commit_sha1("HEAD")?;
        }
        git.require_clean_work_tree("proceed")?;

        let (i1, i2) = self.incorporate_manual_merge(git, &commit)?;

        git.detach(&format!("Detach HEAD from {refname}"))?;
        git.delete_ref(&refname, &format!("imerge '{}': remove scratch reference", self.name()))?;

        let frontier = self.map_frontier()?;
        let result = frontier.incorporate_merge(&mut self.grid, i1, i2);
        if result.is_ok() {
            eprintln!("Merge has been recorded for merge {i1}-{i2}.");
        }
        self.save(git)?;
        result
    }

    fn set_refname(&self, git: &Git, refname: &str, commit: &str, force: bool) -> Result<()> {
        match git.get_commit_sha1(refname) {
            Err(_) => {
                git.update_ref(refname, commit, "imerge: recording final merge", true)?;
                git.checkout(refname, true)?;
            }
            Ok(old) => {
                let head_refname = git.get_head_refname(false);
                if !force && !git.is_ancestor(&old, commit)? {
                    return crate::error::failure(format!("{refname} cannot be fast-forwarded to {commit}!"));
                }
                if head_refname.as_deref() == Some(refname) {
                    git.reset_hard(Some(commit))?;
                } else {
                    git.update_ref(refname, commit, "imerge: recording final merge", true)?;
                    git.checkout(refname, true)?;
                }
            }
        }
        Ok(())
    }

    fn simplify_to_full(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        for i1 in 1..self.grid.len1 {
            for i2 in 1..self.grid.len2 {
                if !self.grid.is_known(i1, i2) {
                    return crate::error::failure(format!(
                        "Cannot simplify to \"full\" because merge {i1}-{i2} is not yet done"
                    ));
                }
            }
        }
        let vertex = self.grid.get(self.grid.len1 - 1, self.grid.len2 - 1).sha1.clone().unwrap();
        self.set_refname(git, refname, &vertex, force)
    }

    fn simplify_to_rebase_with_history(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        let i1 = self.grid.len1 - 1;
        for i2 in 1..self.grid.len2 {
            if !self.grid.is_known(i1, i2) {
                return crate::error::failure(format!(
                    "Cannot simplify to rebase-with-history because merge {i1}-{i2} is not yet done"
                ));
            }
        }
        let mut commit = self.grid.get(i1, 0).sha1.clone().unwrap();
        for i2 in 1..self.grid.len2 {
            let orig = self.grid.get(0, i2).sha1.clone().unwrap();
            let tree = git.get_tree(&self.grid.get(i1, i2).sha1.clone().unwrap())?;
            let msg = format!(
                "{}\n\n(rebased-with-history from commit {orig})\n",
                git.get_log_message(&orig)?.trim_end_matches('\n')
            );
            let author = git.get_author_info(&orig)?;
            commit = git.commit_tree(&tree, &[commit, orig], &msg, Some(&author))?;
        }
        self.set_refname(git, refname, &commit, force)
    }

    fn simplify_to_border(
        &self,
        git: &Git,
        refname: &str,
        with_history1: bool,
        with_history2: bool,
        force: bool,
    ) -> Result<()> {
        let i1last = self.grid.len1 - 1;
        for i2 in 1..self.grid.len2 {
            if !self.grid.is_known(i1last, i2) {
                return crate::error::failure(format!(
                    "Cannot simplify to border because merge {i1last}-{i2} is not yet done"
                ));
            }
        }
        let i2last = self.grid.len2 - 1;
        for i1 in 1..self.grid.len1 {
            if !self.grid.is_known(i1, i2last) {
                return crate::error::failure(format!(
                    "Cannot simplify to border because merge {i1}-{i2last} is not yet done"
                ));
            }
        }

        let i1 = i1last;
        let mut commit = self.grid.get(i1, 0).sha1.clone().unwrap();
        for i2 in 1..self.grid.len2 - 1 {
            let orig = self.grid.get(0, i2).sha1.clone().unwrap();
            let tree = git.get_tree(&self.grid.get(i1, i2).sha1.clone().unwrap())?;
            let logmsg = git.get_log_message(&orig)?;
            let (parents, msg) = if with_history2 {
                (
                    vec![commit.clone(), orig.clone()],
                    format!("{}\n\n(rebased-with-history from commit {orig})\n", logmsg.trim_end_matches('\n')),
                )
            } else {
                (
                    vec![commit.clone()],
                    format!("{}\n\n(rebased from commit {orig})\n", logmsg.trim_end_matches('\n')),
                )
            };
            let author = git.get_author_info(&orig)?;
            commit = git.commit_tree(&tree, &parents, &msg, Some(&author))?;
        }
        let commit1 = commit;

        let i2 = i2last;
        let mut commit = self.grid.get(0, i2).sha1.clone().unwrap();
        for i1 in 1..self.grid.len1 - 1 {
            let orig = self.grid.get(i1, 0).sha1.clone().unwrap();
            let tree = git.get_tree(&self.grid.get(i1, i2).sha1.clone().unwrap())?;
            let logmsg = git.get_log_message(&orig)?;
            let (parents, msg) = if with_history1 {
                (
                    vec![orig.clone(), commit.clone()],
                    format!("{}\n\n(rebased-with-history from commit {orig})\n", logmsg.trim_end_matches('\n')),
                )
            } else {
                (
                    vec![commit.clone()],
                    format!("{}\n\n(rebased from commit {orig})\n", logmsg.trim_end_matches('\n')),
                )
            };
            let author = git.get_author_info(&orig)?;
            commit = git.commit_tree(&tree, &parents, &msg, Some(&author))?;
        }
        let commit2 = commit;

        let tree = git.get_tree(&self.grid.get(self.grid.len1 - 1, self.grid.len2 - 1).sha1.clone().unwrap())?;
        let msg = format!("Merge {} into {} (using imerge border)", self.tip2, self.tip1);
        let apex = git.commit_tree(&tree, &[commit1, commit2], &msg, None)?;
        self.set_refname(git, refname, &apex, force)
    }

    /// Generic engine behind `rebase`/`drop`/`revert`: build a commit chain
    /// (reusing existing objects where possible) from `base` through
    /// `path`, requiring the *pre-simplification* apex to still be a
    /// fast-forward of `refname` unless `force` is set.
    fn simplify_to_path(&self, git: &Git, refname: &str, base: &PathRef, path: &[(PathRef, PathRef)], force: bool) -> Result<()> {
        let base_sha1 = base.to_sha1(&self.grid)?;
        let mut path_sha1 = Vec::new();
        for (commit, metadata) in path {
            path_sha1.push((commit.to_sha1(&self.grid)?, metadata.to_sha1(&self.grid)?));
        }

        let apex = match path_sha1.last() {
            Some((c, _)) => c.clone(),
            None => base_sha1.clone(),
        };

        if !force && !git.is_ff(refname, &apex)? {
            return crate::error::failure(format!(
                "{refname} cannot be updated to {apex} without discarding history.\n\
                 Use --force if you are sure, or choose a different reference"
            ));
        }

        let chain = git.create_commit_chain(Some(&base_sha1), &path_sha1)?;
        self.set_refname(git, refname, &chain, true)
    }

    fn simplify_to_rebase(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        let i1 = self.grid.len1 - 1;
        let path: Vec<(PathRef, PathRef)> = (1..self.grid.len2).map(|i2| (PathRef::Cell(i1, i2), PathRef::Cell(0, i2))).collect();
        self.simplify_to_path(git, refname, &PathRef::Cell(i1, 0), &path, force)
            .map_err(|e| match e {
                ImergeError::MissingMerge { i1, i2 } => {
                    ImergeError::Failure(format!("Cannot simplify to {} because merge {i1}-{i2} is not yet done", self.goal))
                }
                other => other,
            })
    }

    fn simplify_to_drop(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        let base = match &self.goalopts {
            Some(g) if g.base.is_some() => g.base.clone().unwrap(),
            _ => return crate::error::failure("Goal \"drop\" was not initialized correctly"),
        };
        let i2 = self.grid.len2 - 1;
        let path: Vec<(PathRef, PathRef)> = (1..self.grid.len1).map(|i1| (PathRef::Cell(i1, i2), PathRef::Cell(i1, 0))).collect();
        self.simplify_to_path(git, refname, &PathRef::Sha1(base), &path, force)
            .map_err(|e| match e {
                ImergeError::MissingMerge { i1, i2 } => {
                    ImergeError::Failure(format!("Cannot simplify to rebase because merge {i1}-{i2} is not yet done"))
                }
                other => other,
            })
    }

    fn simplify_to_revert(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        self.simplify_to_rebase(git, refname, force)
    }

    fn simplify_to_merge(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        if !self.grid.is_known(self.grid.len1 - 1, self.grid.len2 - 1) {
            return crate::error::failure(format!(
                "Cannot simplify to merge because merge {}-{} is not yet done",
                self.grid.len1 - 1,
                self.grid.len2 - 1
            ));
        }
        let tree = git.get_tree(&self.grid.get(self.grid.len1 - 1, self.grid.len2 - 1).sha1.clone().unwrap())?;
        let parents = vec![
            self.grid.get(self.grid.len1 - 1, 0).sha1.clone().unwrap(),
            self.grid.get(0, self.grid.len2 - 1).sha1.clone().unwrap(),
        ];
        let msg = format!("Merge {} into {} (using imerge)", self.tip2, self.tip1);
        let sha1 = git.commit_tree(&tree, &parents, &msg, None)?;
        self.set_refname(git, refname, &sha1, force)?;
        git.amend()
    }

    pub fn simplify(&self, git: &Git, refname: &str, force: bool) -> Result<()> {
        match self.goal.as_str() {
            "full" => self.simplify_to_full(git, refname, force),
            "rebase" => self.simplify_to_rebase(git, refname, force),
            "rebase-with-history" => self.simplify_to_rebase_with_history(git, refname, force),
            "border" => self.simplify_to_border(git, refname, false, false, force),
            "border-with-history" => self.simplify_to_border(git, refname, false, true, force),
            "border-with-history2" => self.simplify_to_border(git, refname, true, true, force),
            "drop" => self.simplify_to_drop(git, refname, force),
            "revert" => self.simplify_to_revert(git, refname, force),
            "merge" => self.simplify_to_merge(git, refname, force),
            other => crate::error::failure(format!("Invalid value for goal ({other:?})")),
        }
    }

    pub fn save(&mut self, git: &Git) -> Result<()> {
        let name = self.grid.name.clone();
        let mut blockers = Vec::new();
        for i2 in 0..self.grid.len2 {
            for i1 in 0..self.grid.len1 {
                if self.grid.is_known(i1, i2) {
                    self.grid.get_mut(i1, i2).save(git, &name, i1, i2)?;
                }
                if self.grid.is_blocked(i1, i2) {
                    blockers.push((i1, i2));
                }
            }
        }
        let state = StateDict {
            version: format!("{}.{}.{}", STATE_VERSION.0, STATE_VERSION.1, STATE_VERSION.2),
            blockers,
            tip1: Some(self.tip1.clone()),
            tip2: Some(self.tip2.clone()),
            goal: self.goal.clone(),
            goalopts: self.goalopts.clone(),
            manual: self.manual,
            dedupe_patches: self.grid.dedupe_patches,
            branch: Some(self.branch.clone()),
        };
        git.write_imerge_state_dict(&name, &state)
    }
}
