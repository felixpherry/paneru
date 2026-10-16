//! How a row turns its column order into window frames.

use bevy::ecs::entity::Entity;

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
}
