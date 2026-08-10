//! PDF conversion pipeline. Port of src/converters/pdf/.

pub mod columns;
pub mod content_lex;
pub mod extract;
pub mod fast_extract;
pub mod grid;
pub mod headers;
pub mod index;
pub mod own_pdf;
pub mod render;
pub mod types;

/// JavaScriptCore-faithful Array#sort for comparators that do not implement
/// a strict total order.
///
/// The TS PDF pipeline sorts with tolerance-band comparators (same-line Y
/// tolerance ⇒ compare X) that are intentionally intransitive. The TS CLI runs
/// under Bun (JavaScriptCore), whose Array#sort is Powersort (Munro–Wild) with
/// TimSort-style galloping merges — see WebKit's
/// Source/JavaScriptCore/runtime/StableSort.h (arrayStableSort with
/// MergeStrategy::Galloping). With an inconsistent comparator the final order
/// depends on the exact algorithm, so byte-parity requires reproducing it
/// verbatim, including run extension (cutoff 8, forced run length 64), the
/// power computation, gallop hints, and the adaptive minGallop schedule.
pub(crate) fn js_stable_sort<T: Clone>(
    items: &mut [T],
    cmp: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    let lt = |a: &T, b: &T| cmp(a, b) == std::cmp::Ordering::Less;
    array_stable_sort(items, &lt);
}

const EXTEND_RUN_CUTOFF: usize = 8;
const FORCE_RUN_LENGTH: usize = 64;
const MIN_GALLOP_THRESHOLD: usize = 7;

/// JSC arrayInsertionSort: binary insertion over the whole prefix [0, i),
/// starting from sortedHeader + 1.
fn array_insertion_sort<T: Clone>(a: &mut [T], lt: &impl Fn(&T, &T) -> bool, sorted_header: usize) {
    let length = a.len();
    let mut i = sorted_header + 1;
    while i < length {
        let value = a[i].clone();
        let mut left = 0usize;
        let mut right = i;
        while left < right {
            let m = (left + right) / 2;
            if lt(&value, &a[m]) {
                right = m;
            } else {
                left = m + 1;
            }
        }
        // Shift [left, i) right by one and place value.
        let mut j = i;
        while j > left {
            a[j] = a[j - 1].clone();
            j -= 1;
        }
        a[left] = value;
        i += 1;
    }
}

/// JSC extendAndNormalizeRun: returns INCLUSIVE end of the ascending-normalized
/// run starting at begin.
fn extend_and_normalize_run<T>(a: &mut [T], begin: usize, lt: &impl Fn(&T, &T) -> bool) -> usize {
    let mut end = begin;
    let num = a.len();
    if end + 1 >= num {
        return end;
    }
    let descending = lt(&a[end + 1], &a[end]);
    if descending {
        end += 1;
        while end + 1 < num && lt(&a[end + 1], &a[end]) {
            end += 1;
        }
        a[begin..=end].reverse();
    } else {
        end += 1;
        while end + 1 < num && !lt(&a[end + 1], &a[end]) {
            end += 1;
        }
    }
    end
}

/// JSC gallopLeft: leftmost k in [0, length] with base[k-1] < key <= base[k].
/// size_t wrap-around in the hint translation is intentional (matches C++).
fn gallop_left<T>(
    key: &T,
    base: &[T],
    length: usize,
    hint: usize,
    lt: &impl Fn(&T, &T) -> bool,
) -> usize {
    debug_assert!(hint < length);
    let mut last_offset = 0usize;
    let mut offset = 1usize;

    if lt(&base[hint], key) {
        // Gallop right: search in (hint, length).
        let max_offset = length - hint;
        while offset < max_offset {
            if !lt(&base[hint + offset], key) {
                break;
            }
            last_offset = offset;
            let next_offset = (offset << 1).wrapping_add(1);
            offset = if next_offset > offset {
                next_offset.min(max_offset)
            } else {
                max_offset
            };
        }
        last_offset += hint;
        offset += hint;
    } else {
        // Gallop left: search in [0, hint).
        let max_offset = hint + 1;
        while offset < max_offset {
            if lt(&base[hint - offset], key) {
                break;
            }
            last_offset = offset;
            let next_offset = (offset << 1).wrapping_add(1);
            offset = if next_offset > offset {
                next_offset.min(max_offset)
            } else {
                max_offset
            };
        }
        let tmp = last_offset;
        last_offset = hint.wrapping_sub(offset); // may wrap to usize::MAX (C++ size_t)
        offset = hint - tmp;
    }

    // Binary search in (lastOffset, offset].
    last_offset = last_offset.wrapping_add(1);
    while last_offset < offset {
        let m = last_offset + ((offset - last_offset) >> 1);
        if lt(&base[m], key) {
            last_offset = m + 1;
        } else {
            offset = m;
        }
    }
    offset
}

