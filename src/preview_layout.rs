//! Pure structural layout policy for the tree and preview surfaces.
//!
//! This module owns only frame chrome and structural regions. Content interiors and scrollbars are
//! deliberately left to the Presenter because they depend on the displayed content.

use crate::config::TreePosition;
use ratatui::layout::{Constraint, Layout, Rect};

/// Inputs to the responsive, structural layout policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutInput {
    pub area: Rect,
    pub has_prompt: bool,
    pub has_remote_status: bool,
    pub has_pin: bool,
    /// On a narrow frame, show the tree when true and the active preview when false.
    pub narrow_shows_tree: bool,
    pub tree_hidden: bool,
    pub preview_split_pct: u16,
    pub tree_split_pct: u16,
    pub tree_position: TreePosition,
    pub tree_max_cols: u16,
    pub tree_split_manual: bool,
}

impl LayoutInput {
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            has_prompt: false,
            has_remote_status: false,
            has_pin: false,
            narrow_shows_tree: true,
            tree_hidden: false,
            preview_split_pct: 50,
            tree_split_pct: 40,
            tree_position: TreePosition::Left,
            tree_max_cols: u16::MAX,
            tree_split_manual: false,
        }
    }

    pub fn with_prompt(mut self, has_prompt: bool) -> Self {
        self.has_prompt = has_prompt;
        self
    }

    pub fn with_remote_status(mut self, has_remote_status: bool) -> Self {
        self.has_remote_status = has_remote_status;
        self
    }

    pub fn with_pin(mut self, has_pin: bool) -> Self {
        self.has_pin = has_pin;
        self
    }
}

/// Structural regions for one frame. Divider regions mark a one-cell-wide boundary at the first
/// cell of the right-hand region; they are hit-test markers rather than additional drawable gaps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewLayout {
    pub body: Rect,
    pub prompt: Option<Rect>,
    pub remote_status: Option<Rect>,
    pub tree: Option<Rect>,
    pub pinned: Option<Rect>,
    pub active: Option<Rect>,
    pub tree_divider: Option<Rect>,
    pub preview_divider: Option<Rect>,
    /// A pin exists but its candidate region misses the 40-column interior floor. The tree and
    /// active regions deliberately match the no-pin layout; the Presenter uses this signal to
    /// add the persistent widen notice to the active projection.
    pub pinned_hidden_for_space: bool,
}

/// The existing fixed no-pin breakpoint. A pinned layout uses the structural preview floor below.
const NARROW_SPLIT: u16 = 80;
const PREVIEW_INTERIOR_FLOOR: u16 = 40;
const PREVIEW_BORDER_COLUMNS: u16 = 2;

/// The smallest tree percentage that can provide the configured minimum number of tree columns.
/// Re-exported by `presenter` for its existing controller callers.
pub fn min_tree_split_pct(pane_width: u16) -> u16 {
    if pane_width == 0 {
        return crate::config::MIN_TREE_MAX_COLS;
    }
    let pct = (crate::config::MIN_TREE_MAX_COLS as u32 * 100).div_ceil(pane_width as u32) as u16;
    pct.clamp(1, 40)
}

/// Carve every structural region from `input.area`, saturating safely for tiny frames.
pub fn layout(input: LayoutInput) -> PreviewLayout {
    let (body, remote_status, prompt) =
        carve_chrome(input.area, input.has_prompt, input.has_remote_status);
    let mut result = PreviewLayout {
        body,
        prompt,
        remote_status,
        ..PreviewLayout::default()
    };
    if body.width == 0 || body.height == 0 {
        return result;
    }

    if !input.has_pin {
        carve_unpinned(&mut result, input);
        return result;
    }
    let (tree, preview_area, tree_divider) = if input.tree_hidden {
        (None, body, None)
    } else {
        let (tree, preview, divider) = tree_and_preview(body, input);
        (Some(tree), preview, Some(divider))
    };
    let ratio = input.preview_split_pct.clamp(20, 80);
    // The session ratio continues to name the pinned share, now measured from the right edge.
    let previews = Layout::horizontal([
        Constraint::Percentage(100 - ratio),
        Constraint::Percentage(ratio),
    ])
    .split(preview_area);

    if preview_is_wide_enough(previews[0]) && preview_is_wide_enough(previews[1]) {
        result.tree = tree;
        result.active = Some(previews[0]);
        result.pinned = Some(previews[1]);
        result.tree_divider = tree_divider.map(|x| divider_region(body, x));
        result.preview_divider = Some(divider_region(body, previews[1].x));
    } else {
        // A hidden pin must never reflow or collapse the existing tree/active geometry. Reuse the
        // no-pin carve exactly, including its established 80-column fallback behaviour.
        carve_unpinned(&mut result, input);
        result.pinned_hidden_for_space = true;
    }
    result
}

