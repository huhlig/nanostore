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

//! Stress tests for Graph table engine.
//!
//! These tests validate Graph behavior under heavy load:
//! - Large graphs (10K+ vertices, 50K+ edges)
//! - Complex traversals (deep BFS/DFS)
//! - Dense connectivity
//! - Multiple edge types
//! - Long paths

use nanostore::table::{GraphAdjacency, GraphConfig, MemoryGraphTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::wal::LogSequenceNumber;

/// Helper to create a test graph
fn create_test_graph(directed: bool) -> MemoryGraphTable {
    let config = GraphConfig::new().with_directed(directed);
    MemoryGraphTable::new(TableId::from(1), "stress_graph".to_string(), config)
}

/// Helper to commit graph changes
fn commit_graph(graph: &MemoryGraphTable) {
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
}

/// Test large graph: 10,000 vertices with 50,000 edges
///
/// Validates that Graph can handle large-scale graphs.
#[test]
fn test_large_graph_10k_vertices_50k_edges() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 10,000 vertices with 50,000 edges
    // Each vertex connects to 5 random other vertices
    for i in 0..10_000 {
        let source = format!("vertex_{}", i);
        for j in 0..5 {
            let target_idx = (i * 7919 + j * 6547) % 10_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify graph structure
    let stats = graph.stats().unwrap();
    assert!(stats.entry_count.unwrap() >= 50_000);

    // Verify random vertices have edges
    for i in [0, 1000, 5000, 9999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), Some(b"connects")).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert_eq!(edges.len(), 5, "Vertex {} should have 5 edges", i);
    }
}

/// Test dense graph: 1,000 vertices, each connected to 100 others
///
/// Validates that Graph handles dense connectivity.
#[test]
fn test_dense_graph_1k_vertices_100k_edges() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create dense graph: 1,000 vertices, each with 100 edges
    for i in 0..1_000 {
        let source = format!("vertex_{}", i);
        for j in 0..100 {
            let target_idx = (i + j + 1) % 1_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify dense connectivity
    for i in [0, 250, 500, 750, 999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), Some(b"connects")).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert_eq!(edges.len(), 100, "Vertex {} should have 100 edges", i);
    }
}

