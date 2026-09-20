//! Thin wrapper around the `git` command-line binary.
//!
//! This mirrors the Python original's `GitRepository` class: git-imerge's
//! whole value proposition is running many small *real* merges with *real*
//! conflict detection, so we shell out to the actual `git` binary rather
//! than reimplementing merge/commit-tree/etc. with a library.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::error::{ImergeError, Result};

pub const BRANCH_PREFIX: &str = "refs/heads/";

/// The well-known empty-tree object, present in every git repository. Used
/// as the "parent" when diffing a root commit for `patch_id`.
pub const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Author/committer metadata to inject into a `git commit-tree` subprocess's
/// environment (`GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_AUTHOR_DATE`).
pub type CommitMetadata = HashMap<String, String>;

pub enum AutomergeOutcome {
    Success(String),
    Conflict,
}

pub struct Git {
    /// Memoized `git patch-id --stable` results, keyed by commit SHA-1.
    /// `None` means "not eligible" (a merge commit). Patch-id is a
    /// per-commit property independent of which imerge is asking, and the
    /// same commit is checked against every cell in its row/column, so
    /// caching here (rather than per-`Grid`) avoids redundant subprocess
    /// calls across the whole run.
    patch_id_cache: RefCell<HashMap<String, Option<String>>>,
}

impl Git {
    pub fn new() -> Self {
        Git { patch_id_cache: RefCell::new(HashMap::new()) }
    }

    // -- low-level process helpers -----------------------------------

    fn output(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("git")
            .args(args)
            .output()
            .map_err(|e| ImergeError::Failure(format!("could not run 'git {}': {e}", args.join(" "))))
    }

