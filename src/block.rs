//! The 2D commit grid and the core pairwise-merge primitives.
//!
//! Cell `(i1, i2)` represents the result of merging `commits1[0..i1]` with
//! `commits2[0..i2]`. Row 0 and column 0 (the "known edges") are the
//! original branch commits (plus the merge base at `(0,0)`); every other
//! cell is filled in by pairwise merges as the incremental merge
//! progresses.
//!
//! This module ports the Python original's `MergeRecord` and `Block`
//! classes. Where the Python used `Block.__getitem__` with slices to
//! produce lightweight "views" (`SubBlock`) into a shared grid, this module
//! uses a plain `Rect` (a `Copy` struct of grid-relative coordinates) that
//! is passed alongside `&Grid`/`&mut Grid` instead: Rust's borrow checker
//! makes an owning "view that also mutates its parent" awkward, whereas
//! "absolute grid + a rectangle of local coordinates" is just data.

use crate::error::{ImergeError, Result};
use crate::git::{AutomergeOutcome, Git};

// ---------------------------------------------------------------------
// MergeRecord
// ---------------------------------------------------------------------

/// The state of a single grid cell: at most one merge result (`sha1`), plus
/// bits tracking where that result came from and whether it's currently
/// saved to a git ref.
#[derive(Clone, Debug, Default)]
pub struct MergeRecord {
    pub sha1: Option<String>,
    pub flags: u8,
}

impl MergeRecord {
    pub const SAVED_AUTO: u8 = 0x01;
    pub const NEW_AUTO: u8 = 0x02;
    pub const SAVED_MANUAL: u8 = 0x04;
    pub const NEW_MANUAL: u8 = 0x08;
    pub const BLOCKED: u8 = 0x10;

    #[allow(dead_code)]
    pub const SAVED: u8 = Self::SAVED_AUTO | Self::SAVED_MANUAL;
    #[allow(dead_code)]
    pub const NEW: u8 = Self::NEW_AUTO | Self::NEW_MANUAL;
    pub const AUTO: u8 = Self::SAVED_AUTO | Self::NEW_AUTO;
    pub const MANUAL: u8 = Self::SAVED_MANUAL | Self::NEW_MANUAL;

    pub fn new() -> Self {
        Self::default()
    }

    /// Record a merge result. `source` must be one of the four `*_AUTO`/
    /// `*_MANUAL` constants above. See the module-level notes in the
    /// architecture report: manual always beats auto; "saved" values are
    /// only used as a fallback when nothing fresher is known.
    pub fn record_merge(&mut self, sha1: &str, source: u8) {
        match source {
            Self::SAVED_AUTO => {
                if self.flags & (Self::MANUAL | Self::NEW) == 0 {
                    self.sha1 = Some(sha1.to_string());
                }
                self.flags |= source;
            }
            Self::NEW_AUTO => {
                if self.flags & Self::MANUAL == 0 {
                    self.sha1 = Some(sha1.to_string());
                    self.flags |= source;
                }
            }
            Self::SAVED_MANUAL => {
                if self.flags & Self::NEW_MANUAL == 0 {
                    self.sha1 = Some(sha1.to_string());
                }
                self.flags |= source;
            }
            Self::NEW_MANUAL => {
                self.sha1 = Some(sha1.to_string());
                self.flags = (self.flags | source) & !Self::NEW_AUTO;
            }
            _ => panic!("undefined merge source: {source}"),
        }
    }

    pub fn record_blocked(&mut self, blocked: bool) {
        if blocked {
            self.flags |= Self::BLOCKED;
        } else {
            self.flags &= !Self::BLOCKED;
        }
    }

    pub fn is_known(&self) -> bool {
        self.sha1.is_some()
    }

    pub fn is_blocked(&self) -> bool {
        self.flags & Self::BLOCKED != 0
    }

    pub fn is_manual(&self) -> bool {
        self.flags & Self::MANUAL != 0
    }