fn carve_chrome(
    area: Rect,
    has_prompt: bool,
    has_remote_status: bool,
) -> (Rect, Option<Rect>, Option<Rect>) {
    let (above_prompt, prompt) = if has_prompt && area.height >= 2 {
        let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };
    if !has_remote_status || above_prompt.height < 2 {
        return (above_prompt, None, prompt);
    }
    let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(above_prompt);
    (parts[0], Some(parts[1]), prompt)
}

fn carve_unpinned(result: &mut PreviewLayout, input: LayoutInput) {
    if input.tree_hidden {
        result.active = Some(result.body);
        return;
    }
    if result.body.width < NARROW_SPLIT {
        if input.narrow_shows_tree {
            result.tree = Some(result.body);
        } else {
            result.active = Some(result.body);
        }
        return;
    }
    let (tree, active, divider) = tree_and_preview(result.body, input);
    result.tree = Some(tree);
    result.active = Some(active);
    result.tree_divider = Some(divider_region(result.body, divider));
}

fn tree_and_preview(area: Rect, input: LayoutInput) -> (Rect, Rect, u16) {
    let tree_pct = input
        .tree_split_pct
        .clamp(min_tree_split_pct(area.width), 90);
    let pct_cols = (area.width as u32 * tree_pct as u32 / 100) as u16;
    let cap_bites = !input.tree_split_manual && pct_cols > input.tree_max_cols;
    let (tree_constraint, preview_constraint) = if cap_bites {
        (Constraint::Length(input.tree_max_cols), Constraint::Min(0))
    } else {
        (
            Constraint::Percentage(tree_pct),
            Constraint::Percentage(100 - tree_pct),
        )
    };
    let columns = match input.tree_position {
        TreePosition::Left => Layout::horizontal([tree_constraint, preview_constraint]).split(area),
        TreePosition::Right => {
            Layout::horizontal([preview_constraint, tree_constraint]).split(area)
        }
    };
    match input.tree_position {
        TreePosition::Left => (columns[0], columns[1], columns[1].x),
        TreePosition::Right => (columns[1], columns[0], columns[1].x),
    }
}

fn preview_is_wide_enough(area: Rect) -> bool {
    area.width.saturating_sub(PREVIEW_BORDER_COLUMNS) >= PREVIEW_INTERIOR_FLOOR
}

