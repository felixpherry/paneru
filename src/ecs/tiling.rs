//! How a row turns its column order into window frames.

use bevy::ecs::entity::Entity;
use bevy::math::IRect;

use crate::manager::Size;
use crate::util::round_px;

/// How a row turns its column order into window frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Tiling {
    /// The horizontally scrolling strip of columns.
    #[default]
    Scrolling,
    /// Built from the column order by the first layout pass after entering.
    Dwindle(Option<SplitTree>),
    /// The master column's share of the viewport width, in (0, 1).
    Master(f64),
}

/// The master width every time a row enters master.
pub const DEFAULT_MASTER_RATIO: f64 = 0.5;

impl Tiling {
    /// The mode the toggle switches to: scrolling → dwindle → master →
    /// scrolling. Entering dwindle starts without a tree and entering master
    /// starts at [`DEFAULT_MASTER_RATIO`].
    pub fn next(&self) -> Tiling {
        match self {
            Tiling::Scrolling => Tiling::Dwindle(None),
            Tiling::Dwindle(_) => Tiling::Master(DEFAULT_MASTER_RATIO),
            Tiling::Master(_) => Tiling::Scrolling,
        }
    }
}

/// Master tiles over a `size` viewport at origin (0, 0), one per column in
/// column order: the first column on the left at `ratio` of the width, every
/// other column stacked on the right at equal heights, the last taking the
/// rounding remainder.
pub fn master_tiles(count: usize, size: Size, ratio: f64) -> Vec<IRect> {
    if count < 2 {
        return vec![IRect::new(0, 0, size.x, size.y); count];
    }
    let master = round_px(ratio * f64::from(size.x));
    let stacked = i32::try_from(count - 1).unwrap_or(i32::MAX);
    let height = size.y / stacked;
    std::iter::once(IRect::new(0, 0, master, size.y))
        .chain((0..stacked).map(|i| {
            let bottom = if i + 1 == stacked {
                size.y
            } else {
                (i + 1) * height
            };
            IRect::new(master, i * height, size.x, bottom)
        }))
        .collect()
}

/// A dwindle row's split tree. An in-order walk of its leaves is the row's
/// column order; each leaf holds its column's windows as of the last sync.
#[derive(Clone, Debug, PartialEq)]
pub enum SplitTree {
    /// One column's tile and that column's windows.
    Leaf(Vec<Entity>),
    /// A tile cut in half — side by side when at least as wide as tall, else
    /// stacked; the first child takes the left or top half.
    Split(Box<SplitTree>, Box<SplitTree>),
}

impl SplitTree {
    /// The leaves' windows, in order.
    pub fn leaves(&self) -> Vec<&[Entity]> {
        match self {
            SplitTree::Leaf(windows) => vec![windows.as_slice()],
            SplitTree::Split(first, second) => [first.leaves(), second.leaves()].concat(),
        }
    }

    /// The leaves' tiles over `rect`, in order. A split halves its rect side
    /// by side when it is at least as wide as tall, else stacked, and its
    /// first child takes the left or top half.
    pub fn tiles(&self, rect: IRect) -> Vec<IRect> {
        let SplitTree::Split(first, second) = self else {
            return vec![rect];
        };
        let (head, tail) = if rect.width() >= rect.height() {
            let x = rect.min.x + rect.width() / 2;
            (
                IRect {
                    max: rect.max.with_x(x),
                    ..rect
                },
                IRect {
                    min: rect.min.with_x(x),
                    ..rect
                },
            )
        } else {
            let y = rect.min.y + rect.height() / 2;
            (
                IRect {
                    max: rect.max.with_y(y),
                    ..rect
                },
                IRect {
                    min: rect.min.with_y(y),
                    ..rect
                },
            )
        };
        [first.tiles(head), second.tiles(tail)].concat()
    }

    /// Whether the tree matches `members` (column `i`'s windows at index `i`):
    /// one leaf per column, leaf `i` holding column `i`'s windows in any order.
    pub fn in_sync(&self, members: &[Vec<Entity>]) -> bool {
        let leaves = self.leaves();
        leaves.len() == members.len()
            && leaves.iter().zip(members).all(|(leaf, column)| {
                leaf.len() == column.len() && column.iter().all(|window| leaf.contains(window))
            })
    }

