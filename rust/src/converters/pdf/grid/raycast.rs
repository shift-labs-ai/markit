//! Ray casting from a text-box center to surrounding table borders.
//! The consumer needs only directional presence, represented as four
//! bits instead of allocated hit records and cloned segment IDs.

use crate::converters::pdf::types::{Segment, TextBox};

use super::AXIS_EPSILON;

/// Retained as the reference implementation the RayIndex fast path is
/// tested against; production code queries the index.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct BoundaryHits(u8);

#[cfg(test)]
impl BoundaryHits {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;

    pub(super) fn count(self) -> usize {
        self.0.count_ones() as usize
    }
}

/// Ray-hit index: the merged x-extents of horizontal segments and
/// y-extents of vertical segments. A box has a ray hit exactly when
/// its center x lies inside some horizontal segment's span (that
/// segment is then above or below it) or its center y inside some
/// vertical segment's span. Presence is all the cell placement path
/// needs, and segment lists run to the thousands on vector-heavy
/// pages — this makes the per-box query O(log n).
pub(super) struct RayIndex {
    h_x_spans: Vec<(f64, f64)>,
    v_y_spans: Vec<(f64, f64)>,
}

fn merge_spans(mut spans: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn spans_contain(spans: &[(f64, f64)], p: f64) -> bool {
    let idx = spans.partition_point(|(start, _)| *start <= p);
    idx > 0 && spans[idx - 1].1 >= p
}

impl RayIndex {
    pub(super) fn new(segments: &[Segment]) -> RayIndex {
        let mut h_x_spans = Vec::new();
        let mut v_y_spans = Vec::new();
        for seg in segments {
            if (seg.y1 - seg.y2).abs() <= AXIS_EPSILON {
                h_x_spans.push((seg.x1.min(seg.x2), seg.x1.max(seg.x2)));
            }
            if (seg.x1 - seg.x2).abs() <= AXIS_EPSILON {
                v_y_spans.push((seg.y1.min(seg.y2), seg.y1.max(seg.y2)));
            }
        }
        RayIndex {
            h_x_spans: merge_spans(h_x_spans),
            v_y_spans: merge_spans(v_y_spans),
        }
    }

    pub(super) fn any_hit(&self, text_box: &TextBox) -> bool {
        let cx = (text_box.bounds.left + text_box.bounds.right) / 2.0;
        let cy = (text_box.bounds.top + text_box.bounds.bottom) / 2.0;
        spans_contain(&self.h_x_spans, cx) || spans_contain(&self.v_y_spans, cy)
    }
}

#[cfg(test)]
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

    /// The O(log n) index must agree with the reference cast on hit
    /// presence for every segment arrangement above.
    #[test]
    fn ray_index_agrees_with_reference_cast() {
        let arrangements: Vec<Vec<Segment>> = vec![
            vec![
                seg("up", 0.0, 10.0, 10.0, 10.0),
                seg("down", 0.0, 0.0, 10.0, 0.0),
            ],
            vec![
                seg("outside-h", 20.0, 10.0, 30.0, 10.0),
                seg("outside-v", 10.0, 20.0, 10.0, 30.0),
                seg("diagonal", 0.0, 0.0, 10.0, 10.0),
            ],
            vec![],
            vec![seg("near-v", 10.0, 0.0, 10.6, 10.0)],
            vec![seg("horizontal", 0.0, 5.0, 10.0, 5.0)],
        ];
        for segments in &arrangements {
            let reference = cast(&text_box(), segments).count() > 0;
            let fast = RayIndex::new(segments).any_hit(&text_box());
            assert_eq!(reference, fast, "{segments:?}");
        }
    }
}
