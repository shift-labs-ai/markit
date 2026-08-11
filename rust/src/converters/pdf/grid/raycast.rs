//! Ray casting from a text-box center to surrounding table borders.
//! The consumer needs only directional presence, represented as four
//! bits instead of allocated hit records and cloned segment IDs.

use crate::converters::pdf::types::{Segment, TextBox};

use super::AXIS_EPSILON;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct BoundaryHits(u8);

impl BoundaryHits {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;

    pub(super) fn count(self) -> usize {
        self.0.count_ones() as usize
    }
}

pub(super) fn cast(text_box: &TextBox, segments: &[Segment]) -> BoundaryHits {
    let cx = (text_box.bounds.left + text_box.bounds.right) / 2.0;
    let cy = (text_box.bounds.top + text_box.bounds.bottom) / 2.0;
    let mut hits = 0u8;
    let (mut up, mut down, mut left, mut right) =
        (f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::INFINITY);

    for seg in segments {
        let is_h = (seg.y1 - seg.y2).abs() <= AXIS_EPSILON;
        let is_v = (seg.x1 - seg.x2).abs() <= AXIS_EPSILON;

        if is_h {
            let min_x = seg.x1.min(seg.x2);
            let max_x = seg.x1.max(seg.x2);
            if cx >= min_x && cx <= max_x {
                let d = seg.y1 - cy;
                if d >= 0.0 && d < up {
                    up = d;
                    hits |= BoundaryHits::UP;
                }
                let d = cy - seg.y1;
                if d >= 0.0 && d < down {
                    down = d;
                    hits |= BoundaryHits::DOWN;
                }
            }
        }

        if is_v {
            let min_y = seg.y1.min(seg.y2);
            let max_y = seg.y1.max(seg.y2);
            if cy >= min_y && cy <= max_y {
                let d = cx - seg.x1;
                if d >= 0.0 && d < left {
                    left = d;
                    hits |= BoundaryHits::LEFT;
                }
                let d = seg.x1 - cx;
                if d >= 0.0 && d < right {
                    right = d;
                    hits |= BoundaryHits::RIGHT;
                }
            }
        }
    }

    BoundaryHits(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converters::pdf::types::Bounds;

    fn text_box() -> TextBox {
        TextBox {
            id: "t".into(),
            text: "x".into(),
            page_number: 1,
            bounds: Bounds {
                left: 4.0,
                right: 6.0,
                bottom: 4.0,
                top: 6.0,
            },
            font_size: 10.0,
            is_bold: false,
        }
    }

    fn seg(id: &str, x1: f64, y1: f64, x2: f64, y2: f64) -> Segment {
        Segment {
            id: id.into(),
            x1,
            y1,
            x2,
            y2,
        }
    }

    #[test]
    fn four_surrounding_borders_hit_all_directions() {
        let lines = [
            seg("up", 0.0, 10.0, 10.0, 10.0),
            seg("down", 0.0, 0.0, 10.0, 0.0),
            seg("left", 0.0, 0.0, 0.0, 10.0),
            seg("right", 10.0, 0.0, 10.0, 10.0),
        ];
        assert_eq!(cast(&text_box(), &lines).count(), 4);
    }

    #[test]
    fn non_intersecting_and_diagonal_lines_do_not_hit() {
        let lines = [
            seg("outside-h", 20.0, 10.0, 30.0, 10.0),
            seg("outside-v", 10.0, 20.0, 10.0, 30.0),
            seg("diagonal", 0.0, 0.0, 10.0, 10.0),
        ];
        assert_eq!(cast(&text_box(), &lines).count(), 0);
    }

    #[test]
    fn empty_segment_slice_has_no_hits() {
        assert_eq!(cast(&text_box(), &[]).count(), 0);
    }

    #[test]
    fn center_on_line_hits_both_directions() {
        let lines = [
            seg("horizontal", 0.0, 5.0, 10.0, 5.0),
            seg("vertical", 5.0, 0.0, 5.0, 10.0),
        ];
        // Zero distance is intentionally a hit in each direction,
        // preserving the original ray semantics.
        assert_eq!(cast(&text_box(), &lines).count(), 4);
    }

    #[test]
    fn shared_axis_epsilon_accepts_nearly_vertical_segment() {
        let lines = [seg("near-v", 10.0, 0.0, 10.6, 10.0)];
        assert_eq!(cast(&text_box(), &lines).count(), 1);
    }

    #[test]
    fn multiple_lines_in_one_direction_count_once() {
        let lines = [
            seg("up-near", 0.0, 7.0, 10.0, 7.0),
            seg("up-far", 0.0, 10.0, 10.0, 10.0),
        ];
        assert_eq!(cast(&text_box(), &lines).count(), 1);
    }
}