    /// Brings `tree` in line with a row's columns (`members[i]` = column `i`'s
    /// windows). A tree already in sync comes back unchanged; `sync(None, …)`
    /// builds a fresh spiral.
    pub fn sync(tree: Option<SplitTree>, members: &[Vec<Entity>]) -> Option<SplitTree> {
        if tree.as_ref().is_some_and(|tree| tree.in_sync(members)) {
            return tree;
        }
        // Each leaf claims the first unclaimed column holding one of its
        // windows. A stack split in two (unstack) keeps its leaf on the first
        // piece, and the second piece is a new column right after it, so it
        // splits the stack's own tile. A leaf with no such column (its windows
        // left the row, or were stacked into a column an earlier leaf claimed)
        // goes.
        let mut claimed = vec![false; members.len()];
        let mut tree = tree.and_then(|tree| {
            tree.filter_map_leaves(&mut |windows| {
                let index = members.iter().zip(&claimed).position(|(column, &taken)| {
                    !taken && column.iter().any(|window| windows.contains(window))
                })?;
                claimed[index] = true;
                Some(members[index].clone())
            })
        });
        // Each new column splits the tile of the column before it (column 0
        // splits the first tile).
        for (index, column) in members.iter().enumerate() {
            if claimed[index] || column.is_empty() {
                continue;
            }
            let before = index
                .checked_sub(1)
                .and_then(|before| members[before].first().copied());
            tree = Some(match tree {
                Some(mut tree) => {
                    tree.split(before, column);
                    tree
                }
                None => SplitTree::Leaf(column.clone()),
            });
        }
        // Leaf k takes column k's windows, so a new column 0 gets the first half
        // of the tile it split, and two exchanged columns trade tiles.
        let mut columns = members.iter().filter(|column| !column.is_empty()).cloned();
        tree.and_then(|tree| tree.filter_map_leaves(&mut |_| columns.next()))
    }

    /// The tree with each leaf's windows replaced through `windows`. A leaf it
    /// maps to `None` goes, and its sibling takes the parent's place.
    fn filter_map_leaves<F>(self, windows: &mut F) -> Option<SplitTree>
    where
        F: FnMut(Vec<Entity>) -> Option<Vec<Entity>>,
    {
        match self {
            SplitTree::Leaf(leaf) => windows(leaf).map(SplitTree::Leaf),
            SplitTree::Split(first, second) => {
                match (
                    first.filter_map_leaves(windows),
                    second.filter_map_leaves(windows),
                ) {
                    (Some(first), Some(second)) => {
                        Some(SplitTree::Split(Box::new(first), Box::new(second)))
                    }
                    (first, second) => first.or(second),
                }
            }
        }
    }