    /// Run git, returning trimmed stdout. Fails with a `Failure` (including
    /// stderr) if git exits non-zero. Equivalent to Python's
    /// `check_output`.
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.output(args)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(ImergeError::Failure(format!(
                "'git {}' failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    }

    /// Run git, returning true iff it exits zero. Never fails except on
    /// inability to spawn git at all.
    fn success(&self, args: &[&str]) -> Result<bool> {
        let out = self.output(args)?;
        Ok(out.status.success())
    }

    /// Run git with the given stdin content, returning trimmed stdout.
    fn run_stdin(&self, args: &[&str], input: &str) -> Result<String> {
        self.run_stdin_env(args, input, None)
    }

    fn run_stdin_env(
        &self,
        args: &[&str],
        input: &str,
        env: Option<&CommitMetadata>,
    ) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = env {
            cmd.envs(env);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ImergeError::Failure(format!("could not run 'git {}': {e}", args.join(" "))))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .map_err(|e| ImergeError::Failure(e.to_string()))?;
        let out = child
            .wait_with_output()
            .map_err(|e| ImergeError::Failure(e.to_string()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(ImergeError::Failure(format!(
                "'git {}' failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    }

    /// Run git with inherited stdio (for interactive/loud commands like
    /// `merge --no-commit`, `commit --amend`, `log --no-walk`).
    fn run_loud(&self, args: &[&str]) -> Result<bool> {
        let status = Command::new("git")
            .args(args)
            .status()
            .map_err(|e| ImergeError::Failure(format!("could not run 'git {}': {e}", args.join(" "))))?;
        Ok(status.success())
    }

    fn run_loud_checked(&self, args: &[&str]) -> Result<()> {
        if self.run_loud(args)? {
            Ok(())
        } else {
            Err(ImergeError::Failure(format!("'git {}' failed", args.join(" "))))
        }
    }

    // -- repository-level helpers --------------------------------------

    pub fn git_dir(&self) -> Result<String> {
        self.run(&["rev-parse", "--git-dir"])
    }

    pub fn check_imerge_name_format(&self, name: &str) -> Result<()> {
        let refname = format!("refs/imerge/{name}");
        if self.success(&["check-ref-format", &refname])? {
            Ok(())
        } else {
            crate::error::failure(format!("Name '{name}' is not a valid refname component!"))
        }
    }

    pub fn check_branch_name_format(&self, name: &str) -> Result<()> {
        let refname = format!("refs/heads/{name}");
        if self.success(&["check-ref-format", &refname])? {
            Ok(())
        } else {
            crate::error::failure(format!("Name '{name}' is not a valid branch name!"))
        }
    }

    /// Parsed `sha1 type refname` triples from `git for-each-ref`.
    fn for_each_ref(&self, pattern: &str) -> Result<Vec<(String, String, String)>> {
        let out = self.run(&[
            "for-each-ref",
            "--format=%(objectname) %(objecttype) %(refname)",
            pattern,
        ])?;
        let mut v = Vec::new();
        for line in out.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(3, ' ');
            let sha1 = parts.next().unwrap_or_default().to_string();
            let typ = parts.next().unwrap_or_default().to_string();
            let refname = parts.next().unwrap_or_default().to_string();
            v.push((sha1, typ, refname));
        }
        Ok(v)
    }

    pub fn iter_existing_imerge_names(&self) -> Result<Vec<String>> {
        let re = regex::Regex::new(r"^refs/imerge/(?P<name>.+)/state$").unwrap();
        let mut names = Vec::new();
        for (_, typ, refname) in self.for_each_ref("refs/imerge")? {
            if typ == "blob" {
                if let Some(caps) = re.captures(&refname) {
                    names.push(caps["name"].to_string());
                }
            }
        }
        Ok(names)
    }

    pub fn set_default_imerge_name(&self, name: Option<&str>) -> Result<()> {
        match name {
            None => {
                // Ignore failure: "value was not set" is not an error.
                let _ = self.output(&["config", "--unset", "imerge.default"])?;
                Ok(())
            }
            Some(name) => {
                self.output(&["config", "imerge.default", name])?;
                Ok(())
            }
        }
    }

    pub fn get_default_imerge_name(&self) -> Result<Option<String>> {
        let out = self.output(&["config", "imerge.default"])?;
        if out.status.success() {
            Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
        } else {
            Ok(None)
        }
    }

    pub fn get_default_edit(&self) -> bool {
        match self.output(&["config", "--bool", "imerge.editmergemessages"]) {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim() == "true"
            }
            _ => false,
        }
    }

    pub fn unstaged_changes(&self) -> Result<bool> {
        Ok(!self.success(&["diff-files", "--quiet", "--ignore-submodules"])?)
    }

    pub fn uncommitted_changes(&self) -> Result<bool> {
        Ok(!self.success(&[
            "diff-index",
            "--cached",
            "--quiet",
            "--ignore-submodules",
            "HEAD",
            "--",
        ])?)
    }

    /// Convert `arg` into a SHA-1, verifying that it refers to a commit.
    pub fn get_commit_sha1(&self, arg: &str) -> Result<String> {
        let target = format!("{arg}^{{commit}}");
        match self.rev_parse(&target)? {
            Some(sha) => Ok(sha),
            None => crate::error::failure(format!("{arg:?} does not refer to a valid git commit")),
        }
    }

    pub fn refresh_index(&self) -> Result<()> {
        let out = self.output(&["update-index", "-q", "--ignore-submodules", "--refresh"])?;
        if out.status.success() {
            Ok(())
        } else {
            let msg = {
                let err = String::from_utf8_lossy(&out.stderr);
                let sout = String::from_utf8_lossy(&out.stdout);
                if !err.trim().is_empty() {
                    err.trim().to_string()
                } else {
                    sout.trim().to_string()
                }
            };
            Err(ImergeError::UncleanWorkTree(msg))
        }
    }

    pub fn verify_imerge_name_available(&self, name: &str) -> Result<()> {
        self.check_imerge_name_format(name)?;
        let refs = self.for_each_ref(&format!("refs/imerge/{name}"))?;
        if !refs.is_empty() {
            return crate::error::failure(format!("Name '{name}' is already in use!"));
        }
        Ok(())
    }

    /// Verify a MergeState with the given name exists (readable, compatible
    /// version). Returns false if the state ref doesn't exist at all;
    /// propagates errors for any other problem.
    pub fn check_imerge_exists(&self, name: &str) -> Result<bool> {
        self.check_imerge_name_format(name)?;
        let state_refname = format!("refs/imerge/{name}/state");
        for (_, typ, refname) in self.for_each_ref(&state_refname)? {
            if refname == state_refname && typ == "blob" {
                self.read_imerge_state_dict(name)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn read_imerge_state_dict(&self, name: &str) -> Result<crate::state::StateDict> {
        let blob = self.run(&["cat-file", "blob", &format!("refs/imerge/{name}/state")])?;
        let state: crate::state::StateDict = serde_json::from_str(&blob)
            .map_err(|e| ImergeError::Failure(format!("could not parse imerge state: {e}")))?;

        let parts: Vec<&str> = state.version.split('.').collect();
        if parts.len() < 2 {
            return crate::error::failure(format!(
                "The format of imerge {name} ({}) is not compatible with this script version.",
                state.version
            ));
        }
        let major: u32 = parts[0].parse().unwrap_or(0);
        let minor: u32 = parts[1].parse().unwrap_or(0);
        if major != crate::state::STATE_VERSION.0 || minor > crate::state::STATE_VERSION.1 {
            return crate::error::failure(format!(
                "The format of imerge {name} ({}) is not compatible with this script version.",
                state.version
            ));
        }
        Ok(state)
    }

    /// Read all refs under `refs/imerge/<name>`, returning
    /// `(state_dict, {(i1, i2): (sha1, source)})` where source is
    /// `"auto"` or `"manual"`.
    #[allow(clippy::type_complexity)]
    pub fn read_imerge_state(
        &self,
        name: &str,
    ) -> Result<(crate::state::StateDict, HashMap<(usize, usize), (String, String)>)> {
        let merge_re = regex::Regex::new(&format!(
            r"^refs/imerge/{}/(?P<source>auto|manual)/(?P<i1>0|[1-9][0-9]*)-(?P<i2>0|[1-9][0-9]*)$",
            regex::escape(name)
        ))
        .unwrap();
        let state_re = regex::Regex::new(&format!(r"^refs/imerge/{}/state$", regex::escape(name))).unwrap();

        let mut state: Option<crate::state::StateDict> = None;
        let mut merges: HashMap<(usize, usize), (String, String)> = HashMap::new();
        let mut unexpected = Vec::new();

        for (sha1, typ, refname) in self.for_each_ref(&format!("refs/imerge/{name}"))? {
            if let Some(caps) = merge_re.captures(&refname) {
                if typ != "commit" {
                    return crate::error::failure(format!("Reference {refname:?} is not a commit!"));
                }
                let i1: usize = caps["i1"].parse().unwrap();
                let i2: usize = caps["i2"].parse().unwrap();
                merges.insert((i1, i2), (sha1, caps["source"].to_string()));
                continue;
            }
            if state_re.is_match(&refname) {
                if typ != "blob" {
                    return crate::error::failure(format!("Reference {refname:?} is not a blob!"));
                }
                state = Some(self.read_imerge_state_dict(name)?);
                continue;
            }
            unexpected.push(refname);
        }

        let state = state.ok_or_else(|| {
            ImergeError::Failure(format!(
                "No state found; it should have been a blob reference at \"refs/imerge/{name}/state\""
            ))
        })?;

        if !unexpected.is_empty() {
            return crate::error::failure(format!(
                "Unexpected reference(s) found in \"refs/imerge/{name}\" namespace:\n    {}\n",
                unexpected.join("\n    ")
            ));
        }

        Ok((state, merges))
    }

    pub fn write_imerge_state_dict(&self, name: &str, state: &crate::state::StateDict) -> Result<()> {
        let mut json =
            serde_json::to_string(state).map_err(|e| ImergeError::Failure(e.to_string()))?;
        json.push('\n');
        let sha1 = self.run_stdin(&["hash-object", "-t", "blob", "-w", "--stdin"], &json)?;
        self.run(&[
            "update-ref",
            "-m",
            &format!("imerge '{name}': Record state"),
            &format!("refs/imerge/{name}/state"),
            &sha1,
        ])?;
        Ok(())
    }

    pub fn is_ancestor(&self, commit1: &str, commit2: &str) -> Result<bool> {
        if commit1 == commit2 {
            return Ok(true);
        }
        let out = self.run(&[
            "rev-list",
            "--count",
            "--ancestry-path",
            &format!("{commit1}..{commit2}"),
        ])?;
        Ok(out.trim().parse::<u64>().unwrap_or(0) != 0)
    }

    pub fn is_ff(&self, refname: &str, commit: &str) -> Result<bool> {
        match self.get_commit_sha1(refname) {
            Ok(old) => self.is_ancestor(&old, commit),
            Err(_) => Ok(true),
        }
    }

    /// Attempt an automatic merge of `commit1` and `commit2`. Must be
    /// called with a clean worktree. Disables `rerere` (see the upstream
    /// comment: git-imerge does so many speculative merges that rerere
    /// would either get confused or pollute its cache).
    pub fn automerge(&self, commit1: &str, commit2: &str, msg: Option<&str>) -> Result<AutomergeOutcome> {
        self.automerge_with_strategy(commit1, commit2, msg, &[], &[])
    }

    /// Like [`automerge`](Self::automerge), but resolves any conflicting
    /// hunks in favor of `commit1` (`-X ours`). Only ever called after a
    /// plain `automerge` has already conflicted *and* `patch_id` has
    /// confirmed the two branch commits responsible for this grid cell are
    /// patch-id-identical, single-parent commits -- in that case either
    /// side's resolution of the conflicting hunk is the same text, so
    /// preferring one deterministically is safe.
    ///
    /// Deliberately bypasses `.gitattributes`-configured custom merge
    /// drivers (`merge=<name>`) for this one call: `-X <strategy-option>`
    /// is specific to git's own recursive/ort merge strategy and is
    /// silently ignored by a custom driver, which takes over content
    /// resolution *before* strategy options would apply and has no way to
    /// know "prefer ours" is even meant for it. Since we've already
    /// established via `patch_id` that this conflict is a false positive
    /// (the same patch on both sides), we don't need -- or want -- any
    /// driver's semantic understanding here; falling back to git's native,
    /// `-X`-aware engine for just this narrow, pre-verified-safe retry is
    /// the correct choice, not a workaround.
    pub fn automerge_prefer_ours(&self, commit1: &str, commit2: &str, msg: Option<&str>) -> Result<AutomergeOutcome> {
        let attrs_path = Self::empty_attributes_file()?;
        let attrs_opt = format!("core.attributesFile={}", attrs_path.display());
        self.automerge_with_strategy(commit1, commit2, msg, &["-c", &attrs_opt], &["-X", "ours"])
    }

    /// Path to a real, guaranteed-empty attributes file (a genuine file
    /// rather than `/dev/null`/`NUL`, for portability), used to disable
    /// `.gitattributes` processing for one `git` invocation via `-c
    /// core.attributesFile=<path>`.
    fn empty_attributes_file() -> Result<std::path::PathBuf> {
        let path = std::env::temp_dir().join(format!("git-imerge-empty-attributes-{}", std::process::id()));
        std::fs::write(&path, "")?;
        Ok(path)
    }

    fn automerge_with_strategy(
        &self,
        commit1: &str,
        commit2: &str,
        msg: Option<&str>,
        global_opts: &[&str],
        extra: &[&str],
    ) -> Result<AutomergeOutcome> {
        // Silent: matches upstream's `call_silently`, avoiding a flood of
        // "leaving N commits behind" detached-HEAD advice on every
        // speculative merge.
        let checkout_out = self.output(&["checkout", "-f", commit1])?;
        if !checkout_out.status.success() {
            return Err(ImergeError::Failure(format!(
                "'git checkout -f {commit1}' failed: {}",
                String::from_utf8_lossy(&checkout_out.stderr).trim()
            )));
        }
        let mut args = vec!["-c", "rerere.enabled=false"];
        args.extend_from_slice(global_opts);
        args.push("merge");
        args.extend_from_slice(extra);
        if let Some(msg) = msg {
            args.push("-m");
            args.push(msg);
        }
        args.push(commit2);
        // Run silently: this may fail (a conflict), which is expected.
        let out = self.output(&args)?;
        if out.status.success() {
            Ok(AutomergeOutcome::Success(self.get_commit_sha1("HEAD")?))
        } else {
            self.abort_merge()?;
            Ok(AutomergeOutcome::Conflict)
        }
    }

    /// Initiate a (likely-conflicting) merge of `commit` into HEAD, leaving
    /// conflict markers/staged changes in the working tree for the user.
    pub fn manualmerge(&self, commit: &str, msg: &str) -> Result<()> {
        // A conflict here is expected and not an error.
        let _ = self.run_loud(&["merge", "--no-commit", "-m", msg, commit])?;
        Ok(())
    }

    pub fn require_clean_work_tree(&self, action: &str) -> Result<()> {
        let out = self.output(&["rev-parse", "--verify", "HEAD"])?;
        if !out.status.success() {
            return Err(ImergeError::UncleanWorkTree(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        self.refresh_index()?;

        let mut errors = Vec::new();
        if self.unstaged_changes()? {
            errors.push(format!("Cannot {action}: You have unstaged changes."));
        }
        if self.uncommitted_changes()? {
            if errors.is_empty() {
                errors.push(format!("Cannot {action}: Your index contains uncommitted changes."));
            } else {
                errors.push("Additionally, your index contains uncommitted changes.".to_string());
            }
        }
        if !errors.is_empty() {
            return Err(ImergeError::UncleanWorkTree(errors.join("\n")));
        }
        Ok(())
    }

    pub fn simple_merge_in_progress(&self) -> Result<bool> {
        let path = std::path::Path::new(&self.git_dir()?).join("MERGE_HEAD");
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(contents.lines().count() == 1),
            Err(_) => Ok(false),
        }
    }

    /// If a simple merge is in progress and ready, commit it. Return
    /// whether it did so.
    pub fn commit_user_merge(&self, edit_log_msg: Option<bool>) -> Result<bool> {
        if !self.simple_merge_in_progress()? {
            return Ok(false);
        }
        self.refresh_index()?;
        if self.unstaged_changes()? {
            return Err(ImergeError::UncleanWorkTree(
                "Cannot proceed: You have unstaged changes.".to_string(),
            ));
        }
        let edit = edit_log_msg.unwrap_or_else(|| self.get_default_edit());
        let args: &[&str] = if edit {
            &["commit", "--no-verify", "--edit"]
        } else {
            &["commit", "--no-verify", "--no-edit"]
        };
        if self.run_loud(args)? {
            Ok(true)
        } else {
            crate::error::failure("Could not commit staged changes.")
        }
    }

    /// Build a linear chain of commits, reusing existing commit objects
    /// where the tree/metadata/parent chain already matches.
    pub fn create_commit_chain(
        &self,
        base: Option<&str>,
        path: &[(String, String)],
    ) -> Result<String> {
        let mut reusing = true;
        let mut parents: Vec<String> = match base {
            None => {
                if path.is_empty() {
                    return crate::error::failure("neither base nor path specified");
                }
                Vec::new()
            }
            Some(base) => vec![base.to_string()],
        };

        for (commit, metadata) in path {
            if reusing {
                if commit == metadata && self.get_commit_parents(commit)? == parents {
                    parents = vec![commit.clone()];
                    continue;
                } else {
                    reusing = false;
                }
            }
            let tree = self.get_tree(commit)?;
            let msg = self.get_log_message(metadata)?;
            let author = self.get_author_info(metadata)?;
            let new_commit = self.commit_tree(&tree, &parents, &msg, Some(&author))?;
            parents = vec![new_commit];
        }

        if parents.len() != 1 {
            return crate::error::failure("create_commit_chain: expected exactly one resulting commit");
        }
        Ok(parents.into_iter().next().unwrap())
    }

    /// `git rev-parse --verify --quiet <arg>`, returning `None` if it
    /// doesn't resolve rather than erroring.
    pub fn rev_parse(&self, arg: &str) -> Result<Option<String>> {
        let out = self.output(&["rev-parse", "--verify", "--quiet", arg])?;
        if out.status.success() {
            Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
        } else {
            Ok(None)
        }
    }

    /// Like `rev_parse`, but errors out (as a `Failure`) if the ref doesn't
    /// resolve; used at CLI-argument boundaries where we want a plain
    /// SHA-1 immediately (mirrors `GitRepository.rev_parse`, which raises
    /// via `check_output`).
    pub fn rev_parse_required(&self, arg: &str) -> Result<String> {
        match self.rev_parse(arg)? {
            Some(sha) => Ok(sha),
            None => crate::error::failure(format!("{arg:?} is not a valid revision")),
        }
    }

    pub fn rev_list_with_parents(&self, args: &[&str]) -> Result<Vec<(String, Vec<String>)>> {
        let mut cmd_args = vec!["log", "--format=%H %P"];
        cmd_args.extend_from_slice(args);
        let out = self.run(&cmd_args)?;
        let mut v = Vec::new();
        for line in out.lines() {
            let mut parts = line.split_whitespace();
            let commit = match parts.next() {
                Some(c) => c.to_string(),
                None => continue,
            };
            let parents: Vec<String> = parts.map(|s| s.to_string()).collect();
            v.push((commit, parents));
        }
        Ok(v)
    }

    /// Print a one-line-ish summary of `commit` to stdout (inherited).
    pub fn summarize_commit(&self, commit: &str) -> Result<()> {
        self.run_loud_checked(&["--no-pager", "log", "--no-walk", commit])
    }

    pub fn get_author_info(&self, commit: &str) -> Result<CommitMetadata> {
        let out = self.run(&["--no-pager", "log", "-n1", "--format=%an%n%ae%n%ai", commit])?;
        let lines: Vec<&str> = out.lines().collect();
        if lines.len() < 3 {
            return crate::error::failure(format!("could not read author info for {commit}"));
        }
        let mut m = CommitMetadata::new();
        m.insert("GIT_AUTHOR_NAME".to_string(), lines[0].to_string());
        m.insert("GIT_AUTHOR_EMAIL".to_string(), lines[1].to_string());
        m.insert("GIT_AUTHOR_DATE".to_string(), lines[2].to_string());
        Ok(m)
    }

    pub fn get_log_message(&self, commit: &str) -> Result<String> {
        let out = self.run(&["cat-file", "commit", commit])?;
        // Header ends at the first blank line.
        let body = match out.split_once("\n\n") {
            Some((_, rest)) => rest.to_string(),
            None => String::new(),
        };
        let mut body = body;
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        Ok(body)
    }

    pub fn get_commit_parents(&self, commit: &str) -> Result<Vec<String>> {
        let out = self.run(&["--no-pager", "log", "--no-walk", "--pretty=format:%P", commit])?;
        Ok(out.split_whitespace().map(|s| s.to_string()).collect())
    }

    /// `git patch-id --stable` of the diff introduced by a single ordinary
    /// (0- or 1-parent) commit, used to recognize when the same underlying
    /// change was made independently on two branches (e.g. a cherry-picked
    /// hotfix). Returns `Ok(None)` -- never an error -- for a merge commit
    /// (out of scope for this heuristic); memoized per commit SHA-1, since
    /// the same commit is checked against every cell in its row/column.
    pub fn patch_id(&self, commit: &str) -> Result<Option<String>> {
        if let Some(cached) = self.patch_id_cache.borrow().get(commit) {
            return Ok(cached.clone());
        }
        let parents = self.get_commit_parents(commit)?;
        let result = if parents.len() > 1 {
            None
        } else {
            let base = parents.first().map(String::as_str).unwrap_or(EMPTY_TREE_SHA1);
            // Deliberately not `self.run(...)`: that helper trims *all*
            // trailing newlines, which would strip the newline patch-id
            // needs to recognize the diff's final line.
            let diff_out = self.output(&["diff", base, commit])?;
            if !diff_out.status.success() {
                return Err(ImergeError::Failure(format!(
                    "'git diff {base} {commit}' failed: {}",
                    String::from_utf8_lossy(&diff_out.stderr).trim()
                )));
            }
            let diff = String::from_utf8_lossy(&diff_out.stdout).to_string();
            let out = self.run_stdin(&["patch-id", "--stable"], &diff)?;
            out.split_whitespace().next().map(str::to_string)
        };
        self.patch_id_cache.borrow_mut().insert(commit.to_string(), result.clone());
        Ok(result)
    }

    pub fn get_tree(&self, arg: &str) -> Result<String> {
        self.rev_parse_required(&format!("{arg}^{{tree}}"))
    }

    pub fn update_ref(&self, refname: &str, value: &str, msg: &str, deref: bool) -> Result<()> {
        let mut args = vec!["update-ref"];
        if !deref {
            args.push("--no-deref");
        }
        args.extend_from_slice(&["-m", msg, refname, value]);
        self.run(&args)?;
        Ok(())
    }

    pub fn delete_ref(&self, refname: &str, msg: &str) -> Result<()> {
        self.run(&["update-ref", "-m", msg, "-d", refname])?;
        Ok(())
    }

    pub fn delete_imerge_refs(&self, name: &str) -> Result<()> {
        let refs = self.run(&[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/imerge/{name}"),
        ])?;
        let stdin: String = refs.lines().map(|r| format!("delete {r}\n")).collect();
        if stdin.is_empty() {
            return Ok(());
        }
        if let Err(e) = self.run_stdin(
            &["update-ref", "-m", &format!("imerge: remove merge '{name}'"), "--stdin"],
            &stdin,
        ) {
            eprintln!("Warning: error removing references:\n{e}");
        }
        Ok(())
    }

    pub fn detach(&self, msg: &str) -> Result<()> {
        self.update_ref("HEAD", "HEAD^0", msg, false)
    }

    pub fn reset_hard(&self, commit: Option<&str>) -> Result<()> {
        match commit {
            Some(c) => self.run_loud_checked(&["reset", "--hard", c]),
            None => self.run_loud_checked(&["reset", "--hard"]),
        }
    }

    pub fn amend(&self) -> Result<()> {
        self.run_loud_checked(&["commit", "--amend"])
    }

    pub fn abort_merge(&self) -> Result<()> {
        // Not "git merge --abort": that flag postdates git 1.7.4.
        let _ = self.output(&["reset", "--merge"])?;
        Ok(())
    }

    pub fn compute_best_merge_base(&self, tip1: &str, tip2: &str) -> Result<String> {
        let out = self.output(&["merge-base", "--all", tip1, tip2])?;
        if !out.status.success() {
            return crate::error::failure(format!("Cannot compute merge base for {tip1:?} and {tip2:?}"));
        }
        let bases: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect();
        if bases.is_empty() {
            return crate::error::failure(format!("{tip1:?} and {tip2:?} do not have a common merge base"));
        }
        if bases.len() == 1 {
            return Ok(bases.into_iter().next().unwrap());
        }
        let mut best: Option<(String, u64)> = None;
        for base in bases {
            let count: u64 = self
                .run(&["rev-list", "--no-merges", "--count", &format!("{base}..{tip1}")])?
                .trim()
                .parse()
                .unwrap_or(0);
            if best.as_ref().map(|(_, c)| count < *c).unwrap_or(true) {
                best = Some((base, count));
            }
        }
        Ok(best.unwrap().0)
    }

    /// Compute a linear ancestry between `commit1` (exclusive) and
    /// `commit2` (inclusive), in chronological order.
    pub fn linear_ancestry(&self, commit1: &str, commit2: &str, first_parent: bool) -> Result<Vec<String>> {
        let oid1 = self.rev_parse_required(commit1)?;
        let oid2 = self.rev_parse_required(commit2)?;

        let mut parentage: HashMap<String, Vec<String>> = HashMap::new();
        parentage.insert(oid1.clone(), Vec::new());
        for (commit, parents) in self.rev_list_with_parents(&[
            "--ancestry-path",
            "--topo-order",
            &format!("{oid1}..{oid2}"),
        ])? {
            parentage.insert(commit, parents);
        }

        let mut commits = Vec::new();
        let mut commit = oid2.clone();
        while commit != oid1 {
            let parents = parentage.get(&commit).cloned().unwrap_or_default();
            let included: Vec<String> = parents.into_iter().filter(|p| parentage.contains_key(p)).collect();
            let next = if included.is_empty() {
                return Err(ImergeError::NotFirstParentAncestor(format!(
                    "{commit1} is not an ancestor of {commit2}"
                )));
            } else if included.len() == 1 || first_parent {
                included[0].clone()
            } else {
                return Err(ImergeError::NonlinearAncestry(format!(
                    "{commit1}..{commit2} has non-linear ancestry (it contains a merge); \
                     git-imerge needs a single linear chain of commits"
                )));
            };
            commits.push(commit);
            commit = next;
        }
        commits.reverse();
        Ok(commits)
    }

    /// Return `(merge_base, commits1, commits2)` for an incremental merge
    /// between `tip1` and `tip2`.
    pub fn get_boundaries(
        &self,
        tip1: &str,
        tip2: &str,
        first_parent: bool,
    ) -> Result<(String, Vec<String>, Vec<String>)> {
        let merge_base = self.compute_best_merge_base(tip1, tip2)?;
        let commits1 = self.linear_ancestry(&merge_base, tip1, first_parent)?;
        if commits1.is_empty() {
            return Err(ImergeError::NothingToDo);
        }
        let commits2 = self.linear_ancestry(&merge_base, tip2, first_parent)?;
        if commits2.is_empty() {
            return Err(ImergeError::NothingToDo);
        }
        Ok((merge_base, commits1, commits2))
    }

    /// Name of the currently checked-out branch, or `None` if detached.
    pub fn get_head_refname(&self, short: bool) -> Option<String> {
        let mut args = vec!["symbolic-ref", "--quiet"];
        if short {
            args.push("--short");
        }
        args.push("HEAD");
        match self.output(&args) {
            Ok(out) if out.status.success() => {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
            _ => None,
        }
    }

    pub fn checkout(&self, refname: &str, quiet: bool) -> Result<()> {
        let mut args = vec!["checkout"];
        if quiet {
            args.push("--quiet");
        }
        let target = if let Some(short) = refname.strip_prefix(BRANCH_PREFIX) {
            short.to_string()
        } else {
            format!("{refname}^0")
        };
        args.push(&target);
        self.run_loud_checked(&args)
    }

    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[String],
        msg: &str,
        metadata: Option<&CommitMetadata>,
    ) -> Result<String> {
        let mut args = vec!["commit-tree".to_string(), tree.to_string()];
        for p in parents {
            args.push("-p".to_string());
            args.push(p.clone());
        }
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_stdin_env(&args_ref, msg, metadata)
    }

    pub fn revert(&self, commit: &str) -> Result<()> {
        let mut args = vec!["revert", "--no-edit"];
        let is_merge = self.get_commit_parents(commit)?.len() > 1;
        if is_merge {
            args.push("-m");
            args.push("1");
        }
        args.push(commit);
        self.run_loud_checked(&args)
    }

    /// Create a new commit object identical to `commit` but with the given
    /// parents (and optionally a replacement message). The result is
    /// written to the object database but not referenced by anything.
    pub fn reparent(&self, commit: &str, parent_sha1s: &[String], msg: Option<&str>) -> Result<String> {
        let raw = self.run(&["cat-file", "commit", commit])?;
        // `run()` strips a single trailing newline; put it back so the
        // header/body split below matches `git cat-file`'s raw output.
        let raw = raw + "\n";
        let sep = raw
            .find("\n\n")
            .ok_or_else(|| ImergeError::Failure(format!("could not parse commit object {commit}")))?;
        let headers = &raw[..sep + 1];
        let rest = &raw[sep + 2..];

        let mut new_commit = String::new();
        for line in headers.lines() {
            if let Some(tree_rest) = line.strip_prefix("tree ") {
                new_commit.push_str("tree ");
                new_commit.push_str(tree_rest);
                new_commit.push('\n');
                for p in parent_sha1s {
                    new_commit.push_str(&format!("parent {p}\n"));
                }
            } else if line.starts_with("parent ") {
                // discard old parents
            } else {
                new_commit.push_str(line);
                new_commit.push('\n');
            }
        }
        new_commit.push('\n');
        match msg {
            None => new_commit.push_str(rest),
            Some(msg) => {
                new_commit.push_str(msg);
                if !msg.ends_with('\n') {
                    new_commit.push('\n');
                }
            }
        }

        self.run_stdin(&["hash-object", "-t", "commit", "-w", "--stdin"], &new_commit)
            .map_err(|_| ImergeError::Failure(format!("Could not reparent commit {commit}")))
    }

    pub fn temporary_head(&self, message: &str) -> Result<TemporaryHead<'_>> {
        TemporaryHead::new(self, message)
    }

    pub fn restore_head(&self, refname: &str, message: &str) -> Result<()> {
        self.run(&["symbolic-ref", "-m", message, "HEAD", refname])?;
        self.reset_hard(None)
    }
}

impl Default for Git {
    fn default() -> Self {
        Self::new()
    }
}

/// A context-manager-equivalent for temporarily recording and restoring
/// HEAD, used by `autofill` (which checks out many intermediate commits
/// while probing merges and must restore the user's original checkout
/// afterward).
pub struct TemporaryHead<'a> {
    git: &'a Git,
    message: String,
    old_refname: Option<String>,
    old_sha1: String,
}

impl<'a> TemporaryHead<'a> {
    fn new(git: &'a Git, message: &str) -> Result<Self> {
        let old_refname = git.get_head_refname(false);
        let old_sha1 = git.get_commit_sha1("HEAD")?;
        Ok(TemporaryHead {
            git,
            message: message.to_string(),
            old_refname,
            old_sha1,
        })
    }

    pub fn restore(&self) -> Result<()> {
        match &self.old_refname {
            Some(refname) => self.git.restore_head(refname, &self.message),
            None => {
                self.git.detach(&self.message)?;
                self.git.reset_hard(Some(&self.old_sha1))
            }
        }
    }
}

impl Drop for TemporaryHead<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