    /// Write/delete the git refs implied by any not-yet-saved flags, then
    /// clear them (converting `NEW_*` to `SAVED_*`).
    pub fn save(&mut self, git: &Git, name: &str, i1: usize, i2: usize) -> Result<()> {
        let set_ref = |source: &str, sha1: &str| -> Result<()> {
            git.update_ref(
                &format!("refs/imerge/{name}/{source}/{i1}-{i2}"),
                sha1,
                &format!("imerge '{name}': Record {source} merge"),
                true,
            )
        };
        let clear_ref = |source: &str| -> Result<()> {
            git.delete_ref(
                &format!("refs/imerge/{name}/{source}/{i1}-{i2}"),
                &format!("imerge '{name}': Remove obsolete {source} merge"),
            )
        };

        if self.flags & Self::MANUAL != 0 {
            if self.flags & Self::AUTO != 0 {
                if self.flags & Self::SAVED_AUTO != 0 {
                    clear_ref("auto")?;
                }
                self.flags &= !Self::AUTO;
            }
            if self.flags & Self::NEW_MANUAL != 0 {
                if let Some(sha1) = self.sha1.clone() {
                    set_ref("manual", &sha1)?;
                    self.flags |= Self::SAVED_MANUAL;
                } else if self.flags & Self::SAVED_MANUAL != 0 {
                    clear_ref("manual")?;
                    self.flags &= !Self::SAVED_MANUAL;
                }
                self.flags &= !Self::NEW_MANUAL;
            }
        } else if self.flags & Self::NEW_AUTO != 0 {
            if let Some(sha1) = self.sha1.clone() {
                set_ref("auto", &sha1)?;
                self.flags |= Self::SAVED_AUTO;
            } else if self.flags & Self::SAVED_AUTO != 0 {
                clear_ref("auto")?;
                self.flags &= !Self::SAVED_AUTO;
            }
            self.flags &= !Self::NEW_AUTO;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Diagram codes (shared by Grid and the frontier overlay in frontier.rs)
// ---------------------------------------------------------------------

pub const MERGE_UNKNOWN: u8 = 0;
pub const MERGE_MANUAL: u8 = 1;
pub const MERGE_AUTOMATIC: u8 = 2;
pub const MERGE_BLOCKED: u8 = 3;
pub const MERGE_UNBLOCKED: u8 = 4;
pub const MERGE_MASK: u8 = 7;

pub fn merge_state_code(is_known: bool, is_manual: bool, is_blocked: bool) -> u8 {
    match (is_known, is_manual, is_blocked) {
        (false, _, false) => MERGE_UNKNOWN,
        (false, _, true) => MERGE_BLOCKED,
        (true, _, true) => MERGE_UNBLOCKED,
        (true, false, false) => MERGE_AUTOMATIC,
        (true, true, false) => MERGE_MANUAL,
    }
}

// ---------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------

/// The dense 2D array of `MergeRecord`s. Always addressed by absolute
/// (origin-(0,0)) coordinates; `Rect` (below) provides the "windowed view"
/// abstraction the algorithms need.
pub struct Grid {
    pub name: String,
    pub len1: usize,
    pub len2: usize,
    data: Vec<Vec<MergeRecord>>,
}

impl Grid {
    pub fn new(name: impl Into<String>, len1: usize, len2: usize) -> Self {
        Grid {
            name: name.into(),
            len1,
            len2,
            data: (0..len1).map(|_| (0..len2).map(|_| MergeRecord::new()).collect()).collect(),
        }
    }

    pub fn get(&self, i1: usize, i2: usize) -> &MergeRecord {
        &self.data[i1][i2]
    }

    pub fn get_mut(&mut self, i1: usize, i2: usize) -> &mut MergeRecord {
        &mut self.data[i1][i2]
    }

    pub fn is_known(&self, i1: usize, i2: usize) -> bool {
        self.data[i1][i2].is_known()
    }

    pub fn is_blocked(&self, i1: usize, i2: usize) -> bool {
        self.data[i1][i2].is_blocked()
    }

    /// The area of the grid excluding the known edges (row 0 / column 0).
    #[allow(dead_code)]
    pub fn area(&self) -> usize {
        (self.len1 - 1) * (self.len2 - 1)
    }

    pub fn create_diagram(&self) -> Vec<Vec<u8>> {
        (0..self.len1)
            .map(|i1| {
                (0..self.len2)
                    .map(|i2| {
                        let rec = self.get(i1, i2);
                        merge_state_code(rec.is_known(), rec.is_manual(), rec.is_blocked())
                    })
                    .collect()
            })
            .collect()
    }
}

// ---------------------------------------------------------------------
// Rect: a window (with its own local (0,0) origin) into a Grid
// ---------------------------------------------------------------------

/// A rectangular view into a `Grid`, addressed by local coordinates
/// `(0..len1, 0..len2)` that map onto `(origin1.., origin2..)` in the
/// underlying grid. Mirrors the Python `Block`/`SubBlock` relationship,
/// except it carries no reference to the grid itself (it's plain `Copy`
/// data); callers pass `&Grid`/`&mut Grid` alongside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub origin1: usize,
    pub origin2: usize,
    pub len1: usize,
    pub len2: usize,
}

impl Rect {
    pub fn full(len1: usize, len2: usize) -> Rect {
        Rect { origin1: 0, origin2: 0, len1, len2 }
    }

    #[allow(dead_code)]
    pub fn area(&self) -> usize {
        (self.len1 - 1) * (self.len2 - 1)
    }

    /// Normalize possibly-negative local indexes (Python-list-style: -1 is
    /// the last element) into non-negative local coordinates, checking
    /// bounds.
    fn normalize(&self, i1: isize, i2: isize) -> Result<(usize, usize)> {
        let n1 = if i1 < 0 { i1 + self.len1 as isize } else { i1 };
        let n2 = if i2 < 0 { i2 + self.len2 as isize } else { i2 };
        if n1 < 0 || n1 as usize >= self.len1 {
            return Err(ImergeError::Failure(format!(
                "first index ({i1}) is out of range 0:{}",
                self.len1
            )));
        }
        if n2 < 0 || n2 as usize >= self.len2 {
            return Err(ImergeError::Failure(format!(
                "second index ({i2}) is out of range 0:{}",
                self.len2
            )));
        }
        Ok((n1 as usize, n2 as usize))
    }

    /// Local (possibly negative) coordinates -> absolute grid coordinates.
    pub fn abs(&self, i1: isize, i2: isize) -> Result<(usize, usize)> {
        let (n1, n2) = self.normalize(i1, i2)?;
        Ok((self.origin1 + n1, self.origin2 + n2))
    }

    /// Absolute grid coordinates -> local coordinates, if they fall inside
    /// this rect.
    pub fn to_local(&self, i1: usize, i2: usize) -> Option<(usize, usize)> {
        if i1 >= self.origin1
            && i1 < self.origin1 + self.len1
            && i2 >= self.origin2
            && i2 < self.origin2 + self.len2
        {
            Some((i1 - self.origin1, i2 - self.origin2))
        } else {
            None
        }
    }

    /// A sub-window at local offset `(start1, start2)` with the given
    /// dimensions (equivalent to Python's `block[start1:start1+len1,
    /// start2:start2+len2]`).
    pub fn sub(&self, start1: usize, len1: usize, start2: usize, len2: usize) -> Rect {
        Rect {
            origin1: self.origin1 + start1,
            origin2: self.origin2 + start2,
            len1,
            len2,
        }
    }

    pub fn known(&self, grid: &Grid, i1: isize, i2: isize) -> Result<bool> {
        let (a1, a2) = self.abs(i1, i2)?;
        Ok(grid.is_known(a1, a2))
    }

    pub fn blocked(&self, grid: &Grid, i1: isize, i2: isize) -> Result<bool> {
        let (a1, a2) = self.abs(i1, i2)?;
        Ok(grid.is_blocked(a1, a2))
    }

    pub fn sha1(&self, grid: &Grid, i1: isize, i2: isize) -> Result<Option<String>> {
        let (a1, a2) = self.abs(i1, i2)?;
        Ok(grid.get(a1, a2).sha1.clone())
    }

    pub fn record_merge(&self, grid: &mut Grid, i1: isize, i2: isize, sha1: &str, source: u8) -> Result<()> {
        let (a1, a2) = self.abs(i1, i2)?;
        grid.get_mut(a1, a2).record_merge(sha1, source);
        Ok(())
    }

    pub fn record_blocked(&self, grid: &mut Grid, i1: isize, i2: isize, blocked: bool) -> Result<()> {
        let (a1, a2) = self.abs(i1, i2)?;
        grid.get_mut(a1, a2).record_blocked(blocked);
        Ok(())
    }

    /// Determine whether `(i1,i2)` can be merged automatically: if already
    /// known, trivially yes; otherwise perform (and discard) a real
    /// speculative merge as an oracle. This has the same real side effects
    /// as the Python original (it leaves an unreferenced merge commit in
    /// the object database and the working tree checked out to it).
    pub fn is_mergeable(&self, git: &Git, grid: &mut Grid, i1: isize, i2: isize) -> Result<bool> {
        if self.known(grid, i1, i2)? {
            return Ok(true);
        }
        let (oi1, oi2) = self.abs(i1, i2)?;
        eprint!("Attempting automerge of {oi1}-{oi2}...");
        let c1 = self.sha1(grid, i1, 0)?.expect("row-0 commit must be known");
        let c2 = self.sha1(grid, 0, i2)?.expect("column-0 commit must be known");
        match git.automerge(&c1, &c2, None)? {
            AutomergeOutcome::Success(_) => {
                eprintln!("success.");
                Ok(true)
            }
            AutomergeOutcome::Conflict => {
                eprintln!("failure.");
                Ok(false)
            }
        }
    }

    /// Try to fill the single micromerge at local `(i1, i2)` (default
    /// `(1,1)`), given that `(i1-1,i2)` and `(i1,i2-1)` are already known.
    /// Returns whether it succeeded.
    pub fn auto_fill_micromerge(&self, git: &Git, grid: &mut Grid, i1: isize, i2: isize) -> Result<bool> {
        let (li1, li2) = self.normalize(i1, i2)?;
        if li1 >= self.len1 || li2 >= self.len2 || self.blocked(grid, i1, i2)? {
            return Ok(false);
        }
        let (oi1, oi2) = self.abs(i1, i2)?;
        eprint!("Attempting to merge {oi1}-{oi2}...");
        let logmsg = format!("imerge '{}': automatic merge {oi1}-{oi2}", grid.name);
        let above = self.sha1(grid, i1 as isize - 1, i2)?.expect("(i1-1,i2) must be known");
        let left = self.sha1(grid, i1, i2 as isize - 1)?.expect("(i1,i2-1) must be known");
        match git.automerge(&left, &above, Some(&logmsg))? {
            AutomergeOutcome::Conflict => {
                eprintln!("conflict.");
                self.record_blocked(grid, i1, i2, true)?;
                Ok(false)
            }
            AutomergeOutcome::Success(merge) => {
                eprintln!("success.");
                self.record_merge(grid, i1, i2, &merge, MergeRecord::NEW_AUTO)?;
                Ok(true)
            }
        }
    }

    /// Complete the outline of this rect: merge along the bottom edge
    /// (fixing `i2 = len2-1`), along the right edge (fixing `i1 = len1-1`),
    /// then compute the vertex two independent ways and require them to
    /// produce the same tree before accepting either (a real correctness
    /// check, not an optimization -- see architecture notes). On success,
    /// all newly-computed cells are recorded at once ("transactional").
    /// On an unexpected conflict (or vertex mismatch), returns
    /// `UnexpectedMergeFailure { i1, i2 }` (local coordinates) so the
    /// caller can backtrack.
    pub fn auto_outline(&self, git: &Git, grid: &mut Grid) -> Result<()> {
        let mut merges: Vec<(usize, usize, String)> = Vec::new();

        // do_merge: attempt a merge expected to succeed; on failure, signal
        // UnexpectedMergeFailure for backtracking instead of recording a
        // BLOCKED cell (that's the auto_fill_micromerge path, not this
        // one). Takes `grid` as an explicit parameter (rather than
        // capturing it) so callers can pass a fresh reborrow each time and
        // the mutable `grid` reference stays free for use between calls.
        let do_merge = |grid: &Grid,
                         i1: usize,
                         commit1: &str,
                         i2: usize,
                         commit2: &str,
                         label: &str,
                         record: bool,
                         merges: &mut Vec<(usize, usize, String)>|
         -> Result<String> {
            if let Some(sha1) = self.sha1(grid, i1 as isize, i2 as isize)? {
                return Ok(sha1);
            }
            let (oi1, oi2) = self.abs(i1 as isize, i2 as isize)?;
            eprint!("{}", label.replace("{i1}", &oi1.to_string()).replace("{i2}", &oi2.to_string()));
            let logmsg = format!("imerge '{}': automatic merge {oi1}-{oi2}", grid.name);
            match git.automerge(commit1, commit2, Some(&logmsg))? {
                AutomergeOutcome::Conflict => {
                    eprintln!("unexpected conflict.  Backtracking...");
                    Err(ImergeError::UnexpectedMergeFailure {
                        i1,
                        i2,
                        msg: format!("automatic merge of {commit1} and {commit2} failed"),
                    })
                }
                AutomergeOutcome::Success(merge) => {
                    eprintln!("success.");
                    if record {
                        merges.push((i1, i2, merge.clone()));
                    }
                    Ok(merge)
                }
            }
        };

        let i2 = self.len2 - 1;
        let mut left = self.sha1(grid, 0, i2 as isize)?.expect("(0,len2-1) must be known");
        for i1 in 1..self.len1 - 1 {
            let c1 = self.sha1(grid, i1 as isize, 0)?.expect("(i1,0) must be known");
            left = do_merge(grid, i1, &c1, i2, &left, "Autofilling {i1}-{i2}...", true, &mut merges)?;
        }

        let i1 = self.len1 - 1;
        let mut above = self.sha1(grid, i1 as isize, 0)?.expect("(len1-1,0) must be known");
        for i2 in 1..self.len2 - 1 {
            let c2 = self.sha1(grid, 0, i2 as isize)?.expect("(0,i2) must be known");
            above = do_merge(grid, i1, &above, i2, &c2, "Autofilling {i1}-{i2}...", true, &mut merges)?;
        }

        let (i1, i2) = (self.len1 - 1, self.len2 - 1);
        if i1 > 1 && i2 > 1 {
            let c1 = self.sha1(grid, i1 as isize, 0)?.expect("(len1-1,0) must be known");
            let vertex_v1 = do_merge(
                grid,
                i1,
                &c1,
                i2,
                &left,
                "Autofilling {i1}-{i2} (first way)...",
                false,
                &mut merges,
            )?;
            let c2 = self.sha1(grid, 0, i2 as isize)?.expect("(0,len2-1) must be known");
            let vertex_v2 = do_merge(
                grid,
                i1,
                &above,
                i2,
                &c2,
                "Autofilling {i1}-{i2} (second way)...",
                false,
                &mut merges,
            )?;
            let (oi1, oi2) = self.abs(i1 as isize, i2 as isize)?;
            if git.get_tree(&vertex_v1)? == git.get_tree(&vertex_v2)? {
                eprintln!("The two ways of autofilling {oi1}-{oi2} agree.");
                let reparented = git.reparent(&vertex_v1, &[above.clone(), left.clone()], None)?;
                merges.push((i1, i2, reparented));
            } else {
                eprintln!("The two ways of autofilling {oi1}-{oi2} do not agree.  Backtracking...");
                return Err(ImergeError::UnexpectedMergeFailure {
                    i1,
                    i2,
                    msg: "Inconsistent vertex merges".to_string(),
                });
            }
        } else {
            do_merge(grid, i1, &above, i2, &left, "Autofilling {i1}-{i2}...", true, &mut merges)?;
        }

        eprintln!(
            "Recording autofilled block [{}:{},{}:{}].",
            self.origin1,
            self.origin1 + self.len1,
            self.origin2,
            self.origin2 + self.len2
        );
        for (i1, i2, merge) in merges {
            self.record_merge(grid, i1 as isize, i2 as isize, &merge, MergeRecord::NEW_AUTO)?;
        }
        Ok(())
    }

    pub fn create_diagram(&self, grid: &Grid) -> Vec<Vec<u8>> {
        (0..self.len1)
            .map(|i1| {
                (0..self.len2)
                    .map(|i2| {
                        let (a1, a2) = (self.origin1 + i1, self.origin2 + i2);
                        let rec = grid.get(a1, a2);
                        merge_state_code(rec.is_known(), rec.is_manual(), rec.is_blocked())
                    })
                    .collect()
            })
            .collect()
    }
}
