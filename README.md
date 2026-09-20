# git-imerge-rs

A Rust port of [git-imerge](https://github.com/mhagger/git-imerge): incremental
merge and rebase for Git. Perform a merge or rebase between two branches
commit-by-commit, so that if conflicts occur you're shown the smallest
possible conflict (between one commit from each side) instead of one huge
tangled one. The merge can be interrupted, resumed, pushed to a remote, and
worked on collaboratively, since all of its state lives in the Git object
database under `refs/imerge/<name>/*`.

This is a from-scratch reimplementation in Rust of the original Python tool,
built by reading `gitimerge.py` end to end and translating its data model,
algorithms and CLI surface as closely as possible. **The on-disk state format
(refs layout and JSON schema under `refs/imerge/<name>/*`) is unchanged**, so
an incremental merge started with the original `git-imerge` can be continued
with `git-imerge-rs` and vice versa.

## Building

```sh
cargo build --release
```

This produces a single binary, `target/release/git-imerge`, that behaves as
a `git` subcommand (`git imerge ...`) once it's on your `PATH` — exactly like
the original.

## Usage

The command surface mirrors the original tool:

| Command | Effect |
| --- | --- |
| `git-imerge merge BRANCH` | Merge `BRANCH` into the current branch, incrementally |
| `git-imerge rebase BRANCH` | Rebase the current branch onto `BRANCH`, incrementally |
| `git-imerge drop COMMIT[..COMMIT]` | Drop one or more commits from the current branch |
| `git-imerge revert COMMIT[..COMMIT]` | Revert one or more commits |
| `git-imerge init`/`start` | Lower-level: set up (and optionally start filling) a named incremental merge between two arbitrary tips |
| `git-imerge continue` | Record a manually-resolved conflict and push forward to the next one |
| `git-imerge record` | Like `continue`, but doesn't set up the working tree for the next conflict |
| `git-imerge autofill` | Push the frontier forward without prompting for any conflicts, restoring your original checkout afterward |
| `git-imerge diagram` | Show an ASCII (and optionally HTML) diagram of the merge's current state |
| `git-imerge simplify`/`finish` | Collapse the completed incremental merge into its final form on a real branch (`finish` also deletes the imerge's bookkeeping refs) |
| `git-imerge list` / `remove` | List / irrevocably delete in-progress incremental merges |
| `git-imerge reparent` | Standalone utility: change a commit's parents and propagate the change to HEAD |

Each subcommand supports the same `--name`, `--goal`, `--branch`,
`--manual`, and `--first-parent` options as the original (see `--help` on
any subcommand). Goals: `merge` (default), `rebase`, `rebase-with-history`,
`border`, `border-with-history`, `border-with-history2`, `full`, `drop`,
`revert` — see the original project's README for what each one produces.

## Architecture

The implementation is split the same way the algorithm naturally
decomposes:

- `git.rs` — a thin wrapper shelling out to the real `git` binary for every
  plumbing operation (`merge`, `commit-tree`, `update-ref`, `reparent` via
  raw object surgery, etc). Git-imerge's whole value is running many small
  *real* merges with *real* conflict detection, so this deliberately does
  not reimplement any of git's merge logic.
- `block.rs` — the 2D commit grid (`Grid`) and the rectangular-window
  abstraction (`Rect`) used to address regions of it, plus the core
  pairwise-merge primitives (`is_mergeable`, `auto_fill_micromerge`,
  `auto_outline`).
- `frontier.rs` — the three merge-frontier strategies (`full`, `manual`,
  and the default `blockwise` bisection-with-backtracking algorithm) that
  decide which cells are "done" and how to push forward.
- `state.rs` — `MergeState`: reads/writes the grid to
  `refs/imerge/<name>/*`, drives the auto-merge loop, and implements each
  `--goal`'s "simplify to a final result" strategy.
- `diagram.rs` — ASCII/HTML rendering.
- `cli.rs` / `commands.rs` — the `clap`-based CLI surface and one function
  per subcommand.

## Known deviations from the original

- **Errors** are always reported as a clean one-line message (no Python-style
  traceback distinction between "expected" and "internal" errors).
- The **HTML diagram** doesn't ship an inlined stylesheet; like the
  original, it references an external `imerge.css` you provide.
- The `writeppm` (PPM image) output was explicitly noted as
  experimental/likely-broken upstream and was not ported.
- Output assumes UTF-8 throughout rather than the original's
  `locale.getpreferredencoding()`.

## License

GNU General Public License, version 2 or later (matching the original
project) — see `COPYING`.
