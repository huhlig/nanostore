//
// Copyright 2025-2026 Hans W. Uhlig. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

//! R-Tree configuration and parameters.

/// R-Tree configuration parameters.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SpatialConfig {
    /// Number of spatial dimensions (2 or 3)
    pub dimensions: usize,

    /// Splitting strategy to use when nodes overflow
    pub split_strategy: SplitStrategy,

    /// Maximum number of entries per node (M)
    /// Typical values: 50-200 for disk-based trees
    pub max_entries_per_node: usize,

    /// Minimum number of entries per node (m)
    /// Typically M/2 or 40% of M for better space utilization
    pub min_entries_per_node: usize,

    /// Reinsert percentage for R*-tree (typically 30%)
    /// Only used when split_strategy is RStar
    pub reinsert_percentage: f64,
}

impl Default for SpatialConfig {
    fn default() -> Self {
        Self {
            dimensions: 2,
            split_strategy: SplitStrategy::RStar,
            max_entries_per_node: 100,
            min_entries_per_node: 40,
            reinsert_percentage: 0.3,
        }
    }
}

impl SpatialConfig {
    /// Create a new spatial configuration.
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            ..Default::default()
        }
    }

    /// Set the split strategy.
    pub fn with_split_strategy(mut self, strategy: SplitStrategy) -> Self {
        self.split_strategy = strategy;
        self
    }

    /// Set the maximum entries per node.
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries_per_node = max_entries;
        // Maintain min as 40% of max
        self.min_entries_per_node = (max_entries * 2) / 5;
        self
    }

    /// Set the reinsert percentage for R*-tree.
    pub fn with_reinsert_percentage(mut self, percentage: f64) -> Self {
        self.reinsert_percentage = percentage.clamp(0.0, 1.0);
        self
    }

    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.dimensions < 2 || self.dimensions > 3 {
            return Err(format!(
                "Invalid dimensions: {}. Must be 2 or 3",
                self.dimensions
            ));
        }

        if self.max_entries_per_node < 4 {
            return Err(format!(
                "Invalid max_entries_per_node: {}. Must be at least 4",
                self.max_entries_per_node
            ));
        }

        if self.min_entries_per_node < 2 {
            return Err(format!(
                "Invalid min_entries_per_node: {}. Must be at least 2",
                self.min_entries_per_node
            ));
        }

        if self.min_entries_per_node >= self.max_entries_per_node {
            return Err(format!(
                "min_entries_per_node ({}) must be less than max_entries_per_node ({})",
                self.min_entries_per_node, self.max_entries_per_node
            ));
        }

        if self.reinsert_percentage < 0.0 || self.reinsert_percentage > 1.0 {
            return Err(format!(
                "Invalid reinsert_percentage: {}. Must be between 0.0 and 1.0",
                self.reinsert_percentage
            ));
        }

        Ok(())
    }
}

/// Node splitting strategy for R-Tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitStrategy {
    /// Linear split (O(n) complexity, fast but lower quality)
    /// Picks two entries that are farthest apart along one dimension
    Linear,

    /// Quadratic split (O(n²) complexity, better quality)
    /// Picks two entries that would waste the most area if grouped
    Quadratic,

    /// R*-tree split (best quality, includes forced reinsert)
    /// Uses sophisticated heuristics including:
    /// - Minimizing overlap between nodes
    /// - Minimizing perimeter (not just area)
    /// - Forced reinsertion to improve tree structure
    RStar,

    /// Hilbert split (O(n log n) with Hilbert curve ordering)
    /// Uses Hilbert space-filling curve for better spatial clustering.
    /// Particularly effective for skewed data distributions.
    Hilbert,
}

impl SplitStrategy {
    /// Get a human-readable description of the strategy.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Linear => "Linear split - fast O(n) but lower quality",
            Self::Quadratic => "Quadratic split - O(n²) with better quality",
            Self::RStar => "R*-tree split - best quality with forced reinsert",
            Self::Hilbert => "Hilbert split - space-filling curve for better clustering",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SpatialConfig::default();
        assert_eq!(config.dimensions, 2);
        assert_eq!(config.split_strategy, SplitStrategy::RStar);
        assert_eq!(config.max_entries_per_node, 100);
        assert_eq!(config.min_entries_per_node, 40);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_builder() {
        let config = SpatialConfig::new(3)
            .with_split_strategy(SplitStrategy::Quadratic)
            .with_max_entries(50)
            .with_reinsert_percentage(0.25);

        assert_eq!(config.dimensions, 3);
        assert_eq!(config.split_strategy, SplitStrategy::Quadratic);
        assert_eq!(config.max_entries_per_node, 50);
        assert_eq!(config.min_entries_per_node, 20);
        assert_eq!(config.reinsert_percentage, 0.25);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_dimensions() {
        let mut config = SpatialConfig::default();
        config.dimensions = 1;
        assert!(config.validate().is_err());

        config.dimensions = 4;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_invalid_entries() {
        let mut config = SpatialConfig::default();
        config.max_entries_per_node = 3;
        assert!(config.validate().is_err());

        config.max_entries_per_node = 10;
        config.min_entries_per_node = 10;
        assert!(config.validate().is_err());
    }
}

// Made with Bob
