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
/// column order; each leaf holds one member window of its column as an anchor.
#[expect(
    dead_code,
    reason = "built by `SplitTree::sync` once dwindle rows tile"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum SplitTree {
    /// One column's tile, anchored on one of its windows.
    Leaf(Entity),
    /// A tile cut in half — side by side when at least as wide as tall, else
    /// stacked; the first child takes the left or top half.
    Split(Box<SplitTree>, Box<SplitTree>),
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
}