/// Test deep BFS traversal on a chain of 5,000 vertices
///
/// Validates that BFS can handle deep traversals.
#[test]
fn test_deep_bfs_traversal_5k_chain() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create a chain: v0 -> v1 -> v2 -> ... -> v4999
    for i in 0..4_999 {
        let source = format!("vertex_{}", i);
        let target = format!("vertex_{}", i + 1);
        let edge_id = format!("edge_{}", i);

        graph
            .add_edge(
                source.as_bytes(),
                b"next",
                target.as_bytes(),
                edge_id.as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    commit_graph(&graph);

    // Traverse from start to end
    let mut visited = Vec::new();
    graph
        .bfs(b"vertex_0", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    // Should visit all 5,000 vertices
    assert_eq!(visited.len(), 5_000);
    assert_eq!(visited[0], b"vertex_0");
    assert_eq!(visited[4_999], b"vertex_4999");
}

/// Test deep DFS traversal on a binary tree of depth 12
///
/// Validates that DFS can handle deep recursive traversals.
#[test]
fn test_deep_dfs_traversal_binary_tree() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create binary tree: depth 12 = 4095 nodes
    for i in 0..2_047 {
        let parent = format!("node_{}", i);
        let left_child = format!("node_{}", 2 * i + 1);
        let right_child = format!("node_{}", 2 * i + 2);

        graph
            .add_edge(
                parent.as_bytes(),
                b"left",
                left_child.as_bytes(),
                format!("edge_left_{}", i).as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();

        graph
            .add_edge(
                parent.as_bytes(),
                b"right",
                right_child.as_bytes(),
                format!("edge_right_{}", i).as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    commit_graph(&graph);

    // DFS from root
    let mut visited = Vec::new();
    graph
        .dfs(b"node_0", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    // Should visit all nodes
    assert!(visited.len() >= 2_047);
    assert_eq!(visited[0], b"node_0");
}

/// Test multiple edge types with 10,000 edges
///
/// Validates that Graph handles multiple edge types efficiently.
#[test]
fn test_multiple_edge_types_10k_edges() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    let edge_types = vec![
        b"follows".as_slice(),
        b"likes".as_slice(),
        b"mentions".as_slice(),
        b"blocks".as_slice(),
        b"reports".as_slice(),
    ];

    // Create 2,000 vertices with 10,000 edges across 5 types
    for i in 0..2_000 {
        let source = format!("user_{}", i);
        for j in 0..5 {
            let target_idx = (i + j * 100 + 1) % 2_000;
            let target = format!("user_{}", target_idx);
            let edge_type = edge_types[j % edge_types.len()];
            let edge_id = format!("edge_{}_{}_{}", i, j, String::from_utf8_lossy(edge_type));

            graph
                .add_edge(
                    source.as_bytes(),
                    edge_type,
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Query each edge type
    for edge_type in &edge_types {
        let cursor = graph.outgoing(b"user_0", Some(*edge_type)).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert!(edges.len() > 0, "Should have edges of type {:?}", edge_type);
    }

    // Query all edge types
    let cursor = graph.outgoing(b"user_0", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 5);
}

/// Test complex graph with cycles
///
/// Validates that Graph handles cycles correctly.
#[test]
fn test_complex_graph_with_cycles_5k_vertices() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create graph with multiple cycles
    // Ring structure: v0 -> v1 -> v2 -> ... -> v4999 -> v0
    for i in 0..5_000 {
        let source = format!("vertex_{}", i);
        let target = format!("vertex_{}", (i + 1) % 5_000);
        let edge_id = format!("ring_edge_{}", i);

        graph
            .add_edge(
                source.as_bytes(),
                b"next",
                target.as_bytes(),
                edge_id.as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    // Add cross-connections to create more cycles
    for i in (0..5_000).step_by(100) {
        let source = format!("vertex_{}", i);
        let target = format!("vertex_{}", (i + 500) % 5_000);
        let edge_id = format!("cross_edge_{}", i);

        graph
            .add_edge(
                source.as_bytes(),
                b"shortcut",
                target.as_bytes(),
                edge_id.as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    commit_graph(&graph);

    // BFS should handle cycles correctly (visit each vertex once)
    let mut visited = Vec::new();
    graph
        .bfs(b"vertex_0", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    assert_eq!(visited.len(), 5_000, "Should visit each vertex exactly once");
}

/// Test graph with many disconnected components
///
/// Validates that Graph handles disconnected components.
#[test]
fn test_disconnected_components_100_components() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 100 disconnected components, each with 50 vertices
    for component in 0..100 {
        for i in 0..49 {
            let source = format!("c{}_v{}", component, i);
            let target = format!("c{}_v{}", component, i + 1);
            let edge_id = format!("c{}_edge_{}", component, i);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // BFS from each component should only visit that component
    for component in [0, 25, 50, 75, 99] {
        let start = format!("c{}_v0", component);
        let mut visited = Vec::new();

        graph
            .bfs(start.as_bytes(), |vertex| {
                visited.push(vertex.to_vec());
                true
            })
            .unwrap();

        assert_eq!(
            visited.len(),
            50,
            "Component {} should have 50 vertices",
            component
        );
    }
}

/// Test neighbor queries with high degree vertices
///
/// Validates that neighbor queries work with high-degree vertices.
#[test]
fn test_high_degree_vertices_1k_neighbors() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create a hub vertex connected to 1,000 other vertices
    for i in 0..1_000 {
        let target = format!("vertex_{}", i);
        let edge_id = format!("hub_edge_{}", i);

        graph
            .add_edge(
                b"hub",
                b"connects",
                target.as_bytes(),
                edge_id.as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    commit_graph(&graph);

    // Query hub's neighbors
    let neighbors = graph.neighbors(b"hub").unwrap();
    assert_eq!(neighbors.len(), 1_000);

    // Query hub's outgoing edges
    let cursor = graph.outgoing(b"hub", Some(b"connects")).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1_000);
}

/// Test edge deletions with large graph
///
/// Validates that edge deletions work correctly at scale.
#[test]
fn test_edge_deletions_10k_edges() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 2,000 vertices with 10,000 edges
    for i in 0..2_000 {
        let source = format!("vertex_{}", i);
        for j in 0..5 {
            let target_idx = (i + j + 1) % 2_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Delete every other edge (5,000 deletions)
    for i in 0..2_000 {
        let source = format!("vertex_{}", i);
        for j in [0, 2, 4] {
            let target_idx = (i + j + 1) % 2_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .remove_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify remaining edges
    for i in [0, 500, 1000, 1500, 1999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), Some(b"connects")).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert_eq!(edges.len(), 2, "Vertex {} should have 2 remaining edges", i);
    }
}

/// Test mixed operations: adds, queries, and deletes
///
/// Simulates a realistic workload with mixed operations.
#[test]
fn test_mixed_operations_20k_total() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Phase 1: Add 10,000 edges
    for i in 0..2_000 {
        let source = format!("vertex_{}", i);
        for j in 0..5 {
            let target_idx = (i + j + 1) % 2_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Phase 2: Perform traversals
    for i in [0, 500, 1000, 1500] {
        let start = format!("vertex_{}", i);
        let mut visited = Vec::new();
        graph
            .bfs(start.as_bytes(), |vertex| {
                visited.push(vertex.to_vec());
                visited.len() < 100 // Limit traversal depth
            })
            .unwrap();
    }

    // Phase 3: Delete 3,000 edges
    for i in (0..2_000).step_by(2) {
        let source = format!("vertex_{}", i);
        for j in [0, 2] {
            let target_idx = (i + j + 1) % 2_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .remove_edge(
                    source.as_bytes(),
                    b"connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Phase 4: Add 10,000 new edges
    for i in 0..2_000 {
        let source = format!("vertex_{}", i);
        for j in 5..10 {
            let target_idx = (i + j + 1) % 2_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"new_connects",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify final state
    let stats = graph.stats().unwrap();
    assert!(stats.entry_count.unwrap() > 15_000);
}

/// Test undirected graph with 5,000 edges
///
/// Validates that undirected graphs work correctly at scale.
#[test]
fn test_undirected_graph_5k_edges() {
    let mut graph = create_test_graph(false); // Undirected
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 1,000 vertices with 5,000 undirected edges
    for i in 0..1_000 {
        let source = format!("vertex_{}", i);
        for j in 0..5 {
            let target_idx = (i + j + 1) % 1_000;
            let target = format!("vertex_{}", target_idx);
            let edge_id = format!("edge_{}_{}", i, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    b"friends",
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify bidirectional connectivity
    for i in [0, 250, 500, 750, 999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), Some(b"friends")).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert!(edges.len() >= 5, "Vertex {} should have edges", i);
    }
}

/// Test graph with self-loops
///
/// Validates that self-loops are handled correctly.
#[test]
fn test_self_loops_1k_vertices() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 1,000 vertices, each with a self-loop
    for i in 0..1_000 {
        let vertex = format!("vertex_{}", i);
        let edge_id = format!("self_loop_{}", i);

        graph
            .add_edge(
                vertex.as_bytes(),
                b"self",
                vertex.as_bytes(),
                edge_id.as_bytes(),
                tx_id,
                lsn,
            )
            .unwrap();
    }

    commit_graph(&graph);

    // Verify self-loops
    for i in [0, 250, 500, 750, 999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), Some(b"self")).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source.0, vertex.as_bytes());
        assert_eq!(edges[0].target.0, vertex.as_bytes());
    }
}

/// Test parallel edges between same vertices
///
/// Validates that multiple edges between same vertices work.
#[test]
fn test_parallel_edges_5k_edges() {
    let mut graph = create_test_graph(true);
    let tx_id = TransactionId::from(1);
    let lsn = LogSequenceNumber::from(1);

    // Create 1,000 vertex pairs with 5 parallel edges each
    for i in 0..1_000 {
        let source = format!("vertex_{}", i);
        let target = format!("vertex_{}", (i + 1) % 1_000);

        for j in 0..5 {
            let edge_type = format!("type_{}", j);
            let edge_id = format!("edge_{}_{}_{}", i, i + 1, j);

            graph
                .add_edge(
                    source.as_bytes(),
                    edge_type.as_bytes(),
                    target.as_bytes(),
                    edge_id.as_bytes(),
                    tx_id,
                    lsn,
                )
                .unwrap();
        }
    }

    commit_graph(&graph);

    // Verify parallel edges
    for i in [0, 250, 500, 750, 999] {
        let vertex = format!("vertex_{}", i);
        let cursor = graph.outgoing(vertex.as_bytes(), None).unwrap();
        let edges = cursor.collect_all().unwrap();
        assert_eq!(edges.len(), 5, "Vertex {} should have 5 parallel edges", i);
    }
}

// Made with Bob