fn divider_region(body: Rect, x: u16) -> Rect {
    Rect::new(x, body.y, 1, body.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_frames_suppress_chrome_and_regions_safely() {
        for (area, prompt, status) in [
            (Rect::new(0, 0, 0, 0), false, false),
            (Rect::new(0, 0, 1, 1), true, true),
            (Rect::new(0, 0, 1, 2), true, true),
        ] {
            let result = layout(
                LayoutInput::new(area)
                    .with_prompt(prompt)
                    .with_remote_status(status)
                    .with_pin(true),
            );
            if area.width == 0 || area.height == 0 {
                assert_eq!(result.tree, None);
                assert_eq!(result.pinned, None);
                assert_eq!(result.active, None);
            } else {
                for region in [result.tree, result.pinned, result.active] {
                    assert!(region.is_none_or(|rect| rect.x >= area.x && rect.y >= area.y));
                }
            }
        }
    }

    #[test]
    fn pin_present_tiny_bodies_match_the_no_pin_layout() {
        for area in [Rect::new(0, 0, 1, 1), Rect::new(0, 0, 1, 2)] {
            let result = layout(LayoutInput::new(area).with_pin(true));
            let without_pin = layout(LayoutInput::new(area));
            assert_eq!(result.tree, without_pin.tree, "tree at {area:?}");
            assert_eq!(result.active, without_pin.active, "active at {area:?}");
            assert_eq!(result.pinned, None, "pinned preview at {area:?}");
            assert!(result.pinned_hidden_for_space, "notice signal at {area:?}");
        }
    }

    #[test]
    fn pin_split_floor_hides_only_the_pin_and_keeps_no_pin_geometry() {
        for ratio in [20, 50, 80] {
            for narrow_shows_tree in [true, false] {
                let input = LayoutInput {
                    area: Rect::new(0, 0, 138, 10),
                    has_pin: true,
                    narrow_shows_tree,
                    preview_split_pct: ratio,
                    tree_split_pct: 40,
                    tree_max_cols: u16::MAX,
                    ..LayoutInput::new(Rect::default())
                };
                let result = layout(input);
                let without_pin = layout(LayoutInput {
                    has_pin: false,
                    ..input
                });
                assert_eq!(
                    result.tree, without_pin.tree,
                    "ratio {ratio}, narrow_shows_tree {narrow_shows_tree}"
                );
                assert_eq!(
                    result.active, without_pin.active,
                    "ratio {ratio}, narrow_shows_tree {narrow_shows_tree}"
                );
                assert_eq!(result.tree_divider, without_pin.tree_divider);
                assert_eq!(result.pinned, None, "ratio {ratio} hides the pin only");
                assert!(
                    result.pinned_hidden_for_space,
                    "ratio {ratio} signals the active notice"
                );
            }
        }
    }

    #[test]
    fn equal_preview_split_rounds_by_at_most_one_column() {
        for width in [84, 85] {
            let result = layout(LayoutInput {
                area: Rect::new(0, 0, width, 10),
                has_pin: true,
                tree_hidden: true,
                ..LayoutInput::new(Rect::default())
            });
            let pinned = result.pinned.unwrap();
            let active = result.active.unwrap();
            assert!(pinned.width.abs_diff(active.width) <= 1);
        }
    }

    #[test]
    fn preview_ratio_is_clamped_and_preserved_across_widths() {
        for (requested, expected) in [(0, 20), (20, 20), (50, 50), (80, 80), (100, 80)] {
            for width in [210, 237, 300] {
                let result = layout(LayoutInput {
                    area: Rect::new(0, 0, width, 10),
                    has_pin: true,
                    tree_hidden: true,
                    preview_split_pct: requested,
                    ..LayoutInput::new(Rect::default())
                });
                let pinned = result.pinned.unwrap();
                assert!(
                    (pinned.width as i32 - (width as i32 * expected / 100)).abs() <= 1,
                    "requested {requested}, width {width}"
                );
            }
        }
    }

    #[test]
    fn tree_side_only_changes_tree_placement_not_preview_order() {
        for (position, tree_is_left) in [(TreePosition::Left, true), (TreePosition::Right, false)] {
            let result = layout(LayoutInput {
                area: Rect::new(10, 5, 220, 12),
                has_pin: true,
                tree_position: position,
                ..LayoutInput::new(Rect::default())
            });
            let tree = result.tree.unwrap();
            let pinned = result.pinned.unwrap();
            let active = result.active.unwrap();
            assert_eq!(tree.x < pinned.x, tree_is_left);
            assert!(
                active.x < pinned.x,
                "active stays left of the pinned preview"
            );
            assert_eq!(result.tree_divider.unwrap().x, tree.x.max(active.x));
            assert_eq!(result.preview_divider.unwrap().x, pinned.x);
        }
    }
}