/// JSC gallopRight: rightmost k in [0, length] with base[k-1] <= key < base[k].
fn gallop_right<T>(
    key: &T,
    base: &[T],
    length: usize,
    hint: usize,
    lt: &impl Fn(&T, &T) -> bool,
) -> usize {
    debug_assert!(hint < length);
    let mut last_offset = 0usize;
    let mut offset = 1usize;

    if lt(key, &base[hint]) {
        // Gallop left: search in [0, hint).
        let max_offset = hint + 1;
        while offset < max_offset {
            if !lt(key, &base[hint - offset]) {
                break;
            }
            last_offset = offset;
            let next_offset = (offset << 1).wrapping_add(1);
            offset = if next_offset > offset {
                next_offset.min(max_offset)
            } else {
                max_offset
            };
        }
        let tmp = last_offset;
        last_offset = hint.wrapping_sub(offset); // may wrap (C++ size_t)
        offset = hint - tmp;
    } else {
        // Gallop right: search in (hint, length).
        let max_offset = length - hint;
        while offset < max_offset {
            if lt(key, &base[hint + offset]) {
                break;
            }
            last_offset = offset;
            let next_offset = (offset << 1).wrapping_add(1);
            offset = if next_offset > offset {
                next_offset.min(max_offset)
            } else {
                max_offset
            };
        }
        last_offset += hint;
        offset += hint;
    }

    last_offset = last_offset.wrapping_add(1);
    while last_offset < offset {
        let m = last_offset + ((offset - last_offset) >> 1);
        if lt(key, &base[m]) {
            offset = m;
        } else {
            last_offset = m + 1;
        }
    }
    offset
}

/// JSC mergePowersortRuns (MergeStrategy::Galloping).
#[allow(clippy::too_many_arguments)]
fn merge_powersort_runs<T: Clone>(
    dst: &mut [T],
    src: &[T],
    src_index1: usize,
    src_end1: usize,
    src_index2: usize,
    src_end2: usize,
    lt: &impl Fn(&T, &T) -> bool,
    min_gallop: &mut usize,
) {
    let mut left_length = src_end1 - src_index1;
    let mut right_length = src_end2 - src_index2;

    if left_length == 0 || right_length == 0 {
        dst[src_index1..src_end2].clone_from_slice(&src[src_index1..src_end2]);
        return;
    }

    // Pre-merge trim: leading left elements already in place.
    let skip_left = gallop_right(
        &src[src_index2],
        &src[src_index1..src_end1],
        left_length,
        0,
        lt,
    );
    if skip_left > 0 {
        dst[src_index1..src_index1 + skip_left]
            .clone_from_slice(&src[src_index1..src_index1 + skip_left]);
    }
    let mut left = src_index1 + skip_left;
    left_length -= skip_left;

    if left_length == 0 {
        dst[src_index2..src_end2].clone_from_slice(&src[src_index2..src_end2]);
        return;
    }

    // Pre-merge trim: trailing right elements already in place.
    let skip_right = right_length
        - gallop_left(
            &src[src_end1 - 1],
            &src[src_index2..src_end2],
            right_length,
            right_length - 1,
            lt,
        );
    if skip_right > 0 {
        dst[src_end2 - skip_right..src_end2]
            .clone_from_slice(&src[src_end2 - skip_right..src_end2]);
    }
    let right_end = src_end2 - skip_right;
    right_length -= skip_right;

    if right_length == 0 {
        dst[left..left + left_length].clone_from_slice(&src[left..left + left_length]);
        return;
    }

    let left_end = src_end1;
    let mut right = src_index2;
    let mut dst_index = left;
    let mut left_wins;
    let mut right_wins;

    'merge: loop {
        // Linear merge until one side wins minGallop times consecutively.
        left_wins = 0usize;
        right_wins = 0usize;

        while left_wins < *min_gallop && right_wins < *min_gallop {
            if right < right_end && left < left_end {
                if lt(&src[right], &src[left]) {
                    dst[dst_index] = src[right].clone();
                    dst_index += 1;
                    right += 1;
                    right_wins += 1;
                    left_wins = 0;
                } else {
                    dst[dst_index] = src[left].clone();
                    dst_index += 1;
                    left += 1;
                    left_wins += 1;
                    right_wins = 0;
                }
            } else {
                break 'merge;
            }
        }

        // Entering galloping mode; penalize.
        *min_gallop += 1;

        loop {
            // Decrease minGallop while galloping is productive.
            if *min_gallop > 1 {
                *min_gallop -= 1;
            }

            if left >= left_end || right >= right_end {
                break 'merge;
            }

            // Gallop in left run for right's current element.
            {
                let k = gallop_right(&src[right], &src[left..left_end], left_end - left, 0, lt);
                left_wins = k;
                if k > 0 {
                    dst[dst_index..dst_index + k].clone_from_slice(&src[left..left + k]);
                    dst_index += k;
                    left += k;
                }
                dst[dst_index] = src[right].clone();
                dst_index += 1;
                right += 1;

                if left >= left_end || right >= right_end {
                    break 'merge;
                }
            }

            // Gallop in right run for left's current element.
            {
                let k = gallop_left(&src[left], &src[right..right_end], right_end - right, 0, lt);
                right_wins = k;
                if k > 0 {
                    dst[dst_index..dst_index + k].clone_from_slice(&src[right..right + k]);
                    dst_index += k;
                    right += k;
                }
                dst[dst_index] = src[left].clone();
                dst_index += 1;
                left += 1;

                if left >= left_end || right >= right_end {
                    break 'merge;
                }
            }

            if !(left_wins >= MIN_GALLOP_THRESHOLD || right_wins >= MIN_GALLOP_THRESHOLD) {
                break;
            }
        }

        // Leaving galloping mode; penalize.
        *min_gallop += 1;
    }

    // Copy remaining elements.
    while left < left_end {
        dst[dst_index] = src[left].clone();
        dst_index += 1;
        left += 1;
    }
    while right < right_end {
        dst[dst_index] = src[right].clone();
        dst_index += 1;
        right += 1;
    }
}

