//! Part sorting utilities for correct ATTACH order during restore.
//!
//! Parts must be attached in order by (partition, min_block) to ensure
//! correct merge behavior, especially for Replacing/Collapsing engines
//! where the order of blocks determines which row version wins.

use crate::manifest::PartInfo;

/// Sort key extracted from a part name.
///
/// Part names follow the format `{partition}_{min}_{max}_{level}`.
/// The partition portion may contain underscores, so we parse from the right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSortKey {
    /// Partition identifier (e.g. "202401", "all").
    pub partition: String,
    /// Minimum block number.
    pub min_block: u64,
}

impl PartSortKey {
    /// Parse a part name into a sort key.
    ///
    /// ClickHouse part names have two formats:
    /// - 4-component: `{partition}_{min_block}_{max_block}_{level}`
    /// - 5-component: `{partition}_{min_block}_{max_block}_{level}_{data_version}`
    ///
    /// The 5-component format appears after mutations or restores increment
    /// the data version. We try 5-component first (all 4 trailing segments
    /// must be numeric), then fall back to 4-component.
    pub fn from_part_name(name: &str) -> Option<Self> {
        // Try 5-component: {partition}_{min}_{max}_{level}_{data_version}
        let seg5: Vec<&str> = name.rsplitn(5, '_').collect();
        if seg5.len() == 5 {
            if let (Ok(_), Ok(_), Ok(_), Ok(min_block)) = (
                seg5[0].parse::<u64>(),
                seg5[1].parse::<u64>(),
                seg5[2].parse::<u64>(),
                seg5[3].parse::<u64>(),
            ) {
                return Some(PartSortKey {
                    partition: seg5[4].to_string(),
                    min_block,
                });
            }
        }

        // Fallback: 4-component {partition}_{min}_{max}_{level}
        let seg4: Vec<&str> = name.rsplitn(4, '_').collect();
        if seg4.len() < 4 {
            return None;
        }
        let min_block = seg4[2].parse::<u64>().ok()?;

        Some(PartSortKey {
            partition: seg4[3].to_string(),
            min_block,
        })
    }
}

impl PartialOrd for PartSortKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PartSortKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partition
            .cmp(&other.partition)
            .then(self.min_block.cmp(&other.min_block))
    }
}

/// Sort parts by (partition, min_block) for correct ATTACH order.
///
/// Returns a new sorted vector without modifying the input.
pub fn sort_parts_by_min_block(parts: &[PartInfo]) -> Vec<PartInfo> {
    let mut sorted = parts.to_vec();
    sorted.sort_by(|a, b| {
        let key_a = PartSortKey::from_part_name(&a.name);
        let key_b = PartSortKey::from_part_name(&b.name);
        match (key_a, key_b) {
            (Some(ka), Some(kb)) => ka.cmp(&kb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }
    });
    sorted
}

/// Check if an engine requires sequential sorted attachment.
///
/// Engines containing "Replacing", "Collapsing", or "Versioned" need parts
/// attached in the correct order so that the merge semantics work properly.
pub fn needs_sequential_attach(engine: &str) -> bool {
    engine.contains("Replacing") || engine.contains("Collapsing") || engine.contains("Versioned")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_part_name_sort_key() {
        let key = PartSortKey::from_part_name("202401_1_50_3").unwrap();
        assert_eq!(key.partition, "202401");
        assert_eq!(key.min_block, 1);
    }

    #[test]
    fn test_parse_part_name_all_partition() {
        let key = PartSortKey::from_part_name("all_0_0_0").unwrap();
        assert_eq!(key.partition, "all");
        assert_eq!(key.min_block, 0);
    }

    #[test]
    fn test_parse_part_name_5_component() {
        // 5-component format: {partition}_{min}_{max}_{level}_{data_version}
        let key = PartSortKey::from_part_name("202401_0_0_0_2").unwrap();
        assert_eq!(key.partition, "202401");
        assert_eq!(key.min_block, 0);

        let key = PartSortKey::from_part_name("202401_1_1_0_8").unwrap();
        assert_eq!(key.partition, "202401");
        assert_eq!(key.min_block, 1);

        let key = PartSortKey::from_part_name("202403_5_5_0_7").unwrap();
        assert_eq!(key.partition, "202403");
        assert_eq!(key.min_block, 5);
    }

    #[test]
    fn test_parse_part_name_5_component_all_partition() {
        // 5-component with "all" partition
        let key = PartSortKey::from_part_name("all_1_1_0_3").unwrap();
        assert_eq!(key.partition, "all");
        assert_eq!(key.min_block, 1);
    }

    #[test]
    fn test_parse_part_name_invalid() {
        // Too few segments
        assert!(PartSortKey::from_part_name("invalid").is_none());
        assert!(PartSortKey::from_part_name("a_b").is_none());
        assert!(PartSortKey::from_part_name("a_b_c").is_none());
    }

    #[test]
    fn test_parse_part_name_non_numeric_block() {
        assert!(PartSortKey::from_part_name("202401_abc_50_3").is_none());
    }

    #[test]
    fn test_sort_parts_by_min_block() {
        let parts = vec![
            make_part("202402_1_1_0"),
            make_part("202401_50_100_1"),
            make_part("202401_1_50_3"),
            make_part("202402_2_5_0"),
        ];

        let sorted = sort_parts_by_min_block(&parts);

        assert_eq!(sorted[0].name, "202401_1_50_3");
        assert_eq!(sorted[1].name, "202401_50_100_1");
        assert_eq!(sorted[2].name, "202402_1_1_0");
        assert_eq!(sorted[3].name, "202402_2_5_0");
    }

    #[test]
    fn test_sort_parts_same_partition() {
        let parts = vec![
            make_part("202401_100_200_2"),
            make_part("202401_1_50_3"),
            make_part("202401_50_100_1"),
        ];

        let sorted = sort_parts_by_min_block(&parts);

        assert_eq!(sorted[0].name, "202401_1_50_3");
        assert_eq!(sorted[1].name, "202401_50_100_1");
        assert_eq!(sorted[2].name, "202401_100_200_2");
    }

    #[test]
    fn test_sort_parts_mixed_4_and_5_component() {
        let parts = vec![
            make_part("202402_1_1_0_3"),
            make_part("202401_0_0_0"),
            make_part("202401_1_1_0_8"),
            make_part("202402_2_5_0"),
        ];

        let sorted = sort_parts_by_min_block(&parts);

        assert_eq!(sorted[0].name, "202401_0_0_0");
        assert_eq!(sorted[1].name, "202401_1_1_0_8");
        assert_eq!(sorted[2].name, "202402_1_1_0_3");
        assert_eq!(sorted[3].name, "202402_2_5_0");
    }

    #[test]
    fn test_needs_sequential_attach() {
        assert!(needs_sequential_attach("ReplacingMergeTree"));
        assert!(needs_sequential_attach("CollapsingMergeTree"));
        assert!(needs_sequential_attach("VersionedCollapsingMergeTree"));
        assert!(needs_sequential_attach("ReplicatedReplacingMergeTree"));
        assert!(needs_sequential_attach(
            "ReplicatedVersionedCollapsingMergeTree"
        ));
        assert!(!needs_sequential_attach("MergeTree"));
        assert!(!needs_sequential_attach("SummingMergeTree"));
        assert!(!needs_sequential_attach("AggregatingMergeTree"));
        assert!(!needs_sequential_attach("ReplicatedMergeTree"));
    }

    fn make_part(name: &str) -> PartInfo {
        PartInfo::new(name, 0, 0)
    }
}