    /// Splits the leaf holding `at` (the first leaf when `None`) into it and a
    /// new leaf holding `windows`. Returns whether it found the leaf.
    fn split(&mut self, at: Option<Entity>, windows: &[Entity]) -> bool {
        match self {
            SplitTree::Leaf(leaf) if at.is_none_or(|at| leaf.contains(&at)) => {
                *self = SplitTree::Split(
                    Box::new(SplitTree::Leaf(std::mem::take(leaf))),
                    Box::new(SplitTree::Leaf(windows.to_vec())),
                );
                true
            }
            SplitTree::Leaf(_) => false,
            SplitTree::Split(first, second) => {
                first.split(at, windows) || second.split(at, windows)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_cycles_scrolling_dwindle_master() {
        let dwindle = Tiling::Scrolling.next();
        assert_eq!(dwindle, Tiling::Dwindle(None));
        let master = dwindle.next();
        assert_eq!(master, Tiling::Master(DEFAULT_MASTER_RATIO));
        assert_eq!(master.next(), Tiling::Scrolling);
    }

    #[test]
    fn master_tiles_split_the_viewport() {
        let size = Size::new(1024, 748);
        assert!(master_tiles(0, size, 0.5).is_empty());
        assert_eq!(master_tiles(1, size, 0.5), [IRect::new(0, 0, 1024, 748)]);
        assert_eq!(
            master_tiles(2, size, 0.5),
            [IRect::new(0, 0, 512, 748), IRect::new(512, 0, 1024, 748)]
        );
        assert_eq!(
            master_tiles(4, size, 0.5),
            [
                IRect::new(0, 0, 512, 748),
                IRect::new(512, 0, 1024, 249),
                IRect::new(512, 249, 1024, 498),
                IRect::new(512, 498, 1024, 748),
            ]
        );
        // A ⅔ master: round_px(0.66667 × 1024) = 683.
        assert_eq!(
            master_tiles(2, size, 0.66667),
            [IRect::new(0, 0, 683, 748), IRect::new(683, 0, 1024, 748)]
        );
    }

    use bevy::ecs::world::World;

    /// `N` distinct window entities.
    fn spawn<const N: usize>() -> [Entity; N] {
        let mut world = World::new();
        std::array::from_fn(|_| world.spawn_empty().id())
    }

    /// One single-window column per entity.
    fn singles(entities: &[Entity]) -> Vec<Vec<Entity>> {
        entities.iter().map(|&entity| vec![entity]).collect()
    }

    /// A tree's tiles over the 1024×748 test viewport; none without a tree.
    fn tiles(tree: Option<&SplitTree>) -> Vec<IRect> {
        tree.map_or_else(Vec::new, |tree| tree.tiles(IRect::new(0, 0, 1024, 748)))
    }

    /// Three columns built fresh: the first left, the other two stacked right.
    fn three_tiles() -> [IRect; 3] {
        [
            IRect::new(0, 0, 512, 748),
            IRect::new(512, 0, 1024, 374),
            IRect::new(512, 374, 1024, 748),
        ]
    }

    #[test]
    fn fresh_tree_spirals_from_the_column_order() {
        let [a, b, c] = spawn();
        let tree = SplitTree::sync(None, &singles(&[a, b, c]));
        assert_eq!(tiles(tree.as_ref()), three_tiles());
        assert_eq!(tree.expect("a tree").leaves(), singles(&[a, b, c]));
    }

    #[test]
    fn new_column_splits_the_tile_before_it() {
        let [a, b, c, new, extra] = spawn();
        let tree = SplitTree::sync(None, &singles(&[a, b, c]));
        // A's tile is taller than wide: the new column takes its bottom half,
        // and B and C keep theirs.
        let a_split = [
            IRect::new(0, 0, 512, 374),
            IRect::new(0, 374, 512, 748),
            IRect::new(512, 0, 1024, 374),
            IRect::new(512, 374, 1024, 748),
        ];
        let after_a = SplitTree::sync(tree.clone(), &singles(&[a, new, b, c]));
        assert_eq!(tiles(after_a.as_ref()), a_split);
        // At column 0 it splits the first tile and takes the top half.
        let first = SplitTree::sync(tree, &singles(&[new, a, b, c]));
        assert_eq!(tiles(first.as_ref()), a_split);
        assert_eq!(first.expect("a tree").leaves(), singles(&[new, a, b, c]));
        // Two new columns in front of the only tile both get one.
        let front = SplitTree::sync(Some(SplitTree::Leaf(vec![c])), &singles(&[new, extra, c]));
        assert_eq!(front.expect("a tree").leaves(), singles(&[new, extra, c]));
    }

    #[test]
    fn gone_column_gives_its_tile_to_its_sibling() {
        let [a, b, c, new] = spawn();
        let tree = SplitTree::sync(None, &singles(&[a, b, c]));
        let four = SplitTree::sync(tree, &singles(&[a, new, b, c]));
        // The middle column leaves: A grows back, the first layout again.
        let middle_gone = SplitTree::sync(four.clone(), &singles(&[a, b, c]));
        assert_eq!(tiles(middle_gone.as_ref()), three_tiles());
        // The last column leaves: its sibling B takes the whole right half.
        let last_gone = SplitTree::sync(four, &singles(&[a, new, b]));
        assert_eq!(
            tiles(last_gone.as_ref()),
            [
                IRect::new(0, 0, 512, 374),
                IRect::new(0, 374, 512, 748),
                IRect::new(512, 0, 1024, 748),
            ]
        );
        // The only column leaves: no tree.
        assert_eq!(SplitTree::sync(Some(SplitTree::Leaf(vec![a])), &[]), None);
    }

    #[test]
    fn exchanged_columns_trade_tiles() {
        let [a, b, c] = spawn();
        let tree = SplitTree::sync(None, &singles(&[a, b, c]));
        let swapped = SplitTree::sync(tree, &singles(&[b, a, c]));
        assert_eq!(tiles(swapped.as_ref()), three_tiles());
        assert_eq!(swapped.expect("a tree").leaves(), singles(&[b, a, c]));
    }

    #[test]
    fn stack_drops_a_leaf_and_unstack_splits_the_tile() {
        let [a, b, c, new] = spawn();
        let tree = SplitTree::sync(None, &singles(&[a, b, c]));
        // The stacked column's own leaf goes, not the last one: New split A's
        // tile, and stacking it under A gives back the first layout.
        let four = SplitTree::sync(tree.clone(), &singles(&[a, new, b, c]));
        let merged = SplitTree::sync(four, &[vec![a, new], vec![b], vec![c]]);
        assert_eq!(tiles(merged.as_ref()), three_tiles());
        // B stacked under A: B's leaf goes and C takes the whole right half.
        let stacked = SplitTree::sync(tree, &[vec![a, b], vec![c]]);
        assert_eq!(
            tiles(stacked.as_ref()),
            [IRect::new(0, 0, 512, 748), IRect::new(512, 0, 1024, 748)]
        );
        // B unstacked again: it splits the stack's tile, and C keeps its own.
        let unstacked = SplitTree::sync(stacked, &singles(&[a, b, c]));
        assert_eq!(
            tiles(unstacked.as_ref()),
            [
                IRect::new(0, 0, 512, 374),
                IRect::new(0, 374, 512, 748),
                IRect::new(512, 0, 1024, 748),
            ]
        );
    }

    #[test]
    fn sync_is_idempotent_and_matches_by_membership() {
        let [a, b, c] = spawn();
        for members in [
            singles(&[a, b, c]),
            vec![vec![a, b], vec![c]],
            vec![vec![c], vec![b, a]],
        ] {
            let tree = SplitTree::sync(None, &members).expect("a tree");
            assert!(tree.in_sync(&members));
            assert_eq!(SplitTree::sync(Some(tree.clone()), &members), Some(tree));
        }
        // Order inside a column doesn't count: a stack whose top changed (a
        // focused tab moves to the front) is still in sync. A window moving to
        // another column is not, even with the column count unchanged.
        let tree = SplitTree::sync(None, &[vec![a, b], vec![c]]).expect("a tree");
        assert!(tree.in_sync(&[vec![b, a], vec![c]]));
        assert!(!tree.in_sync(&[vec![a], vec![b, c]]));
    }

    #[test]
    fn unstacking_either_window_splits_the_stacks_own_tile() {
        let [a, b, c] = spawn();
        // B and C stacked right of A: the stack takes the right half.
        let stacked = SplitTree::sync(None, &[vec![a], vec![b, c]]);
        assert_eq!(
            tiles(stacked.as_ref()),
            [IRect::new(0, 0, 512, 748), IRect::new(512, 0, 1024, 748)]
        );
        // Unstack puts the rest of the stack first and the unstacked window
        // after it. Either way the stack's tile splits and A keeps its half.
        let bottom = SplitTree::sync(stacked.clone(), &singles(&[a, b, c]));
        assert_eq!(tiles(bottom.as_ref()), three_tiles(), "C unstacked");
        let top = SplitTree::sync(stacked, &singles(&[a, c, b]));
        assert_eq!(tiles(top.as_ref()), three_tiles(), "B unstacked");
    }
}
