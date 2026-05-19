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

//! GraphAdjacency table engine implementation.
//!
//! This module provides a specialized graph storage engine optimized for
//! graph traversal operations and adjacency queries.
//!
//! # Architecture
//!
//! ```text
//! Graph Table
//!     ↓
//! Hash/BTree Storage (base engine)
//!     ↓
//! Specialized Indices:
//!   - Outgoing edges: vertex → [edges]
//!   - Incoming edges: vertex → [edges]
//!   - Edge data: edge_id → edge_info
//! ```
//!
//! # Features
//!
//! - **Directed/Undirected graphs**: Configurable graph directionality
//! - **Weighted/Unweighted edges**: Optional edge weights
//! - **Fast neighbor queries**: O(1) lookup of adjacent vertices
//! - **Graph traversal**: Built-in BFS and DFS algorithms
//! - **Flexible storage**: Uses Hash or BTree as base storage
//! - **In-memory indexing**: Optional fast in-memory index
//!
//! # Use Cases
//!
//! - Social networks (friend relationships)
//! - Knowledge graphs (entity relationships)
//! - Dependency graphs (package dependencies)
//! - Road networks (navigation)
//! - Recommendation systems (user-item relationships)
//!
//! # Example
//!
//! ```rust,ignore
//! use nanostore::table::graph::{MemoryGraphTable, GraphConfig};
//! use nanostore::table::GraphAdjacency;
//! use nanostore::types::TableId;
//!
//! // Create a directed graph
//! let config = GraphConfig::new().with_directed(true);
//! let mut graph = MemoryGraphTable::new(
//!     TableId::from(1),
//!     "social_network".to_string(),
//!     config,
//! );
//!
//! // Add edges (relationships)
//! graph.add_edge(b"alice", b"follows", b"bob", b"edge1")?;
//! graph.add_edge(b"bob", b"follows", b"charlie", b"edge2")?;
//! graph.add_edge(b"alice", b"follows", b"charlie", b"edge3")?;
//!
//! // Query outgoing edges (who does Alice follow?)
//! let cursor = graph.outgoing(b"alice", Some(b"follows"))?;
//! for edge in cursor {
//!     println!("Alice follows: {:?}", edge.target);
//! }
//!
//! // Query incoming edges (who follows Bob?)
//! let cursor = graph.incoming(b"bob", Some(b"follows"))?;
//! for edge in cursor {
//!     println!("{:?} follows Bob", edge.source);
//! }
//!
//! // Traverse the graph with BFS
//! graph.bfs(b"alice", |vertex| {
//!     println!("Visiting: {:?}", vertex);
//!     true // continue traversal
//! })?;
//! ```

mod config;
mod edge;
mod memory;

pub use self::config::{GraphConfig, GraphStorageBackend};
pub use self::edge::{AdjacencyList, Edge, EdgeData, GraphKey};
pub use self::memory::{MemoryEdgeCursor, MemoryGraphTable};

// Made with Bob
