use std::fmt;

/// A single error/control-flow type used throughout git-imerge.
///
/// The original Python implementation uses a rich hierarchy of exceptions,
/// some of which are genuine user-facing errors (`Failure` and its
/// subclasses) and some of which are internal control-flow signals used to
/// communicate between the merge-frontier algorithm and its callers
/// (`BlockCompleteError`, `FrontierBlockedError`, `UnexpectedMergeFailure`,
/// etc). Rust has no exceptions, so all of these are modeled as variants of
/// one enum and propagated with `?` / `Result`.
///
/// Every variant has a reasonable `Display` message; `main` simply prints
/// `Display` and exits with status 1 on any error (this is a deliberate
/// simplification of the Python original, which prints a clean message only
/// for `Failure`-derived exceptions and a full traceback for anything else).
#[derive(Debug)]
pub enum ImergeError {
    /// A plain user-facing error message.
    Failure(String),

    /// A merge that was expected (by `auto_outline`) to succeed conflicted
    /// unexpectedly; carries the block-relative coordinates of the failure
    /// so the caller can backtrack.
    UnexpectedMergeFailure { i1: usize, i2: usize, msg: String },

    /// Signal: the block/frontier being expanded is already fully known.
    BlockComplete,

    /// Signal: progress is blocked at (i1, i2) (original/global coordinates)
    /// and the user must resolve it manually.
    FrontierBlocked { i1: usize, i2: usize, msg: String },

    /// A commit was reported as resolved but does not correspond to any
    /// cell that was actually blocking the frontier.
    NotABlockingCommit(String),

    /// There is no manual merge available to incorporate.
    NoManualMerge(String),

    /// A user-supplied manual merge commit cannot be used as-is.
    ManualMergeUnusable { commit: String, msg: String },

    /// A commit could not be found among the known merge results.
    CommitNotFound(String),

    /// A simplification path referenced a grid cell that isn't known yet.
    MissingMerge { i1: usize, i2: usize },

    /// One of the two branches has no commits beyond the merge base.
    NothingToDo,

    /// The working tree has unstaged or uncommitted changes.
    UncleanWorkTree(String),

    /// `commit1..commit2` contains more than one path (a real merge in the
    /// history) and `--first-parent` wasn't given. Callers at the CLI
    /// layer append a "Perhaps use --first-parent?" hint to this one
    /// specifically (matching the Python original).
    NonlinearAncestry(String),

    /// `commit1` is not an ancestor of `commit2` at all (no hint appended).
    NotFirstParentAncestor(String),
}

impl fmt::Display for ImergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImergeError::Failure(msg) => write!(f, "{msg}"),
            ImergeError::UnexpectedMergeFailure { i1, i2, msg } => {
                write!(f, "unexpected merge failure at {i1}-{i2}: {msg}")
            }
            ImergeError::BlockComplete => write!(f, "the block is already complete"),
            ImergeError::FrontierBlocked { msg, .. } => write!(f, "{msg}"),
            ImergeError::NotABlockingCommit(msg) => write!(f, "{msg}"),
            ImergeError::NoManualMerge(msg) => write!(f, "{msg}"),
            ImergeError::ManualMergeUnusable { commit, msg } => {
                write!(f, "commit {commit} is not usable; {msg}")
            }
            ImergeError::CommitNotFound(commit) => write!(
                f,
                "commit {commit} was not found among the known merge commits"
            ),
            ImergeError::MissingMerge { i1, i2 } => write!(f, "merge {i1}-{i2} is not yet done"),
            ImergeError::NothingToDo => write!(f, "nothing to do"),
            ImergeError::UncleanWorkTree(msg) => write!(f, "{msg}"),
            ImergeError::NonlinearAncestry(msg) => write!(f, "{msg}"),
            ImergeError::NotFirstParentAncestor(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ImergeError {}

impl From<anyhow::Error> for ImergeError {
    fn from(e: anyhow::Error) -> Self {
        ImergeError::Failure(format!("{e:#}"))
    }
}

impl From<std::io::Error> for ImergeError {
    fn from(e: std::io::Error) -> Self {
        ImergeError::Failure(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ImergeError>;

pub fn failure<T>(msg: impl Into<String>) -> Result<T> {
    Err(ImergeError::Failure(msg.into()))
}