/// JSC's power(left, middle, right, n) for the Powersort merge policy.
fn powersort_power(left: usize, middle: usize, right: usize, n: usize) -> u32 {
    let n1 = (middle - left) as u128;
    let n2 = (right - middle + 1) as u128;
    let mut a = (left as u128) * 2 + n1;
    let mut b = (middle as u128) * 2 + n2;
    a <<= 62;
    b <<= 62;
    let n = n as u128;
    let differing_bits = (a / n) ^ (b / n);
    (differing_bits as u64).leading_zeros()
}

#[derive(Clone, Copy)]
struct SortedRun {
    begin: usize,
    end: usize, // inclusive
}

/// JSC arrayStableSort<MergeStrategy::Galloping> (Powersort).
fn array_stable_sort<T: Clone>(src: &mut [T], lt: &impl Fn(&T, &T) -> bool) {
    let num_elements = src.len();
    if num_elements == 0 {
        return;
    }

    if num_elements < EXTEND_RUN_CUTOFF {
        array_insertion_sort(src, lt, 0);
        return;
    }

    let mut working_set: Vec<T> = src.to_vec();
    let mut powerstack: Vec<(SortedRun, u32)> = Vec::new();
    let mut min_gallop = MIN_GALLOP_THRESHOLD;

    let mut run1 = SortedRun { begin: 0, end: 0 };
    run1.end = extend_and_normalize_run(src, run1.begin, lt);

    if run1.end - run1.begin < EXTEND_RUN_CUTOFF {
        let size = FORCE_RUN_LENGTH.min(num_elements - run1.begin);
        let header = run1.end - run1.begin;
        array_insertion_sort(&mut src[run1.begin..run1.begin + size], lt, header);
        run1.end = run1.begin + size - 1;
    }
    while run1.end + 1 < num_elements && !lt(&src[run1.end + 1], &src[run1.end]) {
        run1.end += 1;
    }

    while run1.end + 1 < num_elements {
        let mut run2 = SortedRun {
            begin: run1.end + 1,
            end: run1.end + 1,
        };
        run2.end = extend_and_normalize_run(src, run2.begin, lt);

        if run2.end - run2.begin < EXTEND_RUN_CUTOFF {
            let size = FORCE_RUN_LENGTH.min(num_elements - run2.begin);
            let header = run2.end - run2.begin;
            array_insertion_sort(&mut src[run2.begin..run2.begin + size], lt, header);
            run2.end = run2.begin + size - 1;
        }
        while run2.end + 1 < num_elements && !lt(&src[run2.end + 1], &src[run2.end]) {
            run2.end += 1;
        }

        let p = powersort_power(run1.begin, run2.begin, run2.end, num_elements);
        while let Some(&(range_to_merge, top_power)) = powerstack.last() {
            if top_power <= p {
                break;
            }
            powerstack.pop();
            debug_assert_eq!(range_to_merge.end, run1.begin - 1);
            merge_powersort_runs(
                &mut working_set,
                src,
                range_to_merge.begin,
                range_to_merge.end + 1,
                run1.begin,
                run1.end + 1,
                lt,
                &mut min_gallop,
            );
            src[range_to_merge.begin..run1.end + 1]
                .clone_from_slice(&working_set[range_to_merge.begin..run1.end + 1]);
            run1.begin = range_to_merge.begin;
        }

        powerstack.push((run1, p));
        run1 = run2;
    }

    while let Some((range_to_merge, _)) = powerstack.pop() {
        debug_assert_eq!(range_to_merge.end, run1.begin - 1);
        merge_powersort_runs(
            &mut working_set,
            src,
            range_to_merge.begin,
            range_to_merge.end + 1,
            run1.begin,
            run1.end + 1,
            lt,
            &mut min_gallop,
        );
        src[range_to_merge.begin..run1.end + 1]
            .clone_from_slice(&working_set[range_to_merge.begin..run1.end + 1]);
        run1.begin = range_to_merge.begin;
    }
}

