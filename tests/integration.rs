//! End-to-end tests that exercise the compiled `git-imerge` binary against
//! real, throwaway git repositories. These cover the primary user-facing
//! workflows (conflict-free merge, manual conflict resolution, rebase,
//! drop, list/remove) -- the unit tests in `src/block.rs` and
//! `src/frontier.rs` cover the pure bisection/state-machine logic that
//! doesn't need a real repo.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_git-imerge");

struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = TempDir::new().expect("create temp dir");
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap_or_else(|e| panic!("failed to run {program} {args:?}: {e}"))
    }

    /// Run git, asserting success, and return trimmed stdout.
    fn git(&self, args: &[&str]) -> String {
        let out = self.run("git", args);
        assert!(
            out.status.success(),
            "git {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Run git-imerge, asserting success, and return the raw output (both
    /// stdout and stderr are meaningful -- most user-facing messages go to
    /// stderr).
    fn imerge_ok(&self, args: &[&str]) -> Output {
        let out = self.run(BIN, args);
        assert!(
            out.status.success(),
            "git-imerge {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn imerge(&self, args: &[&str]) -> Output {
        self.run(BIN, args)
    }

    fn write(&self, name: &str, contents: &str) {
        std::fs::write(self.path().join(name), contents).unwrap();
    }

    fn commit_all(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn log_oneline(&self) -> Vec<String> {
        self.git(&["log", "--format=%s", "--all"])
            .lines()
            .map(|s| s.to_string())
            .collect()
    }

    fn parents_of(&self, rev: &str) -> Vec<String> {
        self.git(&["log", "--no-walk", "--format=%P", rev])
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn merge_without_conflicts_completes_and_finishes() {
    let repo = Repo::new();
    repo.write("base.txt", "base\n");
    repo.commit_all("base");

    repo.git(&["checkout", "-q", "-b", "left"]);
    repo.write("left.txt", "left\n");
    repo.commit_all("left1");

    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["checkout", "-q", "-b", "right"]);
    repo.write("right.txt", "right\n");
    repo.commit_all("right1");

    repo.git(&["checkout", "-q", "left"]);
    let out = repo.imerge_ok(&["start", "--name", "m1", "right"]);
    assert!(stderr(&out).contains("Merge is complete!"));

    repo.imerge_ok(&["finish", "--name", "m1"]);

    // The imerge's bookkeeping refs must be fully cleaned up.
    assert_eq!(repo.git(&["for-each-ref", "refs/imerge"]), "");

    // HEAD is now a real merge commit with both original tips as parents.
    let parents = repo.parents_of("HEAD");
    assert_eq!(parents.len(), 2);
    assert!(repo.log_oneline().contains(&"left1".to_string()));
    assert!(repo.log_oneline().contains(&"right1".to_string()));
    assert!(Path::new(repo.path()).join("left.txt").exists());
    assert!(Path::new(repo.path()).join("right.txt").exists());
}

#[test]
fn merge_with_conflict_requires_manual_resolution() {
    let repo = Repo::new();
    repo.write("file.txt", "line1\nline2\nline3\n");
    repo.commit_all("base");

    repo.git(&["checkout", "-q", "-b", "left"]);
    repo.write("file.txt", "line1\nLEFT\nline3\n");
    repo.commit_all("left1");

    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["checkout", "-q", "-b", "right"]);
    repo.write("file.txt", "line1\nRIGHT\nline3\n");
    repo.commit_all("right1");

    repo.git(&["checkout", "-q", "left"]);
    let out = repo.imerge_ok(&["start", "--name", "conf", "right"]);
    let msg = stderr(&out);
    assert!(msg.contains("conflict"), "expected a conflict message, got: {msg}");
    assert!(msg.contains("git-imerge continue"));

    // The working tree must actually contain conflict markers now.
    let file = std::fs::read_to_string(repo.path().join("file.txt")).unwrap();
    assert!(file.contains("<<<<<<<"), "expected conflict markers, got: {file}");

    // A `finish` attempt before resolving must fail cleanly (unclean work
    // tree / incomplete merge), not silently succeed.
    let premature = repo.imerge(&["finish", "--name", "conf"]);
    assert!(!premature.status.success());

    repo.write("file.txt", "line1\nMERGED\nline3\n");
    repo.git(&["add", "file.txt"]);
    let out = repo.imerge_ok(&["continue", "--name", "conf"]);
    assert!(stderr(&out).contains("Merge is complete!"));

    repo.imerge_ok(&["finish", "--name", "conf"]);
    assert_eq!(repo.git(&["for-each-ref", "refs/imerge"]), "");

    let merged = std::fs::read_to_string(repo.path().join("file.txt")).unwrap();
    assert_eq!(merged, "line1\nMERGED\nline3\n");
}

#[test]
fn rebase_produces_linear_history() {
    let repo = Repo::new();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");

    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("a.txt", "base\nf1\n");
    repo.commit_all("f1");
    repo.write("a.txt", "base\nf1\nf2\n");
    repo.commit_all("f2");

    repo.git(&["checkout", "-q", "main"]);
    repo.write("b.txt", "m1\n");
    repo.commit_all("m1");

    repo.git(&["checkout", "-q", "feature"]);
    let out = repo.imerge_ok(&["rebase", "main"]);
    assert!(stderr(&out).contains("Merge is complete!"));
    repo.imerge_ok(&["finish"]);

    // A rebase must produce a single linear chain: every commit has at
    // most one parent.
    let commits = repo.git(&["log", "--format=%H", "feature"]);
    for c in commits.lines() {
        assert!(repo.parents_of(c).len() <= 1, "commit {c} is not a plain linear rebase result");
    }
    let subjects: Vec<String> = repo
        .git(&["log", "--format=%s", "feature"])
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(subjects, vec!["f2", "f1", "m1", "base"]);
}

#[test]
fn drop_removes_a_commit_from_history() {
    let repo = Repo::new();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    repo.write("a.txt", "base\nc1\n");
    repo.commit_all("c1");
    repo.write("a.txt", "base\nc1\nc2\n");
    repo.commit_all("c2");
    repo.write("a.txt", "base\nc1\nc2\nc3\n");
    repo.commit_all("c3");

    let drop_sha = repo.git(&["log", "--format=%H", "--grep=^c2$", "-n", "1"]);
    assert!(!drop_sha.is_empty());

    let out = repo.imerge(&["drop", &drop_sha]);
    // Dropping a middle line-append commit conflicts with the commit after
    // it; resolve it the same way the manual-conflict test does.
    if !out.status.success() && stderr(&out).is_empty() {
        panic!("drop failed unexpectedly: {out:?}");
    }
    let file = std::fs::read_to_string(repo.path().join("a.txt")).unwrap();
    if file.contains("<<<<<<<") {
        std::fs::write(repo.path().join("a.txt"), "base\nc1\nc3\n").unwrap();
        repo.git(&["add", "a.txt"]);
        repo.imerge_ok(&["continue"]);
    }

    repo.imerge_ok(&["finish"]);

    let subjects: Vec<String> = repo
        .git(&["log", "--format=%s", "main"])
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(subjects, vec!["c3", "c1", "base"], "c2 should be gone but c1/c3 kept");
    let final_content = std::fs::read_to_string(repo.path().join("a.txt")).unwrap();
    assert_eq!(final_content, "base\nc1\nc3\n");
}

#[test]
fn list_and_remove_manage_multiple_imerges() {
    let repo = Repo::new();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");

    repo.git(&["checkout", "-q", "-b", "left"]);
    repo.write("left.txt", "left\n");
    repo.commit_all("left1");
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["checkout", "-q", "-b", "right"]);
    repo.write("right.txt", "right\n");
    repo.commit_all("right1");
    repo.git(&["checkout", "-q", "left"]);

    // Two independent, unrelated named imerges (both left unfinished).
    repo.imerge_ok(&["init", "--name", "alpha", "right"]);
    repo.imerge_ok(&["init", "--name", "beta", "right"]);

    let listed = String::from_utf8_lossy(&repo.imerge_ok(&["list"]).stdout).to_string();
    let names: Vec<&str> = listed.lines().map(|l| l.trim_start_matches(['*', ' '])).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));

    repo.imerge_ok(&["remove", "--name", "alpha"]);
    let listed_after = String::from_utf8_lossy(&repo.imerge_ok(&["list"]).stdout).to_string();
    assert!(!listed_after.contains("alpha"));
    assert!(listed_after.contains("beta"));

    // Removing the only remaining imerge leaves the list empty.
    repo.imerge_ok(&["remove", "--name", "beta"]);
    let listed_empty = String::from_utf8_lossy(&repo.imerge_ok(&["list"]).stdout).to_string();
    assert!(listed_empty.trim().is_empty());
    assert_eq!(repo.git(&["for-each-ref", "refs/imerge"]), "");
}

#[test]
fn diagram_reports_key_and_tip_names() {
    let repo = Repo::new();
    repo.write("a.txt", "base\n");
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "left"]);
    repo.write("left.txt", "left\n");
    repo.commit_all("left1");
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["checkout", "-q", "-b", "right"]);
    repo.write("right.txt", "right\n");
    repo.commit_all("right1");
    repo.git(&["checkout", "-q", "left"]);

    repo.imerge_ok(&["start", "--name", "d1", "right"]);
    let out = repo.imerge_ok(&["diagram", "--name", "d1", "--no-color"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("left"));
    assert!(text.contains("right"));
    assert!(text.contains("Key:"));
    assert!(text.contains("no merge recorded"));
}
