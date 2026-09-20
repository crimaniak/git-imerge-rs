//! Merge-frontier strategies: which cells are considered "done" and how to
//! push the frontier forward.
//!
//! Three strategies exist, matching the Python original:
//!
//! * [`Frontier::Full`] -- fill every cell one at a time (`--goal=full`).
//! * [`Frontier::Manual`] -- like `Full`, but never attempts an automerge;
//!   every cell requires a manual resolution (`--manual`).
//! * [`Frontier::Blockwise`] -- the default: use bisection
//!   ([`find_frontier_blocks`]) to guess large rectangular regions that
//!   are probably entirely auto-mergeable, verify the guess by actually
//!   outlining the region ([`crate::block::Rect::auto_outline`]), and
//!   backtrack/re-bisect on any inconsistency.

use crate::block::{Grid, Rect};
use crate::error::{ImergeError, Result};
use crate::git::Git;

pub const FRONTIER_WITHIN: u8 = 0x10;
pub const FRONTIER_RIGHT_EDGE: u8 = 0x20;
pub const FRONTIER_BOTTOM_EDGE: u8 = 0x40;

/// Binary search for the smallest `i` in `lo..hi` for which `f(i)` returns
/// false, assuming `f` is true for a prefix and false after. Returns `hi`
/// if there is no such `i`.
fn find_first_false<F>(mut lo: usize, mut hi: usize, mut f: F) -> Result<usize>
where
    F: FnMut(usize) -> Result<bool>,
{
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if f(mid)? {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// Bisection search for rectangular regions of `block` that are probably
/// entirely auto-mergeable (see the module docs and the architecture
/// report for the staircase-assumption diagrams this implements). Not a
/// guarantee -- callers must verify by actually outlining each candidate.
pub fn find_frontier_blocks(git: &Git, grid: &mut Grid, block: Rect) -> Result<Vec<Rect>> {
    let mut result = Vec::new();

    if block.len1 <= 1 || block.len2 <= 1 || block.blocked(grid, 1, 1)? {
        return Ok(result);
    }

    if block.is_mergeable(git, grid, block.len1 as isize - 1, block.len2 as isize - 1)? {
        result.push(block);
        return Ok(result);
    }

    if !block.is_mergeable(git, grid, 1, 1)? {
        block.record_blocked(grid, 1, 1, true)?;
        return Ok(result);
    }

    let i1_fixed = 1usize;
    let mut i2 = find_first_false(2, block.len2, |i| {
        block.is_mergeable(git, grid, i1_fixed as isize, i as isize)
    })?;
    let mut i1 = i1_fixed;

    loop {
        if i2 == 1 {
            break;
        }

        if i1 == block.len1 - 1 || block.is_mergeable(git, grid, block.len1 as isize - 1, i2 as isize - 1)? {
            result.push(block.sub(0, block.len1, 0, i2));
            break;
        } else {
            i1 = find_first_false(i1 + 1, block.len1 - 1, |i| {
                block.is_mergeable(git, grid, i as isize, i2 as isize - 1)
            })?;
            result.push(block.sub(0, i1, 0, i2));
        }

        if i2 - 1 == 1 || !block.is_mergeable(git, grid, i1 as isize, 1)? {
            break;
        } else {
            i2 = find_first_false(2, i2 - 1, |i| block.is_mergeable(git, grid, i1 as isize, i as isize))?;
        }
    }

    Ok(result)
}

fn normalized_blocks(mut blocks: Vec<Rect>) -> Vec<Rect> {
    blocks.retain(|b| b.len1 != 0 && b.len2 != 0);
    blocks.sort_by_key(|b| b.len1);

    fn contains(a: Rect, b: Rect) -> bool {
        a.len1 >= b.len1 && a.len2 >= b.len2
    }

    let mut ret: Vec<Rect> = Vec::new();
    for block in blocks {
        loop {
            match ret.last().copied() {
                None => {
                    ret.push(block);
                    break;
                }
                Some(last) => {
                    if contains(last, block) {
                        break;
                    } else if contains(block, last) {
                        ret.pop();
                    } else {
                        ret.push(block);
                        break;
                    }
                }
            }
        }
    }
    ret
}

fn remove_failure(blocks: &[Rect], i1: usize, i2: usize) -> Vec<Rect> {
    let mut newblocks = Vec::new();
    let mut shrunk = false;
    for &block in blocks {
        if i1 < block.len1 && i2 < block.len2 {
            if i1 > 1 {
                newblocks.push(block.sub(0, i1, 0, block.len2));
            }
            if i2 > 1 {
                newblocks.push(block.sub(0, block.len1, 0, i2));
            }
            shrunk = true;
        } else {
            newblocks.push(block);
        }
    }
    if shrunk {
        normalized_blocks(newblocks)
    } else {
        blocks.to_vec()
    }
}

/// Partition `blocks` (belonging to a frontier over `top`) around an
/// already-outlined `target` block, returning up to two
/// `(new_top, new_blocks)` pairs for the sub-frontiers to the left/right.
fn partition(top: Rect, blocks: &[Rect], target: Rect) -> Result<Vec<(Rect, Vec<Rect>)>> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for &b in blocks {
        if b.len1 == target.len1 && b.len2 == target.len2 {
            continue;
        } else if b.len1 < target.len1 && b.len2 > target.len2 {
            left.push(b.sub(0, b.len1, target.len2 - 1, b.len2 - (target.len2 - 1)));
        } else if b.len1 > target.len1 && b.len2 < target.len2 {
            right.push(b.sub(target.len1 - 1, b.len1 - (target.len1 - 1), 0, b.len2));
        } else {
            return Err(ImergeError::Failure(
                "BlockwiseMergeFrontier partitioned with inappropriate block".to_string(),
            ));
        }
    }

    let mut result = Vec::new();
    if target.len2 < top.len2 {
        let new_top = top.sub(0, target.len1, target.len2 - 1, top.len2 - (target.len2 - 1));
        result.push((new_top, normalized_blocks(left)));
    }
    if target.len1 < top.len1 {
        let new_top = top.sub(target.len1 - 1, top.len1 - (target.len1 - 1), 0, target.len2);
        result.push((new_top, normalized_blocks(right)));
    }
    Ok(result)
}

fn iter_boundary_blocks(top: Rect, blocks: &[Rect]) -> Vec<Rect> {
    let mut result = Vec::new();
    if blocks.is_empty() || blocks[0].len2 < top.len2 {
        result.push(top.sub(0, 1, 0, top.len2));
    }
    result.extend_from_slice(blocks);
    if blocks.is_empty() || blocks.last().unwrap().len1 < top.len1 {
        result.push(top.sub(0, top.len1, 0, 1));
    }
    result
}

fn iter_blocker_blocks(top: Rect, blocks: &[Rect]) -> Vec<Rect> {
    let boundary = iter_boundary_blocks(top, blocks);
    boundary
        .windows(2)
        .map(|w| {
            let (b1, b2) = (w[0], w[1]);
            top.sub(
                b1.len1 - 1,
                b2.len1 - (b1.len1 - 1),
                b2.len2 - 1,
                b1.len2 - (b2.len2 - 1),
            )
        })
        .collect()
}

fn get_affected_blocker_block(top: Rect, blocks: &[Rect], i1: usize, i2: usize) -> Result<Rect> {
    for block in iter_blocker_blocks(top, blocks) {
        if let Some(local) = block.to_local(i1, i2) {
            return if local == (1, 1) {
                Ok(block)
            } else {
                Err(ImergeError::NotABlockingCommit(format!(
                    "Commit {i1}-{i2} was not blocking the frontier."
                )))
            };
        }
    }
    Err(ImergeError::NotABlockingCommit(format!(
        "Commit {i1}-{i2} was not on the frontier."
    )))
}

/// Reconstruct the current blockwise frontier from what's already known in
/// `grid` (used whenever we resume an in-progress imerge, as opposed to
/// probing for new automerges). Walks a staircase path from the top-right
/// down/left, backtracking when stuck; see the architecture report for the
/// algorithm's `FIXME` about pathological combinatorial cases (this
/// implementation is intentionally never fed inputs that trigger it, same
/// as upstream).
pub fn map_known_frontier(grid: &Grid, block: Rect) -> Result<(Rect, Vec<Rect>)> {
    let mut path: Vec<(usize, usize, bool)> = Vec::new();
    let (mut i1, mut i2) = (block.len1 - 1, 0usize);
    let mut down = true;

    loop {
        if down {
            if i2 == block.len2 - 1 {
                down = false;
            } else if block.known(grid, i1 as isize, i2 as isize + 1)?
                && !block.blocked(grid, i1 as isize, i2 as isize + 1)?
            {
                path.push((i1, i2, true));
                i2 += 1;
            } else {
                down = false;
            }
        } else if i1 == 0 {
            path.push((i1, i2, false));
            break;
        } else if block.known(grid, i1 as isize - 1, i2 as isize)?
            && !block.blocked(grid, i1 as isize - 1, i2 as isize)?
        {
            path.push((i1, i2, false));
            down = true;
            i1 -= 1;
        } else {
            loop {
                let (pi1, pi2, pdown) = path
                    .pop()
                    .ok_or_else(|| ImergeError::Failure("Block is improperly formed!".to_string()))?;
                i1 = pi1;
                i2 = pi2;
                down = pdown;
                if down {
                    down = false;
                    break;
                }
            }
        }
    }

    let mut blocks = Vec::new();
    for w in path.windows(2) {
        let (_, _, downold) = w[0];
        let (i1new, i2new, downnew) = w[1];
        if downold && !downnew {
            blocks.push(block.sub(0, i1new + 1, 0, i2new + 1));
        }
    }
    Ok((block, normalized_blocks(blocks)))
}

/// Compute the boundary blocks for `block` via bisection, then outline
/// each candidate for real, backtracking (re-bisecting narrower regions)
/// on any unexpected conflict or vertex inconsistency. Returns the final
/// `(top_block, blocks)` frontier once no further outlining is possible.
pub fn initiate_merge(git: &Git, grid: &mut Grid, block: Rect) -> Result<(Rect, Vec<Rect>)> {
    let mut top_blocks = normalized_blocks(find_frontier_blocks(git, grid, block)?);
    let top_block = block;

    let mut cur_block = block;
    let mut cur_blocks = top_blocks.clone();
    let mut is_top = true;

    loop {
        if cur_blocks.is_empty() {
            break;
        }
        let subblock = cur_blocks[0];
        match subblock.auto_outline(git, grid) {
            Err(ImergeError::UnexpectedMergeFailure { i1, i2, .. }) => {
                cur_blocks = remove_failure(&cur_blocks, i1, i2);
                if is_top {
                    top_blocks = cur_blocks.clone();
                }
                if (i1, i2) == (1, 1) {
                    subblock.record_blocked(grid, 1, 1, true)?;
                }
                if !is_top {
                    let (abs1, abs2) = subblock.abs(i1 as isize, i2 as isize)?;
                    if let Some((t1, t2)) = top_block.to_local(abs1, abs2) {
                        top_blocks = remove_failure(&top_blocks, t1, t2);
                    }
                }
            }
            Err(e) => return Err(e),
            Ok(()) => {
                let mut subs = partition(cur_block, &cur_blocks, subblock)?;
                subs.retain(|(_, b)| !b.is_empty());
                if subs.is_empty() {
                    break;
                }
                let (nb, nbs) = subs.remove(0);
                cur_block = nb;
                cur_blocks = nbs;
                is_top = false;
            }
        }
    }

    Ok((top_block, top_blocks))
}

fn blockwise_create_diagram(grid: &Grid, top: Rect, blocks: &[Rect]) -> Vec<Vec<u8>> {
    let mut diagram = top.create_diagram(grid);
    let len1 = top.len1;
    let len2 = top.len2;

    let first = blocks.first().copied();
    diagram[0][len2 - 1] |= FRONTIER_BOTTOM_EDGE;
    for i2 in 1..len2 {
        if first.map_or(true, |b| i2 >= b.len2) {
            diagram[0][i2] |= FRONTIER_RIGHT_EDGE;
        }
    }

    let mut prev_block: Option<Rect> = None;
    for n in 0..blocks.len() {
        let block = blocks[n];
        let next_block = blocks.get(n + 1).copied();
        for i1 in 0..block.len1 {
            for i2 in 0..block.len2 {
                let mut v = FRONTIER_WITHIN;
                if i1 == block.len1 - 1 && next_block.map_or(true, |b| i2 >= b.len2) {
                    v |= FRONTIER_RIGHT_EDGE;
                }
                if i2 == block.len2 - 1 && prev_block.map_or(true, |b| i1 >= b.len1) {
                    v |= FRONTIER_BOTTOM_EDGE;
                }
                diagram[i1][i2] |= v;
            }
        }
        prev_block = Some(block);
    }

    let prev_block = blocks.last().copied();
    for i1 in 1..len1 {
        if prev_block.map_or(true, |b| i1 >= b.len1) {
            diagram[i1][0] |= FRONTIER_BOTTOM_EDGE;
        }
    }
    diagram[len1 - 1][0] |= FRONTIER_RIGHT_EDGE;

    diagram
}

fn full_auto_expand(block: Rect, git: &Git, grid: &mut Grid) -> Result<()> {
    let mut len2 = block.len2;
    let mut blocker: Option<(usize, usize)> = None;
    for i1 in 1..block.len1 {
        for i2 in 1..len2 {
            if block.known(grid, i1 as isize, i2 as isize)? {
                continue;
            } else if block.blocked(grid, i1 as isize, i2 as isize)? {
                if blocker.is_none() {
                    blocker = Some(block.abs(i1 as isize, i2 as isize)?);
                }
                len2 = i2;
                break;
            } else if block.auto_fill_micromerge(git, grid, i1 as isize, i2 as isize)? {
                continue;
            } else {
                block.record_blocked(grid, i1 as isize, i2 as isize, true)?;
                if blocker.is_none() {
                    blocker = Some(block.abs(i1 as isize, i2 as isize)?);
                }
                len2 = i2;
                break;
            }
        }
    }
    match blocker {
        Some((i1, i2)) => Err(ImergeError::FrontierBlocked {
            i1,
            i2,
            msg: format!("Conflict; suggest manual merge of {i1}-{i2}"),
        }),
        None => Err(ImergeError::BlockComplete),
    }
}

fn manual_auto_expand(block: Rect, grid: &Grid) -> Result<()> {
    for i1 in 1..block.len1 {
        for i2 in 1..block.len2 {
            if !block.known(grid, i1 as isize, i2 as isize)? {
                let (oi1, oi2) = block.abs(i1 as isize, i2 as isize)?;
                return Err(ImergeError::FrontierBlocked {
                    i1: oi1,
                    i2: oi2,
                    msg: format!("Manual merges requested; please merge {oi1}-{oi2}"),
                });
            }
        }
    }
    Err(ImergeError::BlockComplete)
}

fn blockwise_auto_expand(git: &Git, grid: &mut Grid, top: Rect, blocks: &[Rect]) -> Result<()> {
    let mut blocker_blocks = iter_blocker_blocks(top, blocks);
    if blocker_blocks.is_empty() {
        return Err(ImergeError::BlockComplete);
    }
    blocker_blocks.sort_by_key(|b| b.abs(0, 0).unwrap());

    for block in &blocker_blocks {
        let (_new_top, new_blocks) = initiate_merge(git, grid, *block)?;
        if !new_blocks.is_empty() {
            return Ok(());
        }
    }
    let (i1, i2) = blocker_blocks[0].abs(1, 1)?;
    Err(ImergeError::FrontierBlocked {
        i1,
        i2,
        msg: format!("Conflict; suggest manual merge of {i1}-{i2}"),
    })
}

/// A merge-frontier snapshot: which strategy is in effect, and (for the
/// blockwise strategy) which regions are currently believed fully known.
pub enum Frontier {
    Full(Rect),
    Manual(Rect),
    Blockwise { top: Rect, blocks: Vec<Rect> },
}

impl Frontier {
    pub fn block(&self) -> Rect {
        match self {
            Frontier::Full(b) | Frontier::Manual(b) => *b,
            Frontier::Blockwise { top, .. } => *top,
        }
    }

    pub fn is_complete(&self, grid: &Grid) -> Result<bool> {
        match self {
            Frontier::Full(b) | Frontier::Manual(b) => b.known(grid, -1, -1),
            Frontier::Blockwise { top, blocks } => Ok(blocks.len() == 1
                && blocks[0].len1 == top.len1
                && blocks[0].len2 == top.len2),
        }
    }

    /// Try to push the frontier forward. `Ok(())` means progress was made
    /// and the caller should re-map the frontier and call again;
    /// `Err(ImergeError::BlockComplete)`/`Err(ImergeError::FrontierBlocked
    /// {..})` are the terminal signals (see [`crate::state::MergeState::
    /// auto_complete_frontier`]).
    pub fn auto_expand(&self, git: &Git, grid: &mut Grid) -> Result<()> {
        match self {
            Frontier::Full(b) => full_auto_expand(*b, git, grid),
            Frontier::Manual(b) => manual_auto_expand(*b, grid),
            Frontier::Blockwise { top, blocks } => blockwise_auto_expand(git, grid, *top, blocks),
        }
    }

    pub fn incorporate_merge(&self, grid: &mut Grid, i1: usize, i2: usize) -> Result<()> {
        match self {
            Frontier::Full(b) | Frontier::Manual(b) => {
                if !b.blocked(grid, i1 as isize, i2 as isize)? {
                    Err(ImergeError::NotABlockingCommit(format!(
                        "Commit {i1}-{i2} was not on the frontier."
                    )))
                } else {
                    b.record_blocked(grid, i1 as isize, i2 as isize, false)
                }
            }
            Frontier::Blockwise { top, blocks } => {
                let block = get_affected_blocker_block(*top, blocks, i1, i2)?;
                block.record_blocked(grid, 1, 1, false)
            }
        }
    }

    pub fn create_diagram(&self, grid: &Grid) -> Vec<Vec<u8>> {
        match self {
            Frontier::Full(b) | Frontier::Manual(b) => b.create_diagram(grid),
            Frontier::Blockwise { top, blocks } => blockwise_create_diagram(grid, *top, blocks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MergeRecord;

    fn rect(len1: usize, len2: usize) -> Rect {
        Rect { origin1: 0, origin2: 0, len1, len2 }
    }

    // -- find_first_false ---------------------------------------------------

    #[test]
    fn find_first_false_basic() {
        let f = |i: usize| -> Result<bool> { Ok(i < 5) };
        assert_eq!(find_first_false(0, 10, f).unwrap(), 5);
        assert_eq!(find_first_false(0, 3, f).unwrap(), 3, "all-true range returns hi");
        assert_eq!(find_first_false(6, 10, f).unwrap(), 6, "all-false range returns lo");
    }

    // -- normalized_blocks ---------------------------------------------------

    #[test]
    fn normalized_blocks_drops_contained_blocks() {
        let big = rect(4, 4);
        let small = rect(2, 2);
        assert_eq!(normalized_blocks(vec![small, big]), vec![big]);
    }

    #[test]
    fn normalized_blocks_keeps_staircase_blocks_sorted_by_len1() {
        let a = rect(2, 5);
        let b = rect(5, 2);
        assert_eq!(normalized_blocks(vec![b, a]), vec![a, b]);
    }

    #[test]
    fn normalized_blocks_drops_empty_blocks() {
        let a = rect(0, 3);
        let b = rect(3, 0);
        let c = rect(2, 2);
        assert_eq!(normalized_blocks(vec![a, b, c]), vec![c]);
    }

    // -- remove_failure ------------------------------------------------------

    #[test]
    fn remove_failure_shrinks_containing_block_both_ways() {
        let block = rect(5, 5);
        let result = remove_failure(&[block], 3, 3);
        assert_eq!(result.len(), 2);
        assert!(result.contains(&block.sub(0, 3, 0, 5)));
        assert!(result.contains(&block.sub(0, 5, 0, 3)));
    }

    #[test]
    fn remove_failure_at_first_row_only_shrinks_columns() {
        let block = rect(5, 5);
        // i1 == 1 means the "i1 > 1" shrink is skipped.
        assert_eq!(remove_failure(&[block], 1, 3), vec![block.sub(0, 5, 0, 3)]);
    }

    #[test]
    fn remove_failure_leaves_unrelated_blocks_untouched() {
        let block = rect(3, 3);
        // (5,5) is outside `block`'s bounds entirely.
        assert_eq!(remove_failure(&[block], 5, 5), vec![block]);
    }

    // -- partition -------------------------------------------------------

    #[test]
    fn partition_splits_around_a_centered_target() {
        let top = rect(6, 6);
        let target = rect(3, 3);
        let result = partition(top, &[target], target).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, top.sub(0, 3, 2, 4));
        assert!(result[0].1.is_empty());
        assert_eq!(result[1].0, top.sub(2, 4, 0, 3));
        assert!(result[1].1.is_empty());
    }

    #[test]
    fn partition_of_the_whole_top_yields_nothing() {
        let top = rect(4, 4);
        assert!(partition(top, &[top], top).unwrap().is_empty());
    }

    // -- boundary / blocker blocks --------------------------------------

    #[test]
    fn blocker_blocks_of_an_empty_frontier_is_the_whole_grid() {
        let top = rect(4, 4);
        let blockers = iter_blocker_blocks(top, &[]);
        assert_eq!(blockers, vec![top]);
    }

    #[test]
    fn blocker_blocks_shrink_around_a_full_width_outline() {
        let top = rect(4, 4);
        // Outlined across the full width (all of i2) but only the first
        // two rows of i1: exactly one gap remains, from the outlined
        // block's corner to top's far corner.
        let outlined = top.sub(0, 2, 0, 4);
        let blockers = iter_blocker_blocks(top, &[outlined]);
        assert_eq!(blockers, vec![top.sub(1, 3, 0, 4)]);
    }

    // -- map_known_frontier (pure grid reconstruction, no git needed) ----

    #[test]
    fn map_known_frontier_reconstructs_staircase() {
        let mut grid = Grid::new("t", 3, 3);
        grid.get_mut(0, 0).record_merge("base", MergeRecord::NEW_MANUAL);
        grid.get_mut(1, 0).record_merge("c10", MergeRecord::NEW_MANUAL);
        grid.get_mut(2, 0).record_merge("c20", MergeRecord::NEW_MANUAL);
        grid.get_mut(0, 1).record_merge("c01", MergeRecord::NEW_MANUAL);
        grid.get_mut(0, 2).record_merge("c02", MergeRecord::NEW_MANUAL);
        grid.get_mut(1, 1).record_merge("m11", MergeRecord::NEW_AUTO);
        grid.get_mut(2, 1).record_merge("m21", MergeRecord::NEW_AUTO);
        // (1,2) and (2,2) are left unknown.

        let full = Rect::full(3, 3);
        let (top, blocks) = map_known_frontier(&grid, full).unwrap();
        assert_eq!(top, full);
        // Row 0 is known on its own (a degenerate 1x3 sliver), and rows
        // 0-2 x columns 0-1 form the real outlined 3x2 block covering the
        // known (1,1)/(2,1) cells.
        assert_eq!(blocks, vec![rect(1, 3), rect(3, 2)]);
    }

    #[test]
    fn map_known_frontier_of_fresh_grid_has_no_real_progress() {
        let mut grid = Grid::new("t", 3, 3);
        grid.get_mut(0, 0).record_merge("base", MergeRecord::NEW_MANUAL);
        grid.get_mut(1, 0).record_merge("c10", MergeRecord::NEW_MANUAL);
        grid.get_mut(2, 0).record_merge("c20", MergeRecord::NEW_MANUAL);
        grid.get_mut(0, 1).record_merge("c01", MergeRecord::NEW_MANUAL);
        grid.get_mut(0, 2).record_merge("c02", MergeRecord::NEW_MANUAL);
        // No interior cell is known.

        let full = Rect::full(3, 3);
        let (top, blocks) = map_known_frontier(&grid, full).unwrap();
        // Every reconstructed block is a degenerate (zero-area) sliver
        // along the known edges -- meaning the whole interior is still
        // exactly one blocker region, not real progress.
        assert!(blocks.iter().all(|b| b.area() == 0));
        assert_eq!(iter_blocker_blocks(top, &blocks), vec![full]);
    }

    #[test]
    fn full_frontier_is_complete_iff_vertex_known() {
        let mut grid = Grid::new("t", 2, 2);
        grid.get_mut(0, 0).record_merge("base", MergeRecord::NEW_MANUAL);
        grid.get_mut(1, 0).record_merge("c1", MergeRecord::NEW_MANUAL);
        grid.get_mut(0, 1).record_merge("c2", MergeRecord::NEW_MANUAL);
        let frontier = Frontier::Full(Rect::full(2, 2));
        assert!(!frontier.is_complete(&grid).unwrap());

        grid.get_mut(1, 1).record_merge("vertex", MergeRecord::NEW_AUTO);
        assert!(frontier.is_complete(&grid).unwrap());
    }
}