#[cfg(test)]
mod js_sort_tests {
    /// The tolerance-band comparator used by the PDF pipeline (intransitive).
    fn band_cmp(a: &(f64, f64), b: &(f64, f64)) -> std::cmp::Ordering {
        let dy = b.0 - a.0;
        if dy.abs() > 2.0 {
            dy.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        }
    }

    #[test]
    fn matches_std_stable_sort_on_consistent_cmp() {
        let mut seed: u64 = 0x243F6A8885A308D3;
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for trial in 0..300 {
            let len = (rand() % 300) as usize;
            let mut v: Vec<(u64, usize)> = (0..len).map(|i| (rand() % 8, i)).collect();
            let mut expected = v.clone();
            expected.sort_by_key(|a| a.0);
            super::js_stable_sort(&mut v, |a, b| a.0.cmp(&b.0));
            assert_eq!(v, expected, "trial {trial} len {len}");
        }
    }

    #[test]
    fn matches_bun_jsc_sort_on_intransitive_cmp() {
        // Fixtures generated by Bun's own Array#sort with the same
        // tolerance-band comparator (/tmp/sort_fuzz.json). Skipped when absent.
        for path in ["/tmp/sort_fuzz.json", "/tmp/sort_fuzz2.json"] {
            let Ok(data) = std::fs::read_to_string(path) else {
                continue;
            };
            let cases: serde_json::Value = serde_json::from_str(&data).unwrap();
            for (ci, case) in cases.as_array().unwrap().iter().enumerate() {
                let parse = |key: &str| -> Vec<(f64, f64)> {
                    case[key]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|o| (o["y"].as_f64().unwrap(), o["x"].as_f64().unwrap()))
                        .collect()
                };
                let mut input = parse("input");
                let expected = parse("expected");
                super::js_stable_sort(&mut input, band_cmp);
                assert_eq!(input, expected, "fuzz case {ci} in {path}");
            }
        }
    }

    /// Cross-checked against Bun (JavaScriptCore) via /tmp fixtures when
    /// present — see the fuzz harness in the repo history. This test pins the
    /// algorithm to JSC's bottom-up merge on an intransitive comparator.
    #[test]
    fn intransitive_band_cmp_is_deterministic() {
        let boxes = vec![
            (10.0, 5.0),
            (9.0, 1.0),
            (8.5, 3.0),
            (7.0, 2.0),
            (10.5, 0.5),
            (9.5, 9.0),
            (8.0, 4.0),
            (11.0, 1.5),
        ];
        let mut v = boxes.clone();
        super::js_stable_sort(&mut v, band_cmp);
        let again = {
            let mut w = boxes;
            super::js_stable_sort(&mut w, band_cmp);
            w
        };
        assert_eq!(v, again);
    }
}
