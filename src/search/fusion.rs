//! Reciprocal rank fusion: merges several ranked lists of passage ids into one.

use std::collections::HashMap;

/// Fuses ranked lists of ids, each with a weight. An id's score is the sum
/// over the lists that hold it of `weight / (k + rank)`, with rank counting
/// from 1. Returns every id once, best first and lowest id first among equals.
pub fn rrf(lists: &[(&[u32], f32)], k: f32) -> Vec<(u32, f32)> {
    let mut scores: HashMap<u32, f32> = HashMap::new();
    for &(ids, weight) in lists {
        for (rank, &id) in (1u32..).zip(ids) {
            *scores.entry(id).or_default() += weight / (k + rank as f32);
        }
    }
    let mut fused: Vec<(u32, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_matches_hand_computed_scores() {
        let fused = rrf(&[(&[10, 20, 30], 1.25), (&[20, 40], 1.0)], 60.0);
        let ids: Vec<u32> = fused.iter().map(|f| f.0).collect();
        assert_eq!(ids, [20, 10, 30, 40]);
        assert!((fused[0].1 - (1.25 / 62.0 + 1.0 / 61.0)).abs() < 1e-6);
        assert!((fused[1].1 - 1.25 / 61.0).abs() < 1e-6);
        assert!((fused[2].1 - 1.25 / 63.0).abs() < 1e-6);
        assert!((fused[3].1 - 1.0 / 62.0).abs() < 1e-6);
    }

    #[test]
    fn rrf_ties_break_by_id() {
        let fused = rrf(&[(&[2], 1.0), (&[1], 1.0)], 60.0);
        let ids: Vec<u32> = fused.iter().map(|f| f.0).collect();
        assert_eq!(ids, [1, 2]);
        assert_eq!(fused[0].1, fused[1].1);
    }

    #[test]
    fn rrf_of_no_lists_or_empty_lists_is_empty() {
        assert!(rrf(&[], 60.0).is_empty());
        assert!(rrf(&[(&[], 1.0), (&[], 1.25)], 60.0).is_empty());
    }

    #[test]
    fn rrf_of_one_list_keeps_its_order() {
        let fused = rrf(&[(&[7, 3, 9], 1.0)], 60.0);
        let ids: Vec<u32> = fused.iter().map(|f| f.0).collect();
        assert_eq!(ids, [7, 3, 9]);
    }

    #[test]
    fn a_heavier_list_outranks_a_lighter_one_at_the_same_rank() {
        let fused = rrf(&[(&[1], 1.0), (&[2], 1.25)], 60.0);
        let ids: Vec<u32> = fused.iter().map(|f| f.0).collect();
        assert_eq!(ids, [2, 1]);
    }
}